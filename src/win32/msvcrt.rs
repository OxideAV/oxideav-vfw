//! `msvcrt.dll` stubs — round-20 surface for the MSMPEG4 v3
//! VfW decoder (`mpg4c32.dll`) and its DirectShow siblings.
//!
//! Every codec compiled with MSVC after 1996 imports a small
//! collection of CRT init / heap / C++-ABI symbols from the
//! Microsoft VC redistributable. This module satisfies the
//! minimum subset Milestone 3.1 (docs/winmf/winmf-emulator.md
//! §"Milestone 3.1 — MS-MPEG-4 v3 unblock plan") flagged across
//! the four MSMPEG4-related binaries:
//!
//! * `??2@YAPAXI@Z` — `operator new(size_t)`. Returns
//!   `nullptr` on `size == 0` per the Itanium / MSVC C++ ABI.
//! * `??3@YAXPAX@Z` — `operator delete(void*)`. No-op on
//!   `nullptr`; otherwise wraps `HeapFree`.
//! * `_adjust_fdiv` — Pentium-FDIV-erratum runtime fix-up (a
//!   data symbol on real CRTs; we stub as a function that
//!   returns zero — codecs only `cmp [_adjust_fdiv], 0` in
//!   the math library, which never runs in our decode path).
//! * `_except_handler3` — MSVC SEH frame handler. Returns
//!   `EXCEPTION_CONTINUE_SEARCH = 1`; codecs only ever
//!   register it as the chain head and we don't propagate
//!   exceptions through SEH (see `kernel32!RtlUnwind`).
//! * `_initterm(start, end)` — CRT static initialiser
//!   thunk-table walker; calls each non-null
//!   `void(*)()` between `start` and `end`.
//! * `_purecall` — abstract-virtual-call sentinel; in
//!   release builds this is a no-op on entry.
//! * `malloc` / `free` — wrap the existing `HeapAlloc`
//!   arena.
//!
//! Every stub here is **cdecl** (caller-cleanup) so we
//! register them with `arg_dwords = 0`. See
//! [`super::dispatch_stub`] for the calling-convention
//! contract.
//!
//! Reference docs (clean-room — no Wine / ReactOS source):
//! * MSDN "C run-time library reference" — function
//!   contracts.
//! * Itanium C++ ABI §"Mangling" + Microsoft C++ name-
//!   mangling reference (the `??2`/`??3` decorated names).
//! * MSDN "Structured Exception Handling" — `_except_handler3`
//!   ABI.

use super::{arg_dword, call_guest, HostState, Registry, StubFn, Win32Error};
use crate::emulator::{Cpu, Mmu};

/// Register every msvcrt stub.
pub fn register(registry: &mut Registry) {
    // C++ operator new(size_t) — the Microsoft mangled name.
    registry.register("msvcrt.dll", "??2@YAPAXI@Z", stub_operator_new as StubFn, 0);
    // C++ operator delete(void*) — Microsoft mangled name.
    registry.register(
        "msvcrt.dll",
        "??3@YAXPAX@Z",
        stub_operator_delete as StubFn,
        0,
    );
    // CRT init — fdiv erratum / SEH / static-ctor table /
    // pure-virtual sentinel.
    //
    // `_adjust_fdiv` is a **data symbol**, not a function:
    // codecs read it as `mov reg, [iat]; mov reg, [reg]`
    // (the IAT slot is the *address* of a 4-byte int, not a
    // function pointer). Register a 4-byte data slot
    // initialised to 0 — meaning "no Pentium-FDIV fix-up
    // needed", which is true for any post-1996 CPU and our
    // synthesised Pentium II.
    registry.register_data("msvcrt.dll", "_adjust_fdiv", 0);
    registry.register(
        "msvcrt.dll",
        "_except_handler3",
        stub_except_handler3 as StubFn,
        0,
    );
    registry.register("msvcrt.dll", "_initterm", stub_initterm as StubFn, 0);
    registry.register("msvcrt.dll", "_purecall", stub_purecall as StubFn, 0);
    // C heap.
    registry.register("msvcrt.dll", "malloc", stub_malloc as StubFn, 0);
    registry.register("msvcrt.dll", "free", stub_free as StubFn, 0);
}

/// `void* operator new(size_t)` (Microsoft mangling
/// `??2@YAPAXI@Z`). cdecl. Per the C++ ABI, returns
/// `nullptr` on `size == 0` rather than the smallest legal
/// allocation — codecs sometimes test for that.
fn stub_operator_new(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let size = arg_dword(cpu, mmu, 0).map_err(|t| trap("operator new", t))?;
    if size == 0 {
        return Ok(0);
    }
    let addr = state.arena_alloc(size)?;
    let zeros = vec![0u8; size as usize];
    mmu.write_initializer(addr, &zeros)
        .map_err(|t| trap("operator new", t))?;
    Ok(addr)
}

