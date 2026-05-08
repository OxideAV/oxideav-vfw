//! Round 21 sub-goal B: PE-load mpg4ds32.ax + wmvds32.ax.
mod common;

use oxideav_vfw::win32::Registry;
use oxideav_vfw::Sandbox;
use std::path::PathBuf;

fn binary_path(name: &str) -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    let workspace_root = manifest.parent()?.parent()?;
    let p = workspace_root.join(format!(
        "docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/{name}"
    ));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn report_imports(name: &str, bytes: &[u8]) {
    let imports = match common::list_pe_imports(bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{name}: parse error: {e}");
            return;
        }
    };
    let mut registry = Registry::new();
    registry.register_all();
    let mut missing = Vec::new();
    for (dll, n) in &imports {
        if registry.resolve(dll, n).is_none() {
            missing.push(format!("{dll}!{n}"));
        }
    }
    eprintln!(
        "{name}: {} imports, {} missing:",
        imports.len(),
        missing.len()
    );
    for m in &missing {
        eprintln!("    {m}");
    }
}

#[test]
fn round21_load_mpg4ds32() {
    let Some(p) = binary_path("MPG4DS32.AX") else {
        eprintln!("MPG4DS32.AX not present; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    report_imports("MPG4DS32.AX", &bytes);
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("MPG4DS32.AX", &bytes) {
        Ok(img) => eprintln!(
            "MPG4DS32.AX: loaded image_base={:#x}, entry_point={:#x}, exports={}",
            img.image_base,
            img.entry_point,
            img.exports.len()
        ),
        Err(e) => eprintln!("MPG4DS32.AX: load failed: {e}"),
    }
}

#[test]
fn round21_load_wmvds32() {
    let Some(p) = binary_path("WMVDS32.AX") else {
        eprintln!("WMVDS32.AX not present; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    report_imports("WMVDS32.AX", &bytes);
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("WMVDS32.AX", &bytes) {
        Ok(img) => eprintln!(
            "WMVDS32.AX: loaded image_base={:#x}, entry_point={:#x}, exports={}",
            img.image_base,
            img.entry_point,
            img.exports.len()
        ),
        Err(e) => eprintln!("WMVDS32.AX: load failed: {e}"),
    }
}
