//! Round 57 — drive `msadds32.ax` through `DllGetClassObject` /
//! `IClassFactory::CreateInstance` for the **audio splitter** filter.
//!
//! Round 56 completed `msadds32.ax`'s PE-load surface: every named
//! import resolved, `image_base = 0x1c40_0000`, entry point
//! `0x1c40_233d`, `DllGetClassObject` exported.  Round 57 takes the
//! next step on the audio path — the SAME shape as round-25 took
//! for the video splitter (`mpg4ds32.ax`), against the
//! splitter-internal CLSID registration table.
//!
//! ## How the audio splitter's CLSIDs were discovered
//!
//! Reverse-engineering the audio splitter's `DllGetClassObject`
//! prologue at RVA `0x3635` (clean-room from raw bytes; no
//! Wine / ReactOS / MinGW source consulted) revealed:
//!
//! ```text
//!     push   ebp
//!     mov    ebp, esp
//!     ...
//!     mov    edi, 0x1c40fd38      ; pointer to IID_IUnknown
//!     ...      repe cmpsd          ; compare *rclsid == IID_IUnknown
//!     ...
//!     mov    edi, 0x1c40fd28      ; pointer to IID_IClassFactory
//!     ...      repe cmpsd          ; compare *rclsid == IID_IClassFactory
//!     ...
//!     mov    eax, [0x1c411028]    ; CLSID-table count word
//!     mov    ebx, 0x1c411000      ; CLSID-table base
//!     ...      loop over 20-byte entries comparing *ppv->ClsID
//! ```
//!
//! Walking the table at `RVA 0x11000` (count = 2, stride = 20):
//!
//! | entry | name-ptr        | clsid-ptr | factory thunk |
//! | ----- | --------------- | --------- | ------------- |
//! |   0   | "Windows Media Audio Decoder" | `0xf248` | `0x1c4011b1` |
//! |   1   | "Microsoft MS Audio Decompressor Control Property page" | `0xf298` | `0x1c40269f` |
//!
//! Reading the GUIDs at those `.rdata` offsets:
//!
//! * `0xf248` = **`{22E24591-49D0-11D2-BB50-006008320064}`** —
//!   `CLSID_AudioDecoderFilter` (the audio splitter itself).  Data4
//!   suffix `00 60 08 32 00 64` is in the MS WMA audio family.
//! * `0xf298` = `{8FE7E181-BB96-11D2-A1CB-00609778EA66}` — the
//!   audio decoder's property-page (UI vestige, not used on the
//!   decode path).
//!
//! Both are documented as constants in `crate::com` for stable
//! reuse.
//!
//! ## What this test pins
//!
//! * `Sandbox::dll_get_class_object(img,
//!   MSADDS_AUDIO_DECODER_CLSID, IID_ICLASSFACTORY)` returns
//!   `Ok(factory)` with a non-NULL, plausibly-shaped vtable
//!   pointer.
//! * `Sandbox::co_create_instance(MSADDS_AUDIO_DECODER_CLSID,
//!   IID_IUNKNOWN)` returns an `IUnknown` pointer.
//! * Stretch (informational, not asserted): `query_interface` for
//!   `IID_IBaseFilter` / `IID_IPersist` / `IID_IMediaFilter`.
//!
//! Skipped gracefully if `msadds32.ax` is not present in
//! `docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/`.

use oxideav_vfw::com::{
    call::vtable_is_plausible, MSADDS_AUDIO_DECODER_CLSID, MSADDS_AUDIO_PROPERTY_PAGE_CLSID,
};
use oxideav_vfw::{
    Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY, IID_IMEDIAFILTER, IID_IPERSIST, IID_IUNKNOWN,
};
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

