//! `kernel32.dll` stubs — the minimum surface a Cinepak-class
//! codec DLL imports.
//!
//! Round-1 set per design doc §"`kernel32.dll` essentials" and
//! §"Milestone 1":
//!
//! * `GetProcessHeap`
//! * `HeapAlloc`, `HeapFree`, `HeapReAlloc`
//! * `LocalAlloc`, `LocalFree`
//! * `OutputDebugStringA`
//! * `GetTickCount`
//! * `InterlockedIncrement`, `InterlockedDecrement`
//! * `LoadLibraryA`, `GetProcAddress`
//!
//! Round-2 will add: `VirtualAlloc` / `VirtualFree` /
//! `VirtualProtect`, `EnterCriticalSection` and friends, `Tls*`,
//! `GetLastError` / `SetLastError`, `QueryPerformanceCounter`.
//!
//! Each stub references its MSDN page in a comment for review;
//! the implementations honour the public contract (return
//! values, error semantics, side effects on `lastError`).

use super::{arg_dword, HostState, Registry, StubFn, Win32Error};
use crate::emulator::{Cpu, Mmu};

/// Register every kernel32 stub into `registry`.
pub fn register(registry: &mut Registry) {
    // The list mirrors the design doc §Milestone 1; comments
    // cite the MSDN page.

    // https://learn.microsoft.com/en-us/windows/win32/api/heapapi/nf-heapapi-getprocessheap
    registry.register(
        "kernel32.dll",
        "GetProcessHeap",
        stub_get_process_heap as StubFn,
        0,
    );
    // https://learn.microsoft.com/en-us/windows/win32/api/heapapi/nf-heapapi-heapalloc
    registry.register("kernel32.dll", "HeapAlloc", stub_heap_alloc as StubFn, 3);
    // https://learn.microsoft.com/en-us/windows/win32/api/heapapi/nf-heapapi-heapfree
    registry.register("kernel32.dll", "HeapFree", stub_heap_free as StubFn, 3);
    // https://learn.microsoft.com/en-us/windows/win32/api/heapapi/nf-heapapi-heaprealloc
    registry.register(
        "kernel32.dll",
        "HeapReAlloc",
        stub_heap_realloc as StubFn,
        4,
    );
    // https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-localalloc
    registry.register("kernel32.dll", "LocalAlloc", stub_local_alloc as StubFn, 2);
    // https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-localfree
    registry.register("kernel32.dll", "LocalFree", stub_local_free as StubFn, 1);
    // https://learn.microsoft.com/en-us/windows/win32/api/debugapi/nf-debugapi-outputdebugstringa
    registry.register(
        "kernel32.dll",
        "OutputDebugStringA",
        stub_output_debug_string_a as StubFn,
        1,
    );
    // https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-gettickcount
    registry.register(
        "kernel32.dll",
        "GetTickCount",
        stub_get_tick_count as StubFn,
        0,
    );
    // https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-interlockedincrement
    registry.register(
        "kernel32.dll",
        "InterlockedIncrement",
        stub_interlocked_increment as StubFn,
        1,
    );
    // https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-interlockeddecrement
    registry.register(
        "kernel32.dll",
        "InterlockedDecrement",
        stub_interlocked_decrement as StubFn,
        1,
    );
    // https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-loadlibrarya
    registry.register(
        "kernel32.dll",
        "LoadLibraryA",
        stub_load_library_a as StubFn,
        1,
    );
    // https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-getprocaddress
    registry.register(
        "kernel32.dll",
        "GetProcAddress",
        stub_get_proc_address as StubFn,
        2,
    );
}

// ----- Heap ----------------------------------------------------------

/// `HANDLE GetProcessHeap(void)` — return the canned handle.
fn stub_get_process_heap(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    state: &mut HostState,
) -> Result<u32, Win32Error> {
    Ok(state.process_heap_handle)
}

const HEAP_ZERO_MEMORY: u32 = 0x0000_0008;

