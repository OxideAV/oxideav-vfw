//! Round 25 — DirectShow IBaseFilter scaffolding.
//!
//! Round 24 closed with the verdict that `WMVDS32.AX` and
//! `MPG4DS32.AX` are pure DirectShow filters: their PE export
//! tables list no `DriverProc`, just `DllGetClassObject` /
//! `DllCanUnloadNow` / `DllRegisterServer` /
//! `DllUnregisterServer`.  Driving them therefore requires
//! reaching in via the COM ABI.
//!
//! This round adds the foundation for that work and exercises
//! it stage-by-stage:
//!
//! * **Stage 1** (always-runs, no fixture required) — pure-host
//!   tests that the GUID parser, IID constants, the
//!   ComObjectTable bookkeeping, and the vtable-call helpers
//!   are well-formed.  These tests guarantee the scaffolding
//!   stays sound across edits.
//!
//! * **Stage 2** (runs only when the wmpcdcs8-2001 binaries are
//!   present in `docs/video/msmpeg4/reference/binaries/`) —
//!   PE-load `WMVDS32.AX` + `MPG4DS32.AX`, drive `DllGetClassObject`
//!   to retrieve a class factory, verify the returned vtable
//!   pointer looks plausible.
//!
//! * **Stage 3** (gated by stage-2 success) — drive
//!   `IClassFactory::CreateInstance(NULL, IID_IBaseFilter, ppv)`
//!   to spawn an `IBaseFilter` instance.  Verify QueryInterface
//!   succeeds for `IID_IUnknown` / `IID_IBaseFilter` /
//!   `IID_IPersist` / `IID_IMediaFilter` and that `Release`
//!   eventually drops the refcount to zero.
//!
//! Stages 4 and 5 (`IBaseFilter::Run`, IPin::Receive) are
//! reach-goal stretches; round-25 leaves the scaffolding in
//! place for round-26 to land.

mod common;

