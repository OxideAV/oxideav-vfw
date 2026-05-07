//! Top-level [`Sandbox`] — owns the MMU, the CPU, the Win32 stub
//! registry, and the per-emulator host state, and exposes the
//! "load this DLL and call its DllMain" workflow that the
//! integration tests + future codec wrapper layers drive.
//!
//! This is the highest-level public entry point in the crate.
//! Round-1 only exposes [`Sandbox::load`] + [`Sandbox::call_dll_main`];
//! round-2 will add the VfW `DriverProc` invocation pipeline on
//! top.

use crate::emulator::{
    isa_int::{StepOk, RET_SENTINEL},
    mmu::Perm,
    regs::Reg32,
    Cpu, Mmu,
};
use crate::pe::{Image, Loader};
use crate::win32::{dispatch_stub, HostState, Registry};

/// `DllMain` reason code: process is loading the DLL.
pub const DLL_PROCESS_ATTACH: u32 = 1;
/// `DllMain` reason code: process is unloading the DLL.
pub const DLL_PROCESS_DETACH: u32 = 0;

/// Default region the loader can use as the kernel32 heap arena.
const HEAP_ARENA_START: u32 = 0x6000_0000;
const HEAP_ARENA_END: u32 = 0x7000_0000;

/// Default guest stack region — plenty of room above the heap.
const STACK_BOTTOM: u32 = 0x9000_0000;
const STACK_SIZE: u32 = 0x0010_0000; // 1 MiB
const STACK_TOP: u32 = STACK_BOTTOM + STACK_SIZE;

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
        // Stack (R+W)
        mmu.map(STACK_BOTTOM, STACK_SIZE, Perm::R | Perm::W);

        let mut cpu = Cpu::new();
        cpu.regs.set_esp(STACK_TOP - 0x100); // leave a guard at the top

        let mut registry = Registry::new();
        registry.register_kernel32();

        let host = HostState::new(HEAP_ARENA_START, HEAP_ARENA_END);
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
    pub fn call_dll_main(&mut self, image: &Image, reason: u32) -> Result<u32, crate::Error> {
        let h_module = image.image_base;
        let lpv_reserved = 0u32;
        // Push args right-to-left.
        self.cpu.push32(&mut self.mmu, lpv_reserved)?;
        self.cpu.push32(&mut self.mmu, reason)?;
        self.cpu.push32(&mut self.mmu, h_module)?;
        // Saved return address.
        self.cpu.push32(&mut self.mmu, RET_SENTINEL)?;
        self.cpu.regs.eip = image.entry_point;
        self.run_until_sentinel()?;
        Ok(self.cpu.regs.get32(Reg32::Eax))
    }

    /// Drive the CPU until `eip == RET_SENTINEL`, dispatching to
    /// Win32 stubs whenever `eip` lands on a registered thunk
    /// address.
    pub fn run_until_sentinel(&mut self) -> Result<(), crate::Error> {
        loop {
            if self.cpu.regs.eip == RET_SENTINEL {
                return Ok(());
            }
            // Did we land on a Win32 stub thunk? If so, dispatch
            // and continue.
            if self.registry.is_thunk(self.cpu.regs.eip) {
                dispatch_stub(&mut self.cpu, &mut self.mmu, &self.registry, &mut self.host)?;
                continue;
            }
            match self.cpu.step(&mut self.mmu)? {
                StepOk::Continued => continue,
                StepOk::Halted => return Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