/// Shared boilerplate: load `msadds32.ax`, run DllMain
/// (defensive — the splitter has no DllMain export but
/// `call_dll_main` no-ops cleanly when absent).
fn load() -> Option<(Sandbox, oxideav_vfw::pe::Image)> {
    let p = msadds32_path()?;
    let bytes = std::fs::read(&p).ok()?;
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(2_000_000_000);
    let img = sb.load("msadds32.ax", &bytes).ok()?;
    // The audio splitter does not export DllMain, so this is a
    // no-op call but kept for symmetry with the round-25 video
    // path which does.
    let _ = sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH);
    Some((sb, img))
}

// ---- Phase 1: pin the discovered CLSIDs ---------------------------

#[test]
fn audio_decoder_clsid_constant_matches_braced_form() {
    // The braced form is the one transcribed from the splitter's
    // .rdata at RVA 0xf248.  Pinning the round-trip catches any
    // future typo in the constant.
    assert_eq!(
        MSADDS_AUDIO_DECODER_CLSID.to_braced_string(),
        "{22E24591-49D0-11D2-BB50-006008320064}"
    );
}

#[test]
fn audio_property_page_clsid_constant_matches_braced_form() {
    assert_eq!(
        MSADDS_AUDIO_PROPERTY_PAGE_CLSID.to_braced_string(),
        "{8FE7E181-BB96-11D2-A1CB-00609778EA66}"
    );
}

// ---- Phase 2: drive DllGetClassObject for the audio decoder -------

#[test]
fn msadds32_dll_get_class_object_reaches_class_factory() {
    let Some((mut sb, img)) = load() else {
        eprintln!("round57: msadds32.ax missing; skipping");
        return;
    };
    assert!(
        img.export("DllGetClassObject").is_some(),
        "msadds32.ax must export DllGetClassObject"
    );
    let factory = sb
        .dll_get_class_object(&img, MSADDS_AUDIO_DECODER_CLSID, IID_ICLASSFACTORY)
        .expect(
            "DllGetClassObject(MSADDS_AUDIO_DECODER_CLSID, IID_IClassFactory) \
             should return S_OK + a real class factory",
        );
    assert_ne!(factory, 0, "class factory pointer is NULL");
    assert!(
        vtable_is_plausible(&sb.mmu, factory),
        "class factory vtable at {factory:#010x} does not look like a real COM vtable"
    );
    eprintln!(
        "round57: msadds32.ax DllGetClassObject({{22E24591-49D0-11D2-BB50-006008320064}}) \
         → factory {factory:#010x}; host com table holds {} object(s)",
        sb.host.com.len(),
    );
}

#[test]
fn msadds32_property_page_class_factory_also_reachable() {
    // Stretch: the audio splitter has TWO registered classes.
    // Pinning that the property-page CLSID also resolves cleanly
    // (even though we won't drive its UI methods).
    let Some((mut sb, img)) = load() else {
        eprintln!("round57: msadds32.ax missing; skipping");
        return;
    };
    let factory =
        sb.dll_get_class_object(&img, MSADDS_AUDIO_PROPERTY_PAGE_CLSID, IID_ICLASSFACTORY);
    match factory {
        Ok(p) if p != 0 => {
            assert!(
                vtable_is_plausible(&sb.mmu, p),
                "property-page factory vtable does not look real"
            );
            eprintln!("round57: property-page factory @ {p:#010x}");
        }
        Ok(p) => panic!("DllGetClassObject succeeded but returned NULL ppv: {p:#010x}"),
        Err(e) => {
            // Informational; not a hard fail.  Some splitters
            // gate the property-page CLSID behind a registry
            // probe.
            eprintln!("round57: property-page CLSID DllGetClassObject failed: {e}");
        }
    }
}

