//! `advapi32.dll` stubs — round-8 surface for IR50_32.DLL.
//!
//! IR50 imports the registry API to enumerate codec-specific keys
//! (`HKLM\SOFTWARE\Intel\Indeo Video 5\…`). The decode pipeline
//! does not actually depend on registry data — codecs use
//! it for per-machine bandwidth / quality tuning that the
//! decode body has fall-back defaults for.
//!
//! Every stub returns the canonical "no key / no value" result
//! so the codec falls into its hard-coded default path.
//!
//! Reference: MSDN "Registry Functions" —
//! `https://learn.microsoft.com/en-us/windows/win32/api/winreg/`.

use super::{arg_dword, HostState, Registry, StubFn, Win32Error};
use crate::emulator::{Cpu, Mmu};

// winerror.h: common return codes.
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_SUCCESS: u32 = 0;

/// Register every advapi32 stub.
pub fn register(registry: &mut Registry) {
    registry.register("advapi32.dll", "RegCloseKey", stub_reg_close_key as StubFn, 1);
    registry.register(
        "advapi32.dll",
        "RegCreateKeyA",
        stub_reg_create_key as StubFn,
        3,
    );
    registry.register(
        "advapi32.dll",
        "RegCreateKeyExA",
        stub_reg_create_key_ex as StubFn,
        9,
    );
    registry.register(
        "advapi32.dll",
        "RegDeleteKeyA",
        stub_reg_delete as StubFn,
        2,
    );
    registry.register(
        "advapi32.dll",
        "RegDeleteValueA",
        stub_reg_delete as StubFn,
        2,
    );
    registry.register(
        "advapi32.dll",
        "RegEnumKeyExA",
        stub_reg_enum_key_ex_a as StubFn,
        8,
    );
    registry.register(
        "advapi32.dll",
        "RegOpenKeyA",
        stub_reg_open_key_a as StubFn,
        3,
    );
    registry.register(
        "advapi32.dll",
        "RegOpenKeyExA",
        stub_reg_open_key_ex_a as StubFn,
        5,
    );
    registry.register(
        "advapi32.dll",
        "RegQueryValueA",
        stub_reg_query_value as StubFn,
        4,
    );
    registry.register(
        "advapi32.dll",
        "RegQueryValueExA",
        stub_reg_query_value_ex as StubFn,
        6,
    );
    registry.register(
        "advapi32.dll",
        "RegSetValueA",
        stub_reg_set_value as StubFn,
        5,
    );
    registry.register(
        "advapi32.dll",
        "RegSetValueExA",
        stub_reg_set_value as StubFn,
        6,
    );
}

/// `LSTATUS RegCloseKey(HKEY)`. Always succeeds.
fn stub_reg_close_key(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(ERROR_SUCCESS)
}

/// `LSTATUS RegCreateKeyA(HKEY hKey, LPCSTR lpSubKey, PHKEY
/// phkResult)`. Pretend the key exists; write a non-zero handle
/// to `phkResult` so RAII wrappers proceed.
fn stub_reg_create_key(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let _hkey = arg_dword(cpu, mmu, 0).map_err(|t| trap("RegCreateKeyA", t))?;
    let _sub = arg_dword(cpu, mmu, 1).map_err(|t| trap("RegCreateKeyA", t))?;
    let phk = arg_dword(cpu, mmu, 2).map_err(|t| trap("RegCreateKeyA", t))?;
    if phk != 0 {
        let _ = mmu.store32(phk, 0xC0DE_8E0F);
    }
    Ok(ERROR_SUCCESS)
}

/// `LSTATUS RegCreateKeyExA(...)`. Same outcome as RegCreateKeyA.
fn stub_reg_create_key_ex(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let phk = arg_dword(cpu, mmu, 6).map_err(|t| trap("RegCreateKeyExA", t))?;
    if phk != 0 {
        let _ = mmu.store32(phk, 0xC0DE_8E0F);
    }
    let disposition = arg_dword(cpu, mmu, 7).map_err(|t| trap("RegCreateKeyExA", t))?;
    if disposition != 0 {
        // REG_OPENED_EXISTING_KEY = 2
        let _ = mmu.store32(disposition, 2);
    }
    Ok(ERROR_SUCCESS)
}

/// `LSTATUS RegDeleteKeyA / RegDeleteValueA(...)`. No-op success.
fn stub_reg_delete(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(ERROR_SUCCESS)
}

/// `LSTATUS RegEnumKeyExA(...)`. Return ERROR_NO_MORE_ITEMS = 259
/// on first call so codecs that iterate registry sub-keys exit
/// cleanly.
fn stub_reg_enum_key_ex_a(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    const ERROR_NO_MORE_ITEMS: u32 = 259;
    Ok(ERROR_NO_MORE_ITEMS)
}

/// `LSTATUS RegOpenKeyA(HKEY, LPCSTR lpSubKey, PHKEY phkResult)`.
/// Return ERROR_FILE_NOT_FOUND so the codec uses defaults. Some
/// CRTs treat ERROR_SUCCESS + zero handle as "fail soft"; we stay
/// strict here so the codec fully ignores registry state.
fn stub_reg_open_key_a(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let phk = arg_dword(cpu, mmu, 2).map_err(|t| trap("RegOpenKeyA", t))?;
    if phk != 0 {
        let _ = mmu.store32(phk, 0);
    }
    Ok(ERROR_FILE_NOT_FOUND)
}

/// `LSTATUS RegOpenKeyExA(...)`. Same default as RegOpenKeyA.
fn stub_reg_open_key_ex_a(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let phk = arg_dword(cpu, mmu, 4).map_err(|t| trap("RegOpenKeyExA", t))?;
    if phk != 0 {
        let _ = mmu.store32(phk, 0);
    }
    Ok(ERROR_FILE_NOT_FOUND)
}

/// `LSTATUS RegQueryValueA(...)`. Return ERROR_FILE_NOT_FOUND.
fn stub_reg_query_value(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(ERROR_FILE_NOT_FOUND)
}

/// `LSTATUS RegQueryValueExA(HKEY, LPCSTR lpValueName, LPDWORD
/// lpReserved, LPDWORD lpType, LPBYTE lpData, LPDWORD lpcbData)`.
/// Return ERROR_FILE_NOT_FOUND with cb = 0 so callers see "no
/// data".
fn stub_reg_query_value_ex(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    let pcb = arg_dword(cpu, mmu, 5).map_err(|t| trap("RegQueryValueExA", t))?;
    if pcb != 0 {
        let _ = mmu.store32(pcb, 0);
    }
    Ok(ERROR_FILE_NOT_FOUND)
}

/// `LSTATUS RegSetValueA / RegSetValueExA(...)`. No-op success.
fn stub_reg_set_value(
    _cpu: &mut Cpu,
    _mmu: &mut Mmu,
    _state: &mut HostState,
    _registry: &Registry,
) -> Result<u32, Win32Error> {
    Ok(ERROR_SUCCESS)
}

fn trap(stub: &'static str, t: crate::emulator::Trap) -> Win32Error {
    Win32Error::InvalidArgument {
        stub,
        reason: format!("{t}"),
    }
}
