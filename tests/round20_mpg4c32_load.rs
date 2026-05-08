//! Round 20 — Sub-goal A. PE-load `mpg4c32.dll` (the
//! MS-MPEG-4 v3 VfW decoder) and report which IAT slots
//! the round-20 stub set still leaves unresolved.
//!
//! Per `docs/winmf/winmf-emulator.md` §"Milestone 3.1", round
//! 20 added 13 stubs:
//! * `kernel32!{CreateEventA, CreateThread, SetEvent}`
//! * `msvcrt!{??3@YAXPAX@Z, ??2@YAPAXI@Z, _adjust_fdiv,
//!   _except_handler3, _initterm, free, malloc}`
//! * `user32!{GetScrollPos, SetScrollPos, SetScrollRange}`
//! * `winmm!GetDriverModuleHandle`
//!
//! After this round the binary's import table fully resolves
//! through `Sandbox::load`, so the load step itself returns
//! `Ok(_)`. Subsequent round(s) will drive `DllMain →
//! DRV_LOAD → DRV_ENABLE → DRV_OPEN → ICOpen → ICGetInfo`.
//!
//! ## Wall
//!
//! The mpg4c32.dll bytes live under `docs/video/msmpeg4/`
//! per the project's clean-room reference policy
//! (binaries OK as black-box validators). The test reads
//! the bytes directly from that path; if missing it skips
//! with a `eprintln!` (vs `panic!`) so contributors who
//! haven't pulled the docs subtree get a clear message.
//!
//! Reference docs: `docs/winmf/winmf-emulator.md`,
//! Microsoft PE/COFF specification.

mod common;

use oxideav_vfw::win32::Registry;
use std::path::PathBuf;

/// Resolve the absolute path of the mpg4c32.dll reference
/// binary. Returns `None` if not present (caller's tests
/// then short-circuit with a stderr note rather than fail).
fn mpg4c32_path() -> Option<PathBuf> {
    // Walk up from CARGO_MANIFEST_DIR to find the workspace
    // root. The crate lives at `crates/oxideav-vfw`; the docs
    // tree lives at `<workspace_root>/docs/`.
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    let workspace_root = manifest.parent()?.parent()?;
    let p = workspace_root.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/mpg4c32.dll");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn read_mpg4c32() -> Option<Vec<u8>> {
    let p = mpg4c32_path()?;
    std::fs::read(&p).ok()
}

#[test]
fn mpg4c32_dll_imports_inventory_after_round20_stubs() {
    let Some(bytes) = read_mpg4c32() else {
        eprintln!(
            "round20: mpg4c32.dll not present in docs/video/msmpeg4/reference/binaries/; skipping"
        );
        return;
    };
    let imports = common::list_pe_imports(&bytes).expect("parse mpg4c32.dll imports");
    eprintln!(
        "round20: mpg4c32.dll declares {} (DLL, name) imports:",
        imports.len()
    );
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
    eprintln!("\nround20: unsatisfied: {}", missing.len());
    for m in &missing {
        eprintln!("  {m}");
    }
    // The round-20 milestone gate: every Milestone-3.1 import
    // must resolve. If new ones surface, we surface them
    // verbatim.
    assert!(
        missing.is_empty(),
        "round20: expected zero unsatisfied mpg4c32.dll imports, got: {missing:?}"
    );
}

#[test]
fn mpg4c32_dll_loads_through_sandbox() {
    use oxideav_vfw::Sandbox;
    let Some(bytes) = read_mpg4c32() else {
        eprintln!(
            "round20: mpg4c32.dll not present in docs/video/msmpeg4/reference/binaries/; skipping"
        );
        return;
    };
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    let img = sb
        .load("mpg4c32.dll", &bytes)
        .expect("Sandbox::load mpg4c32.dll");
    eprintln!(
        "round20: mpg4c32.dll loaded — image_base={:#x}, entry_point={:#x}",
        img.image_base, img.entry_point,
    );
    let mut names: Vec<&String> = img.exports.keys().collect();
    names.sort();
    eprintln!("round20: mpg4c32.dll exports {} symbols:", names.len());
    for n in &names {
        eprintln!("  {n} → {:?}", img.export(n),);
    }
    // PE-load success is the round-20 gate. DllMain runs in
    // round 21+ once the next-blocker imports/opcodes are
    // identified.
}

