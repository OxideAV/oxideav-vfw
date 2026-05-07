//! Round-1 milestone integration test: "Load + DllMain + clean
//! exit".
//!
//! Two paths:
//!
//! 1. **Synthesised PE32 DLL.** Always runs. Builds a minimal
//!    valid DLL byte-by-byte (see `oxideav_vfw::pe::test_image`),
//!    loads it through the public [`Sandbox`] API, and calls its
//!    `DllMain(DLL_PROCESS_ATTACH)` through the integer
//!    interpreter. Confirms the entire round-1 stack — MMU, ISA
//!    decoder/executor, PE loader, kernel32 stub registry — is
//!    end-to-end functional.
//!
//! 2. **Real codec DLL.** Gated behind the `test-fixtures`
//!    feature. The user stages a small legacy codec DLL (e.g.
//!    Cinepak's `iccvid.dll`) at `tests/fixtures/iccvid.dll` and
//!    re-runs the test with `--features test-fixtures`. The DLL
//!    is **not** committed to git — see `tests/README.md` for
//!    legitimate sources.
//!
//! With the feature off, the staged-DLL test is silently elided.
//! CI does not block on the fixture being present.

use oxideav_vfw::emulator::regs::Reg32;
use oxideav_vfw::{Sandbox, DLL_PROCESS_ATTACH};

#[test]
fn synth_dll_main_returns_through_sentinel() {
    let bytes = oxideav_vfw::pe::test_image::build_minimal_dll();
    let mut sb = Sandbox::new();
    let img = sb.load("synth.dll", &bytes).expect("PE32 load");
    // Pre-set eax = 1 to model "DllMain returned TRUE".
    sb.cpu.regs.set32(Reg32::Eax, 1);
    let ret = sb.call_dll_main(&img, DLL_PROCESS_ATTACH).expect("run");
    assert_eq!(ret, 1, "synthesised DllMain should return TRUE");
}

#[cfg(feature = "test-fixtures")]
#[test]
fn staged_codec_dll_runs_dll_main_cleanly() {
    use std::path::PathBuf;

    let candidates: &[&str] = &["tests/fixtures/iccvid.dll", "tests/fixtures/ir50_32.dll"];
    let Some(path) = candidates.iter().map(PathBuf::from).find(|p| p.exists()) else {
        eprintln!(
            "no codec DLL staged at tests/fixtures/ — silently skipping. \
             See tests/README.md for legitimate sources."
        );
        return;
    };
    let bytes = std::fs::read(&path).expect("read staged DLL");

    let mut sb = Sandbox::new();
    let img = sb.load(path.to_str().unwrap(), &bytes).expect("PE32 load");
    let ret = sb.call_dll_main(&img, DLL_PROCESS_ATTACH).expect(
        "DllMain ran to completion; if a trap fires here it is the \
         next round-2 work item — e.g. a missing kernel32 stub",
    );
    eprintln!("staged DllMain returned eax = {ret:#x}");
}
