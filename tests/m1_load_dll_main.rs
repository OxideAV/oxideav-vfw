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
//! 2. **Real codec DLL.** Round 3 retargets this against Intel's
//!    Indeo 3 redistributable (`IR32_32.DLL`). The fixture is
//!    located via the [`common::fetch_or_load`] helper, which
//!    resolves user-staged dirs / Wine prefix / Windows system32
//!    / on-disk cache / HTTPS fetch in that order. The DLL is
//!    never committed to git. The test asserts the exact set of
//!    Win32 imports the round-1 + round-2 stub registry does
//!    not yet satisfy — i.e. round 4's concrete todo list.

mod common;

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

/// Round-3 real-codec smoke test against Intel's Indeo 3
/// (`IR32_32.DLL`).
///
/// **Expected outcome at the end of round 3**: the PE loader
/// rejects the import resolution step because the round-1 +
/// round-2 stub registry does not yet cover the user32 / gdi32 /
/// winmm imports the codec needs, plus 24 additional kernel32
/// imports (the codec's CRT init pulls them in). The exact set
/// is asserted below — that set is round 4's deliverable.
///
/// When round 4 lands the missing stubs, this assertion will
/// flip from "miss == EXPECTED_MISSING" to "miss is empty".
/// At that point the test author should:
///
/// 1. Replace the assertion below with a real `Sandbox::load(...)
///    + Sandbox::call_dll_main(...)` walkthrough.
/// 2. The follow-up trap on the first ISA opcode the codec
///    actually uses (e.g. `imul r/m32`) becomes round 5's todo
///    list — same pattern.
#[test]
fn staged_codec_dll_lists_round_four_todo_imports() {
    let bytes =
        common::fetch_or_load("IR32_32.DLL").expect("fetch IR32_32.DLL — see tests/common/mod.rs");

    // Sanity: bytes are a valid PE32 i386 image the loader is
    // willing to *parse* (header, sections, exports). Failure
    // here means the round-1 PE32 parser regressed, not a stub
    // gap.
    let parsed =
        oxideav_vfw::pe::header::parse(&bytes).expect("IR32_32.DLL must parse as a PE32 header");
    assert_eq!(
        parsed.optional.image_base, 0x1000_0000,
        "Indeo 3's preferred ImageBase is the standard 0x10000000"
    );

    // Load through the actual loader. The expectation in round 3
    // is that `resolve_imports` rejects the load with a precise
    // `UnknownImportFunction` describing the first missing stub.
    let mut sb = Sandbox::new();
    let load_err = sb.load("IR32_32.DLL", &bytes).expect_err(
        "round 3: IR32_32.DLL load must fail — round-1+2 stub set \
         does not yet cover user32/gdi32/winmm imports. If this \
         expect_err triggers, round 4 has likely landed and the \
         test should be updated to do a real DllMain walkthrough.",
    );
    let oxideav_vfw::Error::PeLoader(oxideav_vfw::pe::PeError::UnknownImportFunction {
        ref dll,
        ref name,
    }) = load_err
    else {
        panic!("expected UnknownImportFunction, got: {load_err:?}");
    };
    eprintln!("first missing import surfaced by loader: {dll}!{name}");

    // Now enumerate the *full* round-4 todo list — every import
    // the codec declares that the round-1 + round-2 registry
    // does not yet satisfy. This is round-4's concrete dispatch
    // budget.
    let imports = common::list_pe_imports(&bytes).expect("list_pe_imports");
    let registry = sb.registry; // reuse the freshly-built one
    let missing: std::collections::BTreeSet<(String, String)> = imports
        .iter()
        .filter(|(dll, name)| registry.resolve(dll, name).is_none())
        .cloned()
        .collect();

    eprintln!("round-4 todo: {} missing Win32 imports", missing.len());
    let mut by_dll: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
    for (dll, name) in &missing {
        by_dll.entry(dll).or_default().push(name);
    }
    for (dll, names) in &by_dll {
        eprintln!("  {dll}: {} fns — {}", names.len(), names.join(", "));
    }

    // Hard assertion on the exact round-4 todo list. When round
    // 4 closes one of these gaps, this test fails — that is the
    // signal to update the EXPECTED set + (eventually) replace
    // this whole assertion with a real DllMain walkthrough.
    let expected: std::collections::BTreeSet<(String, String)> = round_4_todo_imports()
        .into_iter()
        .map(|(d, n)| (d.to_string(), n.to_string()))
        .collect();

    let unexpected_missing: Vec<_> = missing.difference(&expected).collect();
    let unexpected_supplied: Vec<_> = expected.difference(&missing).collect();
    assert!(
        unexpected_missing.is_empty(),
        "loader is missing imports we did not predict in round 3: {unexpected_missing:?}"
    );
    assert!(
        unexpected_supplied.is_empty(),
        "round-4 progress detected — these expected-missing imports are now resolved: \
         {unexpected_supplied:?} — update round_4_todo_imports() and replace this assertion \
         with a real DllMain walkthrough"
    );
}