/// Drive DRV_LOAD → DRV_ENABLE → DRV_OPEN → ICOpen →
/// ICGetInfo against mpg4c32.dll. The reach gate per
/// Milestone 3.1: "load + DRV_LOAD + DRV_ENABLE + DRV_OPEN +
/// ICOpen + ICGetInfo returns ICERR_OK". This test reports
/// the first stop on the path so the round-21 implementer
/// has a concrete next-blocker.
#[test]
fn mpg4c32_drv_load_drv_open_ic_open_attempt() {
    use oxideav_vfw::Sandbox;
    let Some(bytes) = read_mpg4c32() else {
        eprintln!(
            "round20: mpg4c32.dll not present in docs/video/msmpeg4/reference/binaries/; skipping"
        );
        return;
    };
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(500_000_000);
    let img = sb
        .load("mpg4c32.dll", &bytes)
        .expect("Sandbox::load mpg4c32.dll");
    eprintln!(
        "round20: image_base={:#x}, DriverProc VA={:?}, entry_point={:#x}",
        img.image_base,
        img.export("DriverProc"),
        img.entry_point,
    );
    sb.host.trace_stubs = true;

    // 1. DllMain (= PE entry point — mpg4c32 has no DllMain
    // export).
    match sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH) {
        Ok(r) => eprintln!(
            "round20: DllMain returned {r:#010x} after {} instructions",
            sb.cpu.instr_count
        ),
        Err(e) => {
            eprintln!(
                "round20: DllMain trapped: {e}; eip={:#010x}",
                sb.cpu.regs.eip
            );
            return;
        }
    }

    // 2. Install codec + drive ICOpen against the IV31-style
    // four-CC `MP43` (MS-MPEG-4 v3 standard FOURCC).
    if let Err(e) = sb.install_codec(&img) {
        eprintln!("round20: install_codec failed: {e}");
        return;
    }
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"MP43");
    let pre = sb.cpu.instr_count;
    match sb.ic_open(fcc_video, fcc_handler, 2 /* ICMODE_DECOMPRESS */) {
        Ok(hic) => {
            eprintln!(
                "round20: ICOpen('VIDC','MP43') → hic={hic:#010x}, {} instructions",
                sb.cpu.instr_count - pre,
            );
            if hic == 0 {
                eprintln!("round20: ICOpen returned 0 — codec rejected the FOURCC.");
                return;
            }

            // 3. ICGetInfo
            //
            // Round 24 — `cb` MUST be `>= ICINFO_SIZE` (= 568).
            // Round-20's original `cb=80` hit mpg4c32's strict
            // `cmp [ebp+0x10], 0x238 / jb .return_zero` gate at
            // `mpg4c32!DriverProc+0x999..0x99c`, so the codec
            // returned 0 bytes silently. With `cb=568` the codec
            // populates the full ICINFO record.
            let pre2 = sb.cpu.instr_count;
            match sb.ic_get_info(hic, oxideav_vfw::win32::vfw32::ICINFO_SIZE) {
                Ok(info) => {
                    eprintln!(
                        "round20: ICGetInfo returned {} bytes after {} instructions",
                        info.len(),
                        sb.cpu.instr_count - pre2,
                    );
                    eprintln!("round20: ICINFO bytes: {:02x?}", &info);
                }
                Err(e) => eprintln!("round20: ICGetInfo trapped: {e}"),
            }
            let _ = sb.ic_close(hic);
        }
        Err(e) => eprintln!(
            "round20: ICOpen trapped after {} instructions: {e}; eip={:#010x}",
            sb.cpu.instr_count - pre,
            sb.cpu.regs.eip,
        ),
    }
    eprintln!(
        "round20: post-walkthrough stub trace ({} stub calls; first 30 shown):",
        sb.host.stub_trace.len()
    );
    for (i, line) in sb.host.stub_trace.iter().take(30).enumerate() {
        eprintln!("    [{i}]: {line}");
    }
    eprintln!(
        "round20: post-walkthrough debug log ({} entries):",
        sb.host.debug_log.len()
    );
    for (i, line) in sb.host.debug_log.iter().take(20).enumerate() {
        eprintln!("    [{i}]: {line}");
    }
}

#[test]
fn mpg4c32_dll_dllmain_attempt() {
    use oxideav_vfw::Sandbox;
    let Some(bytes) = read_mpg4c32() else {
        eprintln!(
            "round20: mpg4c32.dll not present in docs/video/msmpeg4/reference/binaries/; skipping"
        );
        return;
    };
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(200_000_000);
    sb.cpu.enable_trace_ring(64);
    let img = sb
        .load("mpg4c32.dll", &bytes)
        .expect("Sandbox::load mpg4c32.dll");
    // Driver export name discovery — record what we see.
    eprintln!(
        "round20: DllMain VA={:?} DriverProc VA={:?} entry_point={:#010x}",
        img.export("DllMain"),
        img.export("DriverProc"),
        img.entry_point,
    );
    sb.host.trace_stubs = true;

    // DllMain is the very first thing PE-loaded code does;
    // try it and report what happens. Round 20's success
    // criterion is "Sandbox::load returns Ok"; DllMain may
    // surface the next blocker (additional opcode, missing
    // FS-relative TEB field, …).
    match sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH) {
        Ok(r) => {
            eprintln!(
                "round20: DllMain returned {r:#010x} after {} instructions",
                sb.cpu.instr_count
            );
        }
        Err(e) => {
            eprintln!(
                "round20: DllMain trapped after {} instructions; eip={:#010x}; trap: {e}",
                sb.cpu.instr_count, sb.cpu.regs.eip,
            );
            // Dump trace ring + bytes around the trap.
            eprintln!(
                "round20: instruction trace ring (last {} eips):",
                sb.cpu.trace_ring.len()
            );
            for (i, eip) in sb.cpu.trace_ring.iter().enumerate() {
                let mut bs = [0u8; 8];
                for j in 0..8u32 {
                    bs[j as usize] = sb.mmu.load8(eip + j).unwrap_or(0);
                }
                eprintln!("    [{i}]: {eip:#010x} bytes={bs:02x?}");
            }
        }
    }
    eprintln!(
        "round20: post-DllMain stub trace ({} stub calls):",
        sb.host.stub_trace.len()
    );
    for (i, line) in sb.host.stub_trace.iter().take(40).enumerate() {
        eprintln!("    [{i}]: {line}");
    }
    eprintln!(
        "round20: post-DllMain debug log ({} entries):",
        sb.host.debug_log.len()
    );
    for (i, line) in sb.host.debug_log.iter().take(20).enumerate() {
        eprintln!("  [{i}]: {line}");
    }
}