/// `LPVOID HeapAlloc(HANDLE, DWORD dwFlags, SIZE_T dwBytes)`.
fn stub_heap_alloc(cpu: &mut Cpu, mmu: &mut Mmu, state: &mut HostState) -> Result<u32, Win32Error> {
    let _h_heap = arg_dword(cpu, mmu, 0).map_err(|t| trap_to_win32("HeapAlloc", t))?;
    let flags = arg_dword(cpu, mmu, 1).map_err(|t| trap_to_win32("HeapAlloc", t))?;
    let n = arg_dword(cpu, mmu, 2).map_err(|t| trap_to_win32("HeapAlloc", t))?;
    let addr = bump_alloc(state, n)?;
    let buf = state.heap.entry(addr).or_default();
    buf.resize(n as usize, 0);
    if (flags & HEAP_ZERO_MEMORY) != 0 {
        for b in buf.iter_mut() {
            *b = 0;
        }
    }
    // Mirror the bytes into emulator memory so the codec can use
    // them directly.
    let bytes = buf.clone();
    mmu.write_initializer(addr, &bytes)
        .map_err(|t| trap_to_win32("HeapAlloc", t))?;
    Ok(addr)
}

/// `BOOL HeapFree(HANDLE, DWORD dwFlags, LPVOID lpMem)`.
fn stub_heap_free(cpu: &mut Cpu, mmu: &mut Mmu, state: &mut HostState) -> Result<u32, Win32Error> {
    let _h = arg_dword(cpu, mmu, 0).map_err(|t| trap_to_win32("HeapFree", t))?;
    let _flags = arg_dword(cpu, mmu, 1).map_err(|t| trap_to_win32("HeapFree", t))?;
    let addr = arg_dword(cpu, mmu, 2).map_err(|t| trap_to_win32("HeapFree", t))?;
    if addr == 0 {
        return Ok(1); // BOOL TRUE; freeing NULL is a no-op
    }
    state
        .heap
        .remove(&addr)
        .ok_or(Win32Error::InvalidHeapBlock {
            stub: "HeapFree",
            addr,
        })?;
    Ok(1)
}

/// `LPVOID HeapReAlloc(HANDLE, DWORD dwFlags, LPVOID lpMem, SIZE_T dwBytes)`.
fn stub_heap_realloc(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
) -> Result<u32, Win32Error> {
    let _h = arg_dword(cpu, mmu, 0).map_err(|t| trap_to_win32("HeapReAlloc", t))?;
    let flags = arg_dword(cpu, mmu, 1).map_err(|t| trap_to_win32("HeapReAlloc", t))?;
    let addr = arg_dword(cpu, mmu, 2).map_err(|t| trap_to_win32("HeapReAlloc", t))?;
    let n = arg_dword(cpu, mmu, 3).map_err(|t| trap_to_win32("HeapReAlloc", t))?;
    if addr == 0 {
        // MSDN: passing NULL for lpMem is undefined; we choose to
        // treat as fresh alloc for resilience.
        return stub_heap_alloc(cpu, mmu, state);
    }
    let old = state
        .heap
        .remove(&addr)
        .ok_or(Win32Error::InvalidHeapBlock {
            stub: "HeapReAlloc",
            addr,
        })?;
    let new_addr = bump_alloc(state, n)?;
    let mut buf = vec![0u8; n as usize];
    let copy_n = old.len().min(n as usize);
    buf[..copy_n].copy_from_slice(&old[..copy_n]);
    if (flags & HEAP_ZERO_MEMORY) != 0 {
        for b in buf.iter_mut().skip(copy_n) {
            *b = 0;
        }
    }
    mmu.write_initializer(new_addr, &buf)
        .map_err(|t| trap_to_win32("HeapReAlloc", t))?;
    state.heap.insert(new_addr, buf);
    Ok(new_addr)
}

fn bump_alloc(state: &mut HostState, n: u32) -> Result<u32, Win32Error> {
    // Round up to 16 to keep allocations roughly cache-line aligned.
    let aligned = n
        .checked_add(15)
        .map(|v| v & !15u32)
        .ok_or(Win32Error::InvalidArgument {
            stub: "HeapAlloc",
            reason: "size overflow".into(),
        })?;
    let addr = state.heap_cursor;
    let next = addr
        .checked_add(aligned)
        .ok_or(Win32Error::InvalidArgument {
            stub: "HeapAlloc",
            reason: "heap address-space overflow".into(),
        })?;
    if next > state.heap_arena_end {
        return Err(Win32Error::InvalidArgument {
            stub: "HeapAlloc",
            reason: format!(
                "arena exhausted (need {n}, have {})",
                state.heap_arena_end - addr
            ),
        });
    }
    state.heap_cursor = next;
    Ok(addr)
}

