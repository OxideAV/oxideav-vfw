//! Round 56 — `msadds32.ax` PE-load milestone reached.
//!
//! After r56 wired `msvcrt!_CIpow` — the MSVC compiler-intrinsic
//! `pow(double, double)` x87-stack helper — the audio splitter's
//! PE-load surface has **every named import resolved**.  This
//! reproducibility-check test pins the milestone so any future
//! regression that re-introduces an unresolved-import blocker is
//! caught loudly.
//!
//! The journey from r48..r56:
//!
//! | round | symbol               | shape                                      |
//! | ----- | -------------------- | ------------------------------------------ |
//! | r48   | `_endthreadex`       | cdecl, fail-soft no-op (unused on decode)  |
//! | r49   | `_strnicmp`          | cdecl, real ASCII tolower compare          |
//! | r50   | `_beginthreadex`     | cdecl, fail-soft return-0 (unused)         |
//! | r52   | `_ftol`              | x87 stack, real truncate-toward-zero       |
//! | r55   | `rand` / `srand`     | cdecl + seedable host API; real LCG        |
//! | r56   | `_CIpow`             | x87 stack, real IEEE 754 powf              |
//!
//! ## What this test does NOT do
//!
//! Drive `msadds32.ax` through `DLL_PROCESS_ATTACH` / `DriverProc`
//! / `DllGetClassObject`.  Those exercise a different surface
//! (`COM` factory + audio-pin negotiation) and are the next
//! critical-path target for actually DRIVING the audio decoder.
//! See the round-56 CHANGELOG entry for the post-load roadmap.
//!
//! Skipped gracefully if the DLL is not present in the docs tree.

use oxideav_vfw::Sandbox;
use std::path::PathBuf;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn msadds32_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/msadds32.ax");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

#[test]
fn msadds32_ax_pe_load_completes_cleanly() {
    let Some(p) = msadds32_path() else {
        eprintln!("round56: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    let img = sb
        .load("msadds32.ax", &bytes)
        .expect("round 56 milestone: msadds32.ax PE-load must complete cleanly after _CIpow");
    eprintln!(
        "round56: msadds32.ax FULLY PE-loaded — image_base={:#010x}, \
         entry_point={:#010x}, DllMain={:?}, DllGetClassObject={:?}",
        img.image_base,
        img.entry_point,
        img.export("DllMain"),
        img.export("DllGetClassObject"),
    );
}

#[test]
fn msadds32_ax_exports_dllgetclassobject() {
    // The audio splitter is a DirectShow filter — its sole COM
    // export is `DllGetClassObject`.  Pin that the post-r56 PE-load
    // surface continues to discover that symbol so the next round
    // can call into it.
    let Some(p) = msadds32_path() else {
        eprintln!("round56: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    let img = sb.load("msadds32.ax", &bytes).expect("PE-load");
    assert!(
        img.export("DllGetClassObject").is_some(),
        "msadds32.ax must export DllGetClassObject for the next-round DirectShow co-create"
    );
}
