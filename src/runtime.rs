//! Top-level [`Sandbox`] — owns the MMU, the CPU, the Win32 stub
//! registry, and the per-emulator host state, and exposes the
//! "load this DLL and call its DllMain" workflow that the
//! integration tests + future codec wrapper layers drive.
//!
//! This is the highest-level public entry point in the crate.
//! Round-1 exposed [`Sandbox::load`] + [`Sandbox::call_dll_main`];
//! round-2 adds the generic [`Sandbox::call_export`] helper that
//! the `vfw32` host stubs use to invoke the codec's `DriverProc`
//! synchronously.

use crate::emulator::{mmu::Perm, Cpu, Mmu};
use crate::pe::{Image, Loader};
use crate::win32::{
    call_guest, run_until_sentinel as run_until_sentinel_free, vfw32, HostState, Registry,
    DATA_IMPORT_BASE,
};

/// `DllMain` reason code: process is loading the DLL.
pub const DLL_PROCESS_ATTACH: u32 = 1;
/// `DllMain` reason code: process is unloading the DLL.
pub const DLL_PROCESS_DETACH: u32 = 0;

/// Default region the loader can use as the kernel32 heap arena.
const HEAP_ARENA_START: u32 = 0x6000_0000;
const HEAP_ARENA_END: u32 = 0x7000_0000;

/// Const-arena region — read-only canned strings handed back from
/// `GetCommandLineA` / `GetEnvironmentStrings` etc.
const CONST_ARENA_START: u32 = 0x7000_0000;
const CONST_ARENA_END: u32 = 0x7010_0000;

/// Data-import slot region — see [`crate::win32::DATA_IMPORT_BASE`].
/// Holds 4-byte values backing CRT data imports like
/// `msvcrt!_adjust_fdiv`. 4 KiB is plenty.
const DATA_IMPORT_REGION_SIZE: u32 = 0x0000_1000;

/// Default guest stack region — plenty of room above the heap.
const STACK_BOTTOM: u32 = 0x9000_0000;
const STACK_SIZE: u32 = 0x0010_0000; // 1 MiB
const STACK_TOP: u32 = STACK_BOTTOM + STACK_SIZE;

/// Thread Environment Block — Windows places its TEB at
/// `0x7FFD_E000` historically. We map a 4 KiB page here and
/// stage the SEH chain head (`FS:[0]`) to `0xFFFF_FFFF` ("end of
/// chain"). Real Windows fills many more fields; for the codec
/// CRT init we only need a writable page so the codec's SEH
/// `__try` setup can save the prior chain head, write its own,
/// and restore on exit.
const TEB_BASE: u32 = 0x7FFD_E000;
const TEB_SIZE: u32 = 0x0000_1000; // 4 KiB
/// `EXCEPTION_REGISTRATION_RECORD*` initialiser at FS:[0].
const SEH_END_OF_CHAIN: u32 = 0xFFFF_FFFF;

