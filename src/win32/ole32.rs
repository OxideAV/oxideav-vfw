//! `ole32.dll` stubs — round-8 surface for IR50_32.DLL.
//!
//! IR50 imports the COM/OLE bootstrap API (`CoInitialize`,
//! `CoCreateInstance`, `CoTaskMemAlloc`, `StringFromGUID2`) for
//! its IUnknown-shaped sub-component plumbing. The decode body
//! itself does not call into COM — these imports back the
//! "Configure" dialog code path.
//!
//! Reference: MSDN "Component Object Model (COM)" —
//! `https://learn.microsoft.com/en-us/windows/win32/com/component-object-model--com-`.

use super::{arg_dword, HostState, Registry, StubFn, Win32Error};
use crate::emulator::{Cpu, Mmu};

const E_NOTIMPL: u32 = 0x8000_4001;
const S_OK: u32 = 0;

/// Register every ole32 stub.
pub fn register(registry: &mut Registry) {
    registry.register(
        "ole32.dll",
        "CoCreateInstance",
        stub_co_create_instance as StubFn,
        5,
    );
    registry.register(
        "ole32.dll",
        "CoFreeUnusedLibraries",
        stub_co_free_unused_libraries as StubFn,
        0,
    );
    registry.register(
        "ole32.dll",
        "CoInitialize",
        stub_co_initialize as StubFn,
        1,
    );
    registry.register(
        "ole32.dll",
        "CoTaskMemAlloc",
        stub_co_task_mem_alloc as StubFn,
        1,
    );
    registry.register(
        "ole32.dll",
        "CoTaskMemFree",
        stub_co_task_mem_free as StubFn,
        1,
    );
    registry.register(
        "ole32.dll",
        "CoUninitialize",
        stub_co_uninitialize as StubFn,
        0,
    );
    registry.register(
        "ole32.dll",
        "StringFromGUID2",
        stub_string_from_guid2 as StubFn,
        3,
    );
}

/// `HRESULT CoCreateInstance(...)`. Return E_NOTIMPL — the codec
/// falls back to its built-in path.
fn stub_co_create_instance(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(E_NOTIMPL)
}

/// `void CoFreeUnusedLibraries(void)`. No-op.
fn stub_co_free_unused_libraries(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(0)
}

/// `HRESULT CoInitialize(LPVOID pvReserved)`. S_OK.
fn stub_co_initialize(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(S_OK)
}

/// `LPVOID CoTaskMemAlloc(SIZE_T cb)`. Forward to the heap arena.
fn stub_co_task_mem_alloc(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let n = arg_dword(cpu, mmu, 0).map_err(|t| trap("CoTaskMemAlloc", t))?;
    if n == 0 {
        return Ok(0);
    }
    let addr = state.arena_alloc(n)?;
    let buf = vec![0u8; n as usize];
    mmu.write_initializer(addr, &buf)
        .map_err(|t| trap("CoTaskMemAlloc", t))?;
    Ok(addr)
}

/// `void CoTaskMemFree(LPVOID pv)`. No-op (we don't free arena
/// allocations).
fn stub_co_task_mem_free(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(0)
}

/// `void CoUninitialize(void)`. No-op.
fn stub_co_uninitialize(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(0)
}

/// `int StringFromGUID2(REFGUID rguid, LPOLESTR lpsz, int
/// cchMax)`. Format the 16-byte GUID into the canonical
/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` UTF-16 string and
/// write it into `lpsz`. Returns the number of UTF-16 code units
/// written including the trailing NUL (39 = 38 + 1 for the
/// canonical form), or 0 on failure.
fn stub_string_from_guid2(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let pguid = arg_dword(cpu, mmu, 0).map_err(|t| trap("StringFromGUID2", t))?;
    let psz = arg_dword(cpu, mmu, 1).map_err(|t| trap("StringFromGUID2", t))?;
    let cch = arg_dword(cpu, mmu, 2).map_err(|t| trap("StringFromGUID2", t))?;
    if pguid == 0 || psz == 0 || cch < 39 {
        return Ok(0);
    }
    // Read the 16-byte GUID.
    let mut g = [0u8; 16];
    for (i, b) in g.iter_mut().enumerate() {
        *b = mmu
            .load8(pguid + i as u32)
            .map_err(|t| trap("StringFromGUID2", t))?;
    }
    let d1 = u32::from_le_bytes([g[0], g[1], g[2], g[3]]);
    let d2 = u16::from_le_bytes([g[4], g[5]]);
    let d3 = u16::from_le_bytes([g[6], g[7]]);
    let d4 = &g[8..16];
    let s = format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        d1, d2, d3, d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7],
    );
    // Encode as UTF-16LE.
    for (i, c) in s.encode_utf16().enumerate() {
        let off = (i as u32) * 2;
        mmu.store16(psz + off, c)
            .map_err(|t| trap("StringFromGUID2", t))?;
    }
    let len = s.encode_utf16().count() as u32;
    mmu.store16(psz + len * 2, 0)
        .map_err(|t| trap("StringFromGUID2", t))?;
    Ok(len + 1)
}

fn trap(stub: &'static str, t: crate::emulator::Trap) -> Win32Error {
    Win32Error::InvalidArgument {
        stub,
        reason: format!("{t}"),
    }
}