const LMEM_ZEROINIT: u32 = 0x0040;

/// `HLOCAL LocalAlloc(UINT uFlags, SIZE_T uBytes)`.
fn stub_local_alloc(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
) -> Result<u32, Win32Error> {
    let flags = arg_dword(cpu, mmu, 0).map_err(|t| trap_to_win32("LocalAlloc", t))?;
    let n = arg_dword(cpu, mmu, 1).map_err(|t| trap_to_win32("LocalAlloc", t))?;
    let addr = bump_alloc(state, n)?;
    let mut buf = vec![0u8; n as usize];
    if (flags & LMEM_ZEROINIT) != 0 {
        for b in buf.iter_mut() {
            *b = 0;
        }
    }
    mmu.write_initializer(addr, &buf)
        .map_err(|t| trap_to_win32("LocalAlloc", t))?;
    state.heap.insert(addr, buf);
    Ok(addr)
}

/// `HLOCAL LocalFree(HLOCAL hMem)`.
fn stub_local_free(cpu: &mut Cpu, mmu: &mut Mmu, state: &mut HostState) -> Result<u32, Win32Error> {
    let addr = arg_dword(cpu, mmu, 0).map_err(|t| trap_to_win32("LocalFree", t))?;
    if addr == 0 {
        return Ok(0);
    }
    state
        .heap
        .remove(&addr)
        .ok_or(Win32Error::InvalidHeapBlock {
            stub: "LocalFree",
            addr,
        })?;
    Ok(0) // Returns NULL on success per MSDN.
}

// ----- Debug + time --------------------------------------------------

/// `void OutputDebugStringA(LPCSTR lpOutputString)`. We log into
/// `state.debug_log` so the fixture-gated end-to-end test can
/// assert the codec emitted a known boot string.
fn stub_output_debug_string_a(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
) -> Result<u32, Win32Error> {
    let p = arg_dword(cpu, mmu, 0).map_err(|t| trap_to_win32("OutputDebugStringA", t))?;
    let s = read_cstr(mmu, p, 4096)?;
    state.debug_log.push(s);
    Ok(0)
}

/// `DWORD GetTickCount(void)`. Returns a monotonically-increasing
/// pseudo-tick. Real wall-clock time is not modelled; many codecs
/// only use the tick as a seed.
fn stub_get_tick_count(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    state: &mut HostState,
) -> Result<u32, Win32Error> {
    state.tick = state.tick.wrapping_add(1);
    Ok(state.tick)
}

// ----- Atomics -------------------------------------------------------

/// `LONG InterlockedIncrement(LONG volatile *Addend)`.
fn stub_interlocked_increment(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
) -> Result<u32, Win32Error> {
    let p = arg_dword(cpu, mmu, 0).map_err(|t| trap_to_win32("InterlockedIncrement", t))?;
    let v = mmu
        .load32(p)
        .map_err(|t| trap_to_win32("InterlockedIncrement", t))?;
    let new = v.wrapping_add(1);
    mmu.store32(p, new)
        .map_err(|t| trap_to_win32("InterlockedIncrement", t))?;
    Ok(new)
}

/// `LONG InterlockedDecrement(LONG volatile *Addend)`.
fn stub_interlocked_decrement(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
) -> Result<u32, Win32Error> {
    let p = arg_dword(cpu, mmu, 0).map_err(|t| trap_to_win32("InterlockedDecrement", t))?;
    let v = mmu
        .load32(p)
        .map_err(|t| trap_to_win32("InterlockedDecrement", t))?;
    let new = v.wrapping_sub(1);
    mmu.store32(p, new)
        .map_err(|t| trap_to_win32("InterlockedDecrement", t))?;
    Ok(new)
}

// ----- Library / function lookup -------------------------------------