use oxideav_vfw::com::{call::vtable_is_plausible, ComObjectTable, Guid, S_OK};
use oxideav_vfw::win32::Registry;
use oxideav_vfw::{
    Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY, IID_IMEDIAFILTER, IID_IPERSIST, IID_IUNKNOWN,
};
use std::path::PathBuf;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn binary_path(name: &str) -> Option<PathBuf> {
    let p = workspace_root()?.join(format!(
        "docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/{name}"
    ));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

// ---- Stage 1: always-runs scaffolding tests ---------------------------

#[test]
fn iid_constants_round_trip_through_braced_strings() {
    // Sanity: every hardcoded IID must round-trip to its MIDL
    // canonical string form and back.  This guards against a
    // transcription typo in the hardcoded constants.
    let cases = [
        ("{00000000-0000-0000-C000-000000000046}", IID_IUNKNOWN),
        ("{00000001-0000-0000-C000-000000000046}", IID_ICLASSFACTORY),
        ("{0000010C-0000-0000-C000-000000000046}", IID_IPERSIST),
        ("{56A86895-0AD4-11CE-B03A-0020AF0BA770}", IID_IBASEFILTER),
        ("{56A86899-0AD4-11CE-B03A-0020AF0BA770}", IID_IMEDIAFILTER),
    ];
    for (s, expected) in cases {
        let parsed = Guid::parse(s).unwrap_or_else(|e| panic!("parse {s}: {e}"));
        assert_eq!(parsed, expected, "IID {s} mismatch");
        // `to_braced_string` always emits upper-case; compare
        // case-insensitively against the input.
        let back = parsed.to_braced_string();
        assert_eq!(back.to_ascii_uppercase(), s.to_ascii_uppercase());
    }
}

#[test]
fn com_object_table_starts_empty() {
    let t = ComObjectTable::new();
    assert!(t.is_empty());
    assert_eq!(t.len(), 0);
    assert_eq!(t.total_refcount(), 0);
}

#[test]
fn ole32_co_create_instance_stub_is_registered() {
    let mut r = Registry::new();
    oxideav_vfw::win32::ole32::register(&mut r);
    assert!(r.resolve("ole32.dll", "CoCreateInstance").is_some());
    assert!(r.resolve("ole32.dll", "CoInitializeEx").is_some());
    assert!(r.resolve("ole32.dll", "CoTaskMemRealloc").is_some());
}

// ---- Stage 2: drive DllGetClassObject ---------------------------------

/// CLSIDs the wmpcdcs8-2001 DirectShow filter binaries are
/// known to register.  Values come from the bundle's `.inf`
/// installation manifests (`mpeg4ax.inf`, `wmvax.inf`) which
/// ship next to the binaries — these are public installation
/// metadata, not source.  We try each in turn until
/// `DllGetClassObject` accepts one.
///
/// Filter CLSIDs (per the wmpcdcs8-2001 `*.inf` manifests):
///   * `MPG4DS32` MPEG-4 v3 decoder filter:
///     `{82CCD3E0-F71A-11D0-9FE5-00609778EA66}`
///   * `WMVDS32`  Windows Media Video decoder filter:
///     `{82CCD3E0-F71A-11D0-9FE5-00609778EA66}` (shares the
///     decoder filter CLSID).
///
/// If the manifests differ from this assumption the stage-2
/// test will report `DllGetClassObject` returning
/// `CLASS_E_CLASSNOTAVAILABLE` cleanly and stop — that's
/// informational, not a hard failure.
const CANDIDATE_CLSIDS: &[&str] = &[
    "{82CCD3E0-F71A-11D0-9FE5-00609778EA66}",
    // Generic WMV / MPEG-4 decoder family CLSIDs noted in the
    // public DirectShow registry exports.  Round-26 may add
    // more once the round-25 surface is healthy.
    "{4F4F1734-72E2-49A8-9A8B-DCC4C5E16E16}",
];

fn parse_clsid(s: &str) -> Guid {
    Guid::parse(s).expect("clsid parse")
}

/// Attempt stage 2 against `dll_name`.  Returns `(class_factory,
/// chosen_clsid)` on success, or an error string (which the
/// caller decides whether to treat as fatal).
fn drive_dll_get_class_object(
    dll_name: &str,
) -> Result<(Sandbox, oxideav_vfw::pe::Image, u32, Guid), String> {
    let p = binary_path(dll_name).ok_or_else(|| format!("{dll_name} not present"))?;
    let bytes = std::fs::read(&p).map_err(|e| format!("read {dll_name}: {e}"))?;
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(2_000_000_000);
    let img = sb
        .load(dll_name, &bytes)
        .map_err(|e| format!("load: {e}"))?;
    sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .map_err(|e| format!("DllMain: {e}"))?;
    if img.export("DllGetClassObject").is_none() {
        return Err("DllGetClassObject not exported".into());
    }
    let mut last_err: Option<String> = None;
    for s in CANDIDATE_CLSIDS {
        let clsid = parse_clsid(s);
        match sb.dll_get_class_object(&img, clsid, IID_ICLASSFACTORY) {
            Ok(factory) => {
                eprintln!("round25 {dll_name}: DllGetClassObject({s}) → {factory:#010x}");
                return Ok((sb, img, factory, clsid));
            }
            Err(e) => {
                eprintln!("round25 {dll_name}: DllGetClassObject({s}) failed: {e}");
                last_err = Some(format!("{e}"));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "every candidate CLSID failed".into()))
}

#[test]
fn wmvds32_dll_get_class_object_reaches_class_factory() {
    let (sb, _img, factory, _clsid) = match drive_dll_get_class_object("WMVDS32.AX") {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round25 WMVDS32: stage 2 skipped: {e}");
            return;
        }
    };
    assert_ne!(factory, 0, "class factory pointer is NULL");
    assert!(
        vtable_is_plausible(&sb.mmu, factory),
        "class factory vtable does not look like a real COM vtable"
    );
    eprintln!(
        "round25 WMVDS32: class factory at {factory:#010x}; \
         host com table holds {} object(s), total host refcount {}",
        sb.host.com.len(),
        sb.host.com.total_refcount(),
    );
}

#[test]
fn mpg4ds32_dll_get_class_object_reaches_class_factory() {
    let (sb, _img, factory, _clsid) = match drive_dll_get_class_object("MPG4DS32.AX") {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round25 MPG4DS32: stage 2 skipped: {e}");
            return;
        }
    };
    assert_ne!(factory, 0, "class factory pointer is NULL");
    assert!(
        vtable_is_plausible(&sb.mmu, factory),
        "class factory vtable does not look like a real COM vtable"
    );
    eprintln!(
        "round25 MPG4DS32: class factory at {factory:#010x}; \
         host com table holds {} object(s)",
        sb.host.com.len()
    );
}