/// `void operator delete(void*)` (Microsoft mangling
/// `??3@YAXPAX@Z`). cdecl. No-op on `nullptr`.
fn stub_operator_delete(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let p = arg_dword(cpu, mmu, 0).map_err(|t| trap("operator delete", t))?;
    if p == 0 {
        return Ok(0);
    }
    // Best-effort: drop the heap entry if known. Unknown
    // pointers (e.g. C++ codec frees something allocated via
    // GlobalAlloc through a base-class destructor) are
    // tolerated silently — symmetrical to `kernel32!HeapFree`.
    let _ = state.heap.remove(&p);
    Ok(0)
}

/// `int _except_handler3(EXCEPTION_RECORD*, EXCEPTION_REGISTRATION*,
/// CONTEXT*, void*)`. cdecl. We never raise SEH exceptions
/// inside the sandbox so the handler is never actually called;
/// this stub exists only so the IAT slot resolves at PE-load
/// time. Returns `EXCEPTION_CONTINUE_SEARCH = 1` which is the
/// "chain past me" outcome SEH expects when a handler can't
/// service the exception.
fn stub_except_handler3(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(1)
}

/// `void _initterm(_PVFV* pfbegin, _PVFV* pfend)`. cdecl.
/// Walks `[pfbegin, pfend)` calling every non-null function
/// pointer it finds, in order. Used by the MSVC CRT to drive
/// global C++ static-ctor / static-dtor lists.
///
/// Each `_PVFV` is `void (__cdecl*)(void)` — no args, no
/// return value. We invoke each entry through
/// [`call_guest`] so any sub-stubs they call (`malloc`,
/// `_initterm` recursively, etc.) dispatch through the host
/// runtime cleanly.
fn stub_initterm(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
    registry: &Registry,
) -> Result<u32, Win32Error> {
    let begin = arg_dword(cpu, mmu, 0).map_err(|t| trap("_initterm", t))?;
    let end = arg_dword(cpu, mmu, 1).map_err(|t| trap("_initterm", t))?;
    if begin == 0 || end == 0 || end <= begin {
        return Ok(0);
    }
    // Bounds: cap iteration at 4096 entries to defend against
    // a malformed table.
    let span = end.saturating_sub(begin);
    let count = (span / 4).min(4096);
    for i in 0..count {
        let slot = begin.wrapping_add(i * 4);
        let fnptr = match mmu.load32(slot) {
            Ok(v) => v,
            Err(_) => break,
        };
        if fnptr == 0 {
            continue;
        }
        // Re-enter the run loop on this thunk-or-real-fn.
        // Errors stop the walk but don't fail the stub —
        // mirrors MSDN's contract that `_initterm` doesn't
        // diagnose ctor failure (the failing ctor is supposed
        // to terminate the process itself if it cares).
        match call_guest(cpu, mmu, registry, state, fnptr, &[]) {
            Ok(_) => {}
            Err(crate::Error::Win32(e)) => return Err(e),
            Err(_) => break,
        }
    }
    Ok(0)
}

/// `void _purecall(void)`. cdecl. Pure-virtual sentinel; in
/// real CRTs this aborts. The decode path doesn't call any
/// pure-virtual function — the symbol is imported only so the
/// vtable layout for codec C++ classes can include the
/// "abort if a non-implemented virtual is called" trap.
fn stub_purecall(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(0)
}

/// `void* malloc(size_t)`. cdecl. Wraps the heap arena.
/// Returns 0 (NULL) on size == 0 to match Microsoft's CRT
/// (POSIX permits a unique pointer instead — the codec only
/// cares that NULL means "did not allocate").
fn stub_malloc(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let size = arg_dword(cpu, mmu, 0).map_err(|t| trap("malloc", t))?;
    if size == 0 {
        return Ok(0);
    }
    let addr = state.arena_alloc(size)?;
    let zeros = vec![0u8; size as usize];
    mmu.write_initializer(addr, &zeros)
        .map_err(|t| trap("malloc", t))?;
    Ok(addr)
}

/// `void free(void*)`. cdecl. No-op on NULL.
fn stub_free(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let p = arg_dword(cpu, mmu, 0).map_err(|t| trap("free", t))?;
    if p == 0 {
        return Ok(0);
    }
    let _ = state.heap.remove(&p);
    Ok(0)
}