/// One sandbox instance per loaded codec DLL.
pub struct Sandbox {
    pub mmu: Mmu,
    pub cpu: Cpu,
    pub registry: Registry,
    pub host: HostState,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Sandbox {
    /// Create a fresh sandbox with the heap arena and stack
    /// pre-mapped, the kernel32 stub set registered, and the
    /// CPU's `esp` pointing at a freshly-allocated stack.
    pub fn new() -> Self {
        let mut mmu = Mmu::new();
        // Heap arena (R+W)
        mmu.map(
            HEAP_ARENA_START,
            HEAP_ARENA_END - HEAP_ARENA_START,
            Perm::R | Perm::W,
        );
        // Const-arena for canned strings (R+W mapped; the caller
        // ABI treats it as R-only — we use write_initializer for
        // population, then any reads honour the perm bits).
        mmu.map(
            CONST_ARENA_START,
            CONST_ARENA_END - CONST_ARENA_START,
            Perm::R | Perm::W,
        );
        // Data-import slot region (R+W) — holds the 4-byte
        // values backing CRT data imports like
        // `msvcrt!_adjust_fdiv`. Seeded with each registered
        // import's `initial` value.
        mmu.map(DATA_IMPORT_BASE, DATA_IMPORT_REGION_SIZE, Perm::R | Perm::W);
        // Stack (R+W)
        mmu.map(STACK_BOTTOM, STACK_SIZE, Perm::R | Perm::W);
        // TEB / FS-segment data (R+W). Initialise FS:[0] = -1
        // (no SEH handler installed) and FS:[0x18] = TEB self
        // pointer per the Windows TEB ABI used by Win32 CRTs.
        mmu.map(TEB_BASE, TEB_SIZE, Perm::R | Perm::W);
        mmu.write_initializer(TEB_BASE, &SEH_END_OF_CHAIN.to_le_bytes())
            .expect("seed TEB FS:[0]");
        mmu.write_initializer(TEB_BASE + 0x18, &TEB_BASE.to_le_bytes())
            .expect("seed TEB FS:[0x18] (self pointer)");
        // FS:[0x30] would be the PEB pointer — we leave it 0
        // until a codec actually dereferences it.

        let mut cpu = Cpu::new();
        cpu.regs.set_esp(STACK_TOP - 0x100); // leave a guard at the top
        cpu.set_fs_base(TEB_BASE);

        let mut registry = Registry::new();
        registry.register_all();
        // Seed data-import slot values into the mapped region.
        for (_dll, _name, d) in registry.data_imports() {
            mmu.write_initializer(d.addr, &d.initial.to_le_bytes())
                .expect("seed data import");
        }

        let host = HostState::new(HEAP_ARENA_START, HEAP_ARENA_END)
            .with_const_arena(CONST_ARENA_START, CONST_ARENA_END);
        Sandbox {
            mmu,
            cpu,
            registry,
            host,
        }
    }

    /// Load a PE32 DLL from `bytes`, mapping it into the
    /// sandbox's MMU. The returned [`Image`] holds the entry
    /// point + export table.
    pub fn load(&mut self, name: &str, bytes: &[u8]) -> Result<Image, crate::Error> {
        let mut loader = Loader::new(&mut self.mmu, &mut self.registry, &mut self.host);
        let img = loader.load(name, bytes)?;
        // Record primary module base so `GetModuleHandleA(NULL)`
        // returns the right value.
        self.host.primary_module_base = img.image_base;
        Ok(img)
    }

    /// Synchronously call `DllMain(hModule, fdwReason, lpvReserved)`
    /// inside the emulator and return the dword `eax` value at
    /// the point the function returned to the synthetic
    /// `RET_SENTINEL`.
    ///
    /// The DllMain ABI is stdcall (callee-cleanup), so we push
    /// `lpvReserved` first, then `fdwReason`, then `hModule`,
    /// then the return-address sentinel. The callee's `RET 12`
    /// (or equivalent) cleans the args.
    ///
    /// Resolution: prefer the `DllMain` named export (Indeo
    /// codecs); fall back to the PE `AddressOfEntryPoint`
    /// (mpg4c32.dll and other CRT-startup-driven DLLs that
    /// don't export `DllMain` by name). Both expose the same
    /// stdcall (HINSTANCE, DWORD, LPVOID) ABI.
    pub fn call_dll_main(&mut self, image: &Image, reason: u32) -> Result<u32, crate::Error> {
        let h_module = image.image_base;
        let lpv_reserved = 0u32;
        let target = image.export("DllMain").unwrap_or(image.entry_point);
        if target == 0 {
            return Err(crate::Error::Win32(
                crate::win32::Win32Error::InvalidArgument {
                    stub: "call_dll_main",
                    reason: format!(
                        "no DllMain export and no PE entry point in {:?}",
                        image.name
                    ),
                },
            ));
        }
        call_guest(
            &mut self.cpu,
            &mut self.mmu,
            &self.registry,
            &mut self.host,
            target,
            &[h_module, reason, lpv_reserved],
        )
    }

    /// Generic stdcall guest-call helper. Resolves `name` against
    /// `image`'s export table, pushes `args` right-to-left + the
    /// `RET_SENTINEL`, and runs until the callee returns.
    /// Returns `eax`.
    ///
    /// Used both internally (by [`Self::call_dll_main`]) and by
    /// future codec adapter layers that need to drive arbitrary
    /// codec exports — `DriverProc`, `MyCodecGetVersion`,
    /// `MyCodecExtraInit`, etc. The round-2 `vfw32::ic_*` host
    /// surface uses [`crate::win32::call_guest`] directly with
    /// the codec's `DriverProc` VA.
    pub fn call_export(
        &mut self,
        image: &Image,
        name: &str,
        args: &[u32],
    ) -> Result<u32, crate::Error> {
        let target = image.export(name).ok_or_else(|| {
            crate::Error::Win32(crate::win32::Win32Error::InvalidArgument {
                stub: "call_export",
                reason: format!("export {name:?} not found in {:?}", image.name),
            })
        })?;
        call_guest(
            &mut self.cpu,
            &mut self.mmu,
            &self.registry,
            &mut self.host,
            target,
            args,
        )
    }

    /// Drive the CPU until `eip == RET_SENTINEL`, dispatching to
    /// Win32 stubs whenever `eip` lands on a registered thunk
    /// address. Thin wrapper over [`crate::win32::run_until_sentinel`]
    /// kept for API stability.
    pub fn run_until_sentinel(&mut self) -> Result<(), crate::Error> {
        run_until_sentinel_free(&mut self.cpu, &mut self.mmu, &self.registry, &mut self.host)
    }

    // ---- vfw32 IC* convenience wrappers ------------------------------

    /// Mark `image` as the codec the next [`Self::ic_open`] call
    /// should target.
    ///
    /// Round 2 supports a single codec image per sandbox — round 3
    /// will lift that into a multi-codec registry. The image must
    /// export `DriverProc`.
    pub fn install_codec(&mut self, image: &Image) -> Result<(), crate::Error> {
        let dp = image.export("DriverProc").ok_or_else(|| {
            crate::Error::Win32(crate::win32::Win32Error::InvalidArgument {
                stub: "install_codec",
                reason: format!("DriverProc not exported by {:?}", image.name),
            })
        })?;
        self.host.default_driver_proc = dp;
        Ok(())
    }

    /// Open the installed codec (`DRV_OPEN`).
    pub fn ic_open(
        &mut self,
        fcc_type: u32,
        fcc_handler: u32,
        mode: u32,
    ) -> Result<u32, crate::Error> {
        vfw32::ic_open(
            &mut self.cpu,
            &mut self.mmu,
            &self.registry,
            &mut self.host,
            fcc_type,
            fcc_handler,
            mode,
        )
    }

    /// Close a codec instance (`DRV_CLOSE`).
    pub fn ic_close(&mut self, hic: u32) -> Result<u32, crate::Error> {
        vfw32::ic_close(
            &mut self.cpu,
            &mut self.mmu,
            &self.registry,
            &mut self.host,
            hic,
        )
    }

    /// Read the codec's `ICINFO` block.
    pub fn ic_get_info(&mut self, hic: u32, cb: u32) -> Result<Vec<u8>, crate::Error> {
        vfw32::ic_get_info(
            &mut self.cpu,
            &mut self.mmu,
            &self.registry,
            &mut self.host,
            hic,
            cb,
        )
    }

    /// `ICDecompressQuery` — does the codec accept this format?
    pub fn ic_decompress_query(
        &mut self,
        hic: u32,
        input: &vfw32::Bih,
        output: Option<&vfw32::Bih>,
    ) -> Result<u32, crate::Error> {
        vfw32::ic_decompress_query(
            &mut self.cpu,
            &mut self.mmu,
            &self.registry,
            &mut self.host,
            hic,
            input,
            output,
        )
    }

    /// `ICDecompressBegin` — set up the decoder pipeline.
    pub fn ic_decompress_begin(
        &mut self,
        hic: u32,
        input: &vfw32::Bih,
        output: &vfw32::Bih,
    ) -> Result<u32, crate::Error> {
        vfw32::ic_decompress_begin(
            &mut self.cpu,
            &mut self.mmu,
            &self.registry,
            &mut self.host,
            hic,
            input,
            output,
        )
    }

    /// `ICDecompressEnd` — tear down the decoder pipeline.
    pub fn ic_decompress_end(&mut self, hic: u32) -> Result<u32, crate::Error> {
        vfw32::ic_decompress_end(
            &mut self.cpu,
            &mut self.mmu,
            &self.registry,
            &mut self.host,
            hic,
        )
    }

    // ---- Trace-mode programmatic API (gated on the `trace`
    // ---- Cargo feature). Documented in
    // ---- `docs/winmf/winmf-emulator.md` §"Trace mode".

    /// Install a memory watchpoint covering `[addr, addr+size)`.
    /// Any guest access whose address range intersects the
    /// watchpoint emits a `kind=mem_write` (or `mem_read`) JSONL
    /// event to the configured sink. Multiple watchpoints may
    /// overlap; each fires independently.
    #[cfg(feature = "trace")]
    pub fn watch(&mut self, addr: u32, size: u32, mode: crate::trace::WatchMode) {
        self.mmu.trace.watch(addr, size, mode);
    }

    /// Remove watchpoints whose `(addr, size)` exactly matches.
    /// Mode is ignored for the match.
    #[cfg(feature = "trace")]
    pub fn unwatch(&mut self, addr: u32, size: u32) {
        self.mmu.trace.unwatch(addr, size);
    }

    /// Toggle per-instruction execution trace at runtime. Has no
    /// effect unless the crate was built with the `trace-exec`
    /// sub-feature.
    #[cfg(feature = "trace")]
    pub fn set_exec_trace(&mut self, on: bool) {
        self.mmu.trace.exec_on = on;
    }

    /// Override the trace JSONL sink at runtime. Defaults to
    /// honouring `OXIDEAV_VFW_TRACE_FILE`.
    #[cfg(feature = "trace")]
    pub fn set_trace_sink(&mut self, sink: Box<dyn std::io::Write + Send>) {
        self.mmu.trace.set_sink(sink);
    }

    /// `ICDecompress` — decode one frame.
    #[allow(clippy::too_many_arguments)]
    pub fn ic_decompress(
        &mut self,
        hic: u32,
        flags: u32,
        input_bih: &vfw32::Bih,
        input_bytes: &[u8],
        output_bih: &vfw32::Bih,
        output_capacity: u32,
    ) -> Result<(u32, Vec<u8>), crate::Error> {
        vfw32::ic_decompress(
            &mut self.cpu,
            &mut self.mmu,
            &self.registry,
            &mut self.host,
            hic,
            flags,
            input_bih,
            input_bytes,
            output_bih,
            output_capacity,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::isa_int::RET_SENTINEL;
    use crate::emulator::regs::Reg32;
    use crate::pe::test_image::build_minimal_dll;

    #[test]
    fn load_synth_dll_and_run_dll_main_returns_to_sentinel() {
        let bytes = build_minimal_dll();
        let mut sb = Sandbox::new();
        let img = sb.load("synth.dll", &bytes).unwrap();
        // Pre-set eax = 1 so we can confirm the synth DllMain
        // returned without modifying it (it's just `ret 12`).
        sb.cpu.regs.set32(Reg32::Eax, 1);
        let ret = sb.call_dll_main(&img, DLL_PROCESS_ATTACH).unwrap();
        assert_eq!(ret, 1);
        assert_eq!(sb.cpu.regs.eip, RET_SENTINEL);
    }

    #[test]
    fn calling_through_iat_thunk_invokes_kernel32_stub() {
        // Emulator-only test: fabricate a code block that calls
        // a kernel32!GetProcessHeap thunk and rets. Verifies the
        // run loop's "is_thunk → dispatch" path.
        let mut sb = Sandbox::new();
        let thunk = sb
            .registry
            .resolve("kernel32.dll", "GetProcessHeap")
            .unwrap();
        // Map a code page at 0x1000.
        sb.mmu.map(0x1000, 0x1000, Perm::R | Perm::X);
        // call dword [thunk_slot]; ret 0
        // Easier: set eip directly to the thunk after pushing
        // the synthetic ret-sentinel.
        sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
        sb.cpu.regs.eip = thunk;
        sb.run_until_sentinel().unwrap();
        assert_eq!(sb.cpu.regs.get32(Reg32::Eax), 0xDEAD_BEEF);
    }
}