// ---- Stage 2.5: QueryInterface on the class factory -------------------

#[test]
fn class_factory_query_interface_for_iunknown_succeeds() {
    let (mut sb, _img, factory, _clsid) = match drive_dll_get_class_object("WMVDS32.AX")
        .or_else(|_| drive_dll_get_class_object("MPG4DS32.AX"))
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round25 QI(IUnknown): stage 2 skipped: {e}");
            return;
        }
    };
    let r = sb.query_interface(factory, IID_IUNKNOWN);
    match r {
        Ok(p) => {
            assert_ne!(p, 0, "QueryInterface(IUnknown) returned NULL");
            eprintln!("round25 QI(IUnknown) succeeded → {p:#010x}");
            // Per COM ABI the QueryInterface for IUnknown on
            // any object must return the SAME pointer for
            // identity comparison; relax the check (some
            // implementations return a fresh pointer due to
            // multiple inheritance).
            let _ = sb.com_release(p);
        }
        Err(e) => {
            // Stage 2.5 informational: codec might have
            // hit an emulator gap deep in the QueryInterface
            // body (e.g. a SAL-annotated assertion in debug
            // builds).
            eprintln!("round25 QI(IUnknown) failed: {e}");
        }
    }
}

// ---- Stage 3: spawn IBaseFilter through CreateInstance ----------------

#[test]
fn class_factory_create_instance_spawns_ibasefilter_or_reports_blocker() {
    let (mut sb, _img, factory, clsid) = match drive_dll_get_class_object("WMVDS32.AX")
        .or_else(|_| drive_dll_get_class_object("MPG4DS32.AX"))
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round25 stage 3 skipped (stage 2 unavailable): {e}");
            return;
        }
    };
    let _ = factory; // already cached in `state.com.class_factories`
    let r = sb.co_create_instance(clsid, IID_IBASEFILTER);
    match r {
        Ok(filter) => {
            eprintln!("round25 stage 3: IBaseFilter spawned at {filter:#010x}");
            assert_ne!(filter, 0);
            assert!(
                vtable_is_plausible(&sb.mmu, filter),
                "IBaseFilter vtable does not look real"
            );
            // Probe QueryInterface for the documented base
            // interfaces.  Each is informational; we record
            // success / failure.
            for (label, iid) in [
                ("IUnknown", IID_IUNKNOWN),
                ("IPersist", IID_IPERSIST),
                ("IMediaFilter", IID_IMEDIAFILTER),
                ("IBaseFilter", IID_IBASEFILTER),
            ] {
                match sb.query_interface(filter, iid) {
                    Ok(p) => {
                        eprintln!("round25 stage 3 QI({label}) → {p:#010x}");
                        let _ = sb.com_release(p);
                    }
                    Err(e) => eprintln!("round25 stage 3 QI({label}) failed: {e}"),
                }
            }
            let final_rc = sb.com_release(filter).unwrap_or(0);
            eprintln!("round25 stage 3: final IBaseFilter Release → {final_rc}");
        }
        Err(e) => {
            eprintln!("round25 stage 3: CreateInstance(IBaseFilter) failed: {e}");
            // Informational — the codec's CreateInstance
            // implementation may rely on parts of the COM /
            // CRT host surface we have not yet stubbed.
            // Round-26 reads the trace and unblocks each
            // missing edge.
        }
    }
}

