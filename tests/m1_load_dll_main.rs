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

/// Round-4 real-codec import-coverage assertion against Intel's
/// Indeo 3 (`IR32_32.DLL`).
///
/// **Round 3** asserted the exact 49-entry set of imports the
/// stub registry could not satisfy; **round 4** lands those
/// stubs, and this test now asserts the registry covers
/// **every** import the DLL declares.
///
/// If this fails ("expected zero unresolved, got N"), a future
/// round of the bundle has been pulled with a different DLL
/// version that imports something new — diagnostic output
/// (eprintln above the panic) lists the offending names so the
/// next round's todo list is concrete.
#[test]
fn staged_codec_dll_resolves_every_import() {
    let bytes =
        common::fetch_or_load("IR32_32.DLL").expect("fetch IR32_32.DLL — see tests/common/mod.rs");

    // Sanity: bytes are a valid PE32 i386 image the loader is
    // willing to *parse* (header, sections, exports). Failure
    // here means the round-1 PE32 parser regressed.
    let parsed =
        oxideav_vfw::pe::header::parse(&bytes).expect("IR32_32.DLL must parse as a PE32 header");
    assert_eq!(
        parsed.optional.image_base, 0x1000_0000,
        "Indeo 3's preferred ImageBase is the standard 0x10000000"
    );

    // Build a Sandbox so the registry mirrors the production
    // stub set, then enumerate the DLL's imports against it.
    let sb = Sandbox::new();
    let imports = common::list_pe_imports(&bytes).expect("list_pe_imports");
    let missing: std::collections::BTreeSet<(String, String)> = imports
        .iter()
        .filter(|(dll, name)| sb.registry.resolve(dll, name).is_none())
        .cloned()
        .collect();

    if !missing.is_empty() {
        let mut by_dll: std::collections::BTreeMap<&str, Vec<&str>> =
            std::collections::BTreeMap::new();
        for (dll, name) in &missing {
            by_dll.entry(dll).or_default().push(name);
        }
        for (dll, names) in &by_dll {
            eprintln!(
                "next-round todo: {dll}: {} fns — {}",
                names.len(),
                names.join(", ")
            );
        }
        panic!(
            "round 4 expected every IR32_32.DLL import to resolve, got {} missing",
            missing.len()
        );
    }
}
