//! Round 8 — list every Win32 import IR50_32.DLL declares + the
//! subset our registry does not satisfy. Used by the round-8
//! implementer to scope the new-stub-set.

mod common;

use oxideav_vfw::win32::Registry;

#[test]
fn ir50_32_dll_imports_inventory() {
    let bytes = common::fetch_or_load("IR50_32.DLL").expect("fetch IR50_32.DLL");
    let imports = common::list_pe_imports(&bytes).expect("imports");
    eprintln!("IR50_32.DLL declares {} (DLL, name) imports:", imports.len());
    for (dll, name) in &imports {
        eprintln!("  {dll}!{name}");
    }
    let mut registry = Registry::new();
    registry.register_all();
    let mut missing = Vec::new();
    for (dll, name) in &imports {
        if registry.resolve(dll, name).is_none() {
            missing.push(format!("{dll}!{name}"));
        }
    }
    eprintln!("\nUnsatisfied: {}", missing.len());
    for m in &missing {
        eprintln!("  {m}");
    }
}