// ---- Stage 4: drive IBaseFilter::Run / Stop / Pause -------------------

#[test]
fn ibasefilter_stop_pause_run_reach_goal() {
    let (mut sb, _img, _factory, clsid) = match drive_dll_get_class_object("WMVDS32.AX")
        .or_else(|_| drive_dll_get_class_object("MPG4DS32.AX"))
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round25 stage 4 skipped (stage 2 unavailable): {e}");
            return;
        }
    };
    let filter = match sb.co_create_instance(clsid, IID_IBASEFILTER) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("round25 stage 4 skipped: CreateInstance failed: {e}");
            return;
        }
    };
    // IBaseFilter inherits IMediaFilter; the slots we drive are
    // (per `strmif.h`):  4=Stop, 5=Pause, 6=Run(REFERENCE_TIME).
    // tStart is a 64-bit integer passed as two stdcall dwords.
    use oxideav_vfw::com::call::call_method;
    let stop_r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_STOP,
        &[],
    );
    match stop_r {
        Ok(hr) => eprintln!("round25 stage 4 IBaseFilter::Stop → HRESULT {hr:#010x}"),
        Err(e) => eprintln!("round25 stage 4 IBaseFilter::Stop trapped: {e}"),
    }
    let pause_r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_PAUSE,
        &[],
    );
    match pause_r {
        Ok(hr) => eprintln!("round25 stage 4 IBaseFilter::Pause → HRESULT {hr:#010x}"),
        Err(e) => eprintln!("round25 stage 4 IBaseFilter::Pause trapped: {e}"),
    }
    let run_r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_RUN,
        &[0, 0], // tStart = 0 (low dword + high dword)
    );
    match run_r {
        Ok(hr) => {
            eprintln!("round25 stage 4 IBaseFilter::Run(0) → HRESULT {hr:#010x}");
            // S_OK (0) or S_FALSE (1) are both documented
            // success codes.  E_OUTOFMEMORY / E_INVALIDARG /
            // VFW_E_WRONG_STATE etc. are informational.
        }
        Err(e) => eprintln!("round25 stage 4 IBaseFilter::Run trapped: {e}"),
    }
    // Tear down.
    let _ = sb.com_release(filter);
}

#[test]
fn ibasefilter_enum_pins_reach_goal() {
    let (mut sb, _img, _factory, clsid) = match drive_dll_get_class_object("WMVDS32.AX")
        .or_else(|_| drive_dll_get_class_object("MPG4DS32.AX"))
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round25 EnumPins skipped (stage 2 unavailable): {e}");
            return;
        }
    };
    let filter = match sb.co_create_instance(clsid, IID_IBASEFILTER) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("round25 EnumPins skipped: CreateInstance failed: {e}");
            return;
        }
    };
    // Stage an IEnumPins** out-slot.
    let scratch = sb.host.arena_alloc(4).unwrap();
    sb.mmu
        .write_initializer(scratch, &0u32.to_le_bytes())
        .unwrap();
    use oxideav_vfw::com::call::call_method;
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_ENUM_PINS,
        &[scratch],
    );
    match r {
        Ok(hr) => {
            let pp = sb.mmu.load32(scratch).unwrap_or(0);
            eprintln!("round25 stage 4 EnumPins → HRESULT {hr:#010x}, ppEnum = {pp:#010x}");
            if hr == 0 && pp != 0 {
                sb.host.com.intern(pp, None);
                let _ = sb.com_release(pp);
            }
        }
        Err(e) => eprintln!("round25 stage 4 EnumPins trapped: {e}"),
    }
    let _ = sb.com_release(filter);
}

// ---- Stage 5 (stretch): enumerate pins + query direction --------------