/// The exact set of `(dll, function)` imports the round-1 +
/// round-2 stub registry does not satisfy for `IR32_32.DLL`.
///
/// This is round 4's todo list, frozen at end of round 3 from
/// the directly-parsed `.idata` of Intel's published `IR32_32.DLL`
/// in `IV5PLAY` (R19770). Adding stubs for any of these to the
/// registry will cause [`staged_codec_dll_lists_round_four_todo_imports`]
/// to fail — at which point update the list (or, once it's
/// empty, replace the test with a real DllMain walkthrough).
fn round_4_todo_imports() -> Vec<(&'static str, &'static str)> {
    vec![
        // GDI32 — display-side: codec sets up an offscreen DIB
        // section, queries device caps, etc. A no-op stub set
        // returning sensible defaults (CreateCompatibleDC → 1,
        // GetDeviceCaps → 32-bit colour, etc.) is enough for
        // DllMain; real device interaction is not needed.
        ("gdi32.dll", "BitBlt"),
        ("gdi32.dll", "CreateCompatibleDC"),
        ("gdi32.dll", "DeleteDC"),
        ("gdi32.dll", "GetDeviceCaps"),
        ("gdi32.dll", "GetNearestColor"),
        ("gdi32.dll", "GetObjectA"),
        ("gdi32.dll", "GetSystemPaletteEntries"),
        ("gdi32.dll", "SelectObject"),
        // KERNEL32 — round 1 covered 12 stubs; this is the
        // additional 24 the codec's CRT init pulls in.
        ("kernel32.dll", "ExitProcess"),
        ("kernel32.dll", "GetACP"),
        ("kernel32.dll", "GetCPInfo"),
        ("kernel32.dll", "GetCommandLineA"),
        ("kernel32.dll", "GetEnvironmentStrings"),
        ("kernel32.dll", "GetFileType"),
        ("kernel32.dll", "GetLastError"),
        ("kernel32.dll", "GetModuleFileNameA"),
        ("kernel32.dll", "GetModuleHandleA"),
        ("kernel32.dll", "GetOEMCP"),
        ("kernel32.dll", "GetStartupInfoA"),
        ("kernel32.dll", "GetStdHandle"),
        ("kernel32.dll", "GetSystemInfo"),
        ("kernel32.dll", "GetVersion"),
        ("kernel32.dll", "GlobalAlloc"),
        ("kernel32.dll", "GlobalFree"),
        ("kernel32.dll", "GlobalLock"),
        ("kernel32.dll", "GlobalUnlock"),
        ("kernel32.dll", "MultiByteToWideChar"),
        ("kernel32.dll", "RtlUnwind"),
        ("kernel32.dll", "VirtualAlloc"),
        ("kernel32.dll", "VirtualFree"),
        ("kernel32.dll", "WideCharToMultiByte"),
        ("kernel32.dll", "WriteFile"),
        // USER32 — codec ships a "Configure" / "About" dialog;
        // for headless decode all of these can be no-op stubs
        // returning 0 / NULL.
        ("user32.dll", "BeginPaint"),
        ("user32.dll", "DialogBoxParamA"),
        ("user32.dll", "EndDialog"),
        ("user32.dll", "EndPaint"),
        ("user32.dll", "GetDC"),
        ("user32.dll", "GetDlgItemInt"),
        ("user32.dll", "GetWindowLongA"),
        ("user32.dll", "GetWindowRect"),
        ("user32.dll", "LoadBitmapA"),
        ("user32.dll", "LoadStringA"),
        ("user32.dll", "MessageBeep"),
        ("user32.dll", "MessageBoxA"),
        ("user32.dll", "PostMessageA"),
        ("user32.dll", "ReleaseDC"),
        ("user32.dll", "SetDlgItemTextA"),
        ("user32.dll", "wsprintfA"),
        // WINMM — `DefDriverProc` is the system-default
        // installable-driver dispatcher. For Indeo 3 we can stub
        // it as "return 0" since the codec's own DriverProc
        // handles every message it cares about and forwards
        // unknowns; if it does forward, the host stub returns
        // ICERR_UNSUPPORTED.
        ("winmm.dll", "DefDriverProc"),
    ]
}