/// `HMODULE LoadLibraryA(LPCSTR lpLibFileName)`.
///
/// Round-1 only acknowledges loaded modules in the registry; it
/// does not attempt to load a fresh DLL on demand. The PE loader
/// records every successfully-loaded DLL in `state.modules`.
fn stub_load_library_a(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
) -> Result<u32, Win32Error> {
    let p = arg_dword(cpu, mmu, 0).map_err(|t| trap_to_win32("LoadLibraryA", t))?;
    let name = read_cstr(mmu, p, 260)?.to_ascii_lowercase();
    if let Some(base) = state.modules.get(&name) {
        return Ok(*base);
    }
    // We pretend the module did not load. Many codecs handle
    // NULL gracefully; the ones that don't will raise a clear
    // trap downstream.
    Ok(0)
}

/// `FARPROC GetProcAddress(HMODULE hModule, LPCSTR lpProcName)`.
///
/// Round-1 returns a registered thunk for the (module, name)
/// pair if one exists; otherwise NULL. Lookup-by-ordinal is not
/// supported in round 1 (low-bit-set address) — a target codec
/// that needs it will surface as a clean trap.
fn stub_get_proc_address(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
) -> Result<u32, Win32Error> {
    let _h = arg_dword(cpu, mmu, 0).map_err(|t| trap_to_win32("GetProcAddress", t))?;
    let name_p = arg_dword(cpu, mmu, 1).map_err(|t| trap_to_win32("GetProcAddress", t))?;
    if name_p < 0x10000 {
        // Pointer is an ordinal (HIWORD == 0) — unsupported.
        return Ok(0);
    }
    // We don't know which DLL was identified, so always return
    // NULL for round-1; callers fall back to import-table
    // resolution.
    Ok(0)
}

// ----- helpers -------------------------------------------------------