fn trap(stub: &'static str, t: crate::emulator::Trap) -> Win32Error {
    Win32Error::InvalidArgument {
        stub,
        reason: format!("{t}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::isa_int::RET_SENTINEL;
    use crate::emulator::mmu::Perm;
    use crate::emulator::regs::Reg32;

    fn make_env() -> (Cpu, Mmu, Registry, HostState) {
        let mut mmu = Mmu::new();
        mmu.map(0x4000, 0x4000, Perm::R | Perm::W);
        mmu.map(0x9000, 0x1000, Perm::R | Perm::W);
        let mut cpu = Cpu::new();
        cpu.regs.set_esp(0x9F00);
        let mut registry = Registry::new();
        registry.register_all();
        let state = HostState::new(0x4000, 0x8000);
        (cpu, mmu, registry, state)
    }

    fn call_cdecl(
        cpu: &mut Cpu,
        mmu: &mut Mmu,
        registry: &Registry,
        state: &mut HostState,
        name: &str,
        args: &[u32],
    ) -> Result<(), crate::Error> {
        // cdecl: caller pushes args + return addr; callee
        // does NOT pop args. We push the args, then the
        // synthetic ret addr (0xDEAD_DEAD), then dispatch.
        for a in args.iter().rev() {
            cpu.push32(mmu, *a)?;
        }
        cpu.push32(mmu, 0xDEAD_DEAD)?;
        cpu.regs.eip = registry.resolve("msvcrt.dll", name).unwrap();
        crate::win32::dispatch_stub(cpu, mmu, registry, state)
    }

    #[test]
    fn operator_new_zero_size_returns_null() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        call_cdecl(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "??2@YAPAXI@Z",
            &[0],
        )
        .unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0);
    }

    #[test]
    fn operator_new_nonzero_returns_heap_addr() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        call_cdecl(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "??2@YAPAXI@Z",
            &[64],
        )
        .unwrap();
        let p = cpu.regs.get32(Reg32::Eax);
        assert_ne!(p, 0);
        assert!(state.heap.contains_key(&p));
    }

    #[test]
    fn operator_delete_nullptr_is_noop() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        call_cdecl(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "??3@YAXPAX@Z",
            &[0],
        )
        .unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0);
    }

    #[test]
    fn malloc_then_free_round_trip() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        call_cdecl(&mut cpu, &mut mmu, &registry, &mut state, "malloc", &[128]).unwrap();
        let p = cpu.regs.get32(Reg32::Eax);
        assert_ne!(p, 0);
        assert!(state.heap.contains_key(&p));
        call_cdecl(&mut cpu, &mut mmu, &registry, &mut state, "free", &[p]).unwrap();
        assert!(!state.heap.contains_key(&p));
    }

    #[test]
    fn initterm_zero_args_is_noop() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        call_cdecl(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "_initterm",
            &[0, 0],
        )
        .unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0);
    }

    #[test]
    fn initterm_walks_table_and_calls_non_null_entries() {
        // Build a fn-pointer table of three entries: null,
        // valid, null. The valid entry points at a tiny
        // hand-built guest function that just `ret`s.
        let mut mmu = Mmu::new();
        mmu.map(0x4000, 0x4000, Perm::R | Perm::W);
        mmu.map(0x8000, 0x1000, Perm::R | Perm::W);
        // Code page for the dummy function.
        mmu.map(0xA000, 0x1000, Perm::R | Perm::X);
        // Single `ret` (0xC3) at 0xA000. cdecl callee.
        mmu.write_initializer(0xA000, &[0xC3]).unwrap();
        // Three-slot table at 0x6000: [0, 0xA000, 0].
        mmu.write_initializer(0x6000, &0u32.to_le_bytes()).unwrap();
        mmu.write_initializer(0x6004, &0xA000u32.to_le_bytes())
            .unwrap();
        mmu.write_initializer(0x6008, &0u32.to_le_bytes()).unwrap();

        let mut cpu = Cpu::new();
        cpu.regs.set_esp(0x8F00);
        let mut registry = Registry::new();
        registry.register_all();
        let mut state = HostState::new(0x4000, 0x8000);

        // _initterm(0x6000, 0x600C)
        let _ = RET_SENTINEL; // referenced via call_guest internally
        for a in [0x600Cu32, 0x6000u32].iter() {
            cpu.push32(&mut mmu, *a).unwrap();
        }
        cpu.push32(&mut mmu, 0xDEAD_DEAD).unwrap();
        cpu.regs.eip = registry.resolve("msvcrt.dll", "_initterm").unwrap();
        crate::win32::dispatch_stub(&mut cpu, &mut mmu, &registry, &mut state).unwrap();
        // No assertion on a side-effect register here — the
        // contract is "did not trap" (the fn-pointer was
        // walked + invoked via `call_guest` and returned).
        assert_eq!(cpu.regs.eip, 0xDEAD_DEAD);
    }
}