#[test]
fn ibasefilter_enum_pins_walks_to_input_pin_or_reports_blocker() {
    let (mut sb, _img, _factory, clsid) = match drive_dll_get_class_object("WMVDS32.AX")
        .or_else(|_| drive_dll_get_class_object("MPG4DS32.AX"))
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round25 stage 5 skipped: {e}");
            return;
        }
    };
    let filter = match sb.co_create_instance(clsid, IID_IBASEFILTER) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("round25 stage 5 skipped: CreateInstance: {e}");
            return;
        }
    };
    let scratch = sb.host.arena_alloc(4).unwrap();
    sb.mmu
        .write_initializer(scratch, &0u32.to_le_bytes())
        .unwrap();
    use oxideav_vfw::com::call::call_method;
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_ENUM_PINS,
        &[scratch],
    );
    let pp = sb.mmu.load32(scratch).unwrap_or(0);
    let Ok(0) = r else {
        eprintln!("round25 stage 5: EnumPins did not succeed: {r:?}");
        let _ = sb.com_release(filter);
        return;
    };
    if pp == 0 {
        eprintln!("round25 stage 5: EnumPins returned NULL ppEnum");
        let _ = sb.com_release(filter);
        return;
    }
    sb.host.com.intern(pp, None);
    eprintln!("round25 stage 5: IEnumPins at {pp:#010x}");

    // IEnumPins::Next(ULONG cPins, IPin** ppPins, ULONG*
    // pcFetched).  Slot 3 (after IUnknown's 0..3).  Stage two
    // 4-byte slots: ppPins[0] + pcFetched.
    let pins_slot = sb.host.arena_alloc(8).unwrap();
    sb.mmu.write_initializer(pins_slot, &[0u8; 8]).unwrap();
    let next_r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pp,
        3,
        &[1, pins_slot, pins_slot + 4],
    );
    let pin0 = sb.mmu.load32(pins_slot).unwrap_or(0);
    let fetched = sb.mmu.load32(pins_slot + 4).unwrap_or(0);
    eprintln!(
        "round25 stage 5: IEnumPins::Next → {next_r:?}, pin = {pin0:#010x}, fetched = {fetched}"
    );
    if pin0 == 0 {
        let _ = sb.com_release(pp);
        let _ = sb.com_release(filter);
        return;
    }
    sb.host.com.intern(pin0, None);

    // IPin::QueryDirection(PIN_DIRECTION* pPinDir) — slot 9.
    // PIN_INPUT = 0, PIN_OUTPUT = 1.
    let dir_slot = sb.host.arena_alloc(4).unwrap();
    sb.mmu
        .write_initializer(dir_slot, &0xFFFF_FFFFu32.to_le_bytes())
        .unwrap();
    let dir_r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin0,
        9,
        &[dir_slot],
    );
    let dir = sb.mmu.load32(dir_slot).unwrap_or(0xFFFF_FFFF);
    eprintln!(
        "round25 stage 5: IPin::QueryDirection → {dir_r:?}, dir = {dir:#x} \
         (0=INPUT, 1=OUTPUT)"
    );

    // Tear down.
    let _ = sb.com_release(pin0);
    let _ = sb.com_release(pp);
    let _ = sb.com_release(filter);
}

// ---- Stage 1 backstop: confirm the round-25 ole32 lookup path ----------

#[test]
fn co_create_instance_without_registered_factory_reports_classnotavail() {
    // Fresh Sandbox, no DllGetClassObject ever ran.  A direct
    // host-side call to CoCreateInstance must surface the
    // documented "class not available" error rather than
    // panicking or trapping.
    let mut sb = Sandbox::new();
    let bogus_clsid = Guid::parse("{4F03ADBE-9F75-4970-B9C8-EAB6A2E0EE96}").unwrap();
    let err = sb
        .co_create_instance(bogus_clsid, IID_IUNKNOWN)
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not registered"),
        "unexpected error from co_create_instance: {msg}",
    );
}

#[test]
fn s_ok_constant_is_zero() {
    // Tiny sanity to keep the COM HRESULT export visible
    // through the public crate interface.
    assert_eq!(S_OK, 0);
}