#[test]
fn msadds32_bogus_clsid_returns_class_not_available() {
    // Drives the same `DllGetClassObject` entry point with a
    // CLSID neither in the splitter's registration table; the
    // splitter must surface `CLASS_E_CLASSNOTAVAILABLE` rather
    // than crash.  Pins the negative path stays well-behaved.
    let Some((mut sb, img)) = load() else {
        eprintln!("round57: msadds32.ax missing; skipping");
        return;
    };
    let bogus = oxideav_vfw::com::Guid::parse("{4F03ADBE-9F75-4970-B9C8-EAB6A2E0EE96}").unwrap();
    let r = sb.dll_get_class_object(&img, bogus, IID_ICLASSFACTORY);
    match r {
        Ok(p) => panic!("bogus CLSID unexpectedly returned ppv = {p:#010x}"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("HRESULT") || msg.contains("0x80040154"),
                "expected REGDB_E_CLASSNOTREG / CLASS_E_CLASSNOTAVAILABLE, got: {msg}"
            );
        }
    }
}

// ---- Phase 3: drive IClassFactory::CreateInstance(IUnknown) -------

#[test]
fn msadds32_class_factory_create_instance_iunknown() {
    // The COM ABI guarantees that every successful
    // `CreateInstance` chains to an IUnknown that we can then
    // QueryInterface from for the richer interfaces.  Pins the
    // minimum-IID-spawn path lands cleanly.
    let Some((mut sb, img)) = load() else {
        eprintln!("round57: msadds32.ax missing; skipping");
        return;
    };
    // Discover + register the class factory first.
    let _factory = sb
        .dll_get_class_object(&img, MSADDS_AUDIO_DECODER_CLSID, IID_ICLASSFACTORY)
        .expect("DllGetClassObject must succeed for the audio decoder CLSID");
    // Drive CoCreateInstance equivalence: CreateInstance(NULL,
    // IID_IUnknown, &ppv).  Minimum IID = strongest guarantee
    // it'll go through.
    let r = sb.co_create_instance(MSADDS_AUDIO_DECODER_CLSID, IID_IUNKNOWN);
    match r {
        Ok(unk) => {
            assert_ne!(unk, 0, "IUnknown pointer is NULL");
            assert!(
                vtable_is_plausible(&sb.mmu, unk),
                "IUnknown vtable at {unk:#010x} does not look real"
            );
            eprintln!("round57: msadds32 IUnknown instance @ {unk:#010x}");
        }
        Err(e) => {
            // Surfacing the HRESULT here gives r58 a concrete
            // blocker name to chase.
            panic!("CreateInstance(IID_IUnknown) failed: {e}");
        }
    }
}

// ---- Phase 4 (stretch): QI for IBaseFilter / IPersist / IMediaFilter

#[test]
fn msadds32_query_interface_probes_inform_round58() {
    let Some((mut sb, img)) = load() else {
        eprintln!("round57: msadds32.ax missing; skipping");
        return;
    };
    let _factory = sb
        .dll_get_class_object(&img, MSADDS_AUDIO_DECODER_CLSID, IID_ICLASSFACTORY)
        .expect("DllGetClassObject must succeed");
    let Ok(unk) = sb.co_create_instance(MSADDS_AUDIO_DECODER_CLSID, IID_IUNKNOWN) else {
        eprintln!("round57: CreateInstance(IUnknown) blocked; skipping QI probes");
        return;
    };
    // Probe each interface; record success / failure but do not
    // fail the test on E_NOINTERFACE — that's actually useful
    // information for round-58 planning.
    for (label, iid) in [
        ("IUnknown", IID_IUNKNOWN),
        ("IPersist", IID_IPERSIST),
        ("IMediaFilter", IID_IMEDIAFILTER),
        ("IBaseFilter", IID_IBASEFILTER),
    ] {
        match sb.query_interface(unk, iid) {
            Ok(p) if p != 0 => {
                eprintln!("round57 QI({label}) → {p:#010x}");
                assert!(
                    vtable_is_plausible(&sb.mmu, p),
                    "QI({label}) returned plausibly-bogus pointer"
                );
            }
            Ok(p) => eprintln!("round57 QI({label}) → S_OK but NULL ppv ({p:#010x})"),
            Err(e) => eprintln!("round57 QI({label}) failed: {e}"),
        }
    }
}