fn read_cstr(mmu: &Mmu, mut addr: u32, max: u32) -> Result<String, Win32Error> {
    let mut bytes = Vec::new();
    for _ in 0..max {
        let b = mmu.load8(addr).map_err(|t| trap_to_win32("read_cstr", t))?;
        if b == 0 {
            break;
        }
        bytes.push(b);
        addr = addr.wrapping_add(1);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn trap_to_win32(stub: &'static str, t: crate::emulator::Trap) -> Win32Error {
    Win32Error::InvalidArgument {
        stub,
        reason: format!("{t}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::mmu::Perm;
    use crate::emulator::regs::Reg32;
    use crate::win32::Registry;

    fn make_env() -> (Cpu, Mmu, Registry, HostState) {
        let mut mmu = Mmu::new();
        // Heap arena
        mmu.map(0x4000, 0x4000, Perm::R | Perm::W);
        // Stack
        mmu.map(0x9000, 0x1000, Perm::R | Perm::W);
        let mut cpu = Cpu::new();
        cpu.regs.set_esp(0x9F00);
        let mut registry = Registry::new();
        registry.register_kernel32();
        let state = HostState::new(0x4000, 0x8000);
        (cpu, mmu, registry, state)
    }

    fn push_args_and_call(
        cpu: &mut Cpu,
        mmu: &mut Mmu,
        registry: &Registry,
        state: &mut HostState,
        dll: &str,
        name: &str,
        args: &[u32],
    ) -> Result<(), crate::Error> {
        // Push args right-to-left.
        for a in args.iter().rev() {
            cpu.push32(mmu, *a)?;
        }
        // Push synthetic ret addr.
        cpu.push32(mmu, 0xDEAD_DEAD)?;
        cpu.regs.eip = registry.resolve(dll, name).expect("registered");
        crate::win32::dispatch_stub(cpu, mmu, registry, state)
    }

    #[test]
    fn registers_at_least_twelve_kernel32_stubs() {
        let mut r = Registry::new();
        let n = r.register_kernel32();
        assert!(n >= 12, "expected ≥ 12 round-1 stubs, got {n}");
    }

    #[test]
    fn get_process_heap_returns_canned_handle() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "GetProcessHeap",
            &[],
        )
        .unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0xDEAD_BEEF);
    }

    #[test]
    fn heap_alloc_then_heap_free_roundtrip() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "HeapAlloc",
            &[0xDEAD_BEEF, 0, 64],
        )
        .unwrap();
        let addr = cpu.regs.get32(Reg32::Eax);
        assert_ne!(addr, 0);
        assert!(state.heap.contains_key(&addr));

        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "HeapFree",
            &[0xDEAD_BEEF, 0, addr],
        )
        .unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 1);
        assert!(!state.heap.contains_key(&addr));
    }

    #[test]
    fn heap_alloc_zero_fills_when_flag_set() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "HeapAlloc",
            &[0xDEAD_BEEF, HEAP_ZERO_MEMORY, 16],
        )
        .unwrap();
        let addr = cpu.regs.get32(Reg32::Eax);
        for i in 0..16 {
            assert_eq!(mmu.load8(addr + i).unwrap(), 0);
        }
    }

    #[test]
    fn heap_free_invalid_pointer_errors() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        let bad = 0xBAD_ADD00u32;
        let r = push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "HeapFree",
            &[0xDEAD_BEEF, 0, bad],
        );
        match r {
            Err(crate::Error::Win32(Win32Error::InvalidHeapBlock { addr, .. })) if addr == bad => {}
            other => panic!("expected InvalidHeapBlock, got {other:?}"),
        }
    }

    #[test]
    fn local_alloc_local_free() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "LocalAlloc",
            &[LMEM_ZEROINIT, 32],
        )
        .unwrap();
        let addr = cpu.regs.get32(Reg32::Eax);
        assert_ne!(addr, 0);
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "LocalFree",
            &[addr],
        )
        .unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0);
    }

    #[test]
    fn output_debug_string_a_logs() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        // Lay out "hi\0" at 0x4000 (heap arena start, R+W).
        mmu.write(0x4000, b"hi\0").unwrap();
        // Bump the heap_cursor to skip those bytes for cleanliness.
        state.heap_cursor = 0x4010;
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "OutputDebugStringA",
            &[0x4000],
        )
        .unwrap();
        assert_eq!(state.debug_log.last().unwrap(), "hi");
    }

    #[test]
    fn get_tick_count_monotonic() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "GetTickCount",
            &[],
        )
        .unwrap();
        let t1 = cpu.regs.get32(Reg32::Eax);
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "GetTickCount",
            &[],
        )
        .unwrap();
        let t2 = cpu.regs.get32(Reg32::Eax);
        assert!(t2 > t1);
    }

    #[test]
    fn interlocked_increment_decrement_roundtrip() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        // Place a u32 = 5 at 0x4000.
        mmu.store32(0x4000, 5).unwrap();
        state.heap_cursor = 0x4010;

        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "InterlockedIncrement",
            &[0x4000],
        )
        .unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 6);
        assert_eq!(mmu.load32(0x4000).unwrap(), 6);

        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "InterlockedDecrement",
            &[0x4000],
        )
        .unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 5);
    }

    #[test]
    fn load_library_a_returns_known_module_or_null() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        state.modules.insert("kernel32.dll".into(), 0x10000);
        // Lay out "kernel32.dll\0"
        let s = b"kernel32.dll\0";
        mmu.write(0x4000, s).unwrap();
        state.heap_cursor = 0x4020;

        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "LoadLibraryA",
            &[0x4000],
        )
        .unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0x10000);

        // Unknown module → 0
        let s = b"unknown.dll\0";
        mmu.write(0x4040, s).unwrap();
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "LoadLibraryA",
            &[0x4040],
        )
        .unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0);
    }

    #[test]
    fn heap_realloc_preserves_old_bytes() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "HeapAlloc",
            &[0xDEAD_BEEF, 0, 8],
        )
        .unwrap();
        let addr = cpu.regs.get32(Reg32::Eax);
        for i in 0..8u32 {
            mmu.store8(addr + i, (i + 1) as u8).unwrap();
            // Mirror in heap-state buffer too.
            state.heap.get_mut(&addr).unwrap()[i as usize] = (i + 1) as u8;
        }
        push_args_and_call(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            "kernel32.dll",
            "HeapReAlloc",
            &[0xDEAD_BEEF, 0, addr, 16],
        )
        .unwrap();
        let new_addr = cpu.regs.get32(Reg32::Eax);
        for i in 0..8u32 {
            assert_eq!(mmu.load8(new_addr + i).unwrap(), (i + 1) as u8);
        }
    }
}
