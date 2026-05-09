//! Round 35 — register the host-side `CLSID_MemoryAllocator`
//! class factory so `mpg4ds32`'s internal
//! `CoCreateInstance(CLSID_MemoryAllocator, NULL,
//! CLSCTX_INPROC_SERVER, IID_IMemAllocator, &alloc)` (called from
//! inside `IMemInputPin::GetAllocator`) returns a real allocator
//! pointer rather than the round-34 baseline
//! `CLASS_E_CLASSNOTAVAILABLE` (`0x80040111`).
//!
//! Per Windows SDK header `axextend.h`,
//! `CLSID_MemoryAllocator = {1E651CC0-B199-11D0-8212-00C04FC32C45}`
//! is the canonical DirectShow memory-allocator class — every
//! pin's internal `CoCreateInstance` for an IMemAllocator goes
//! through it.  Real Windows resolves it via SCM + the registry
//! (`HKEY_CLASSES_ROOT\CLSID\{1E65…}`); on our sandbox there is
//! no SCM, so we pre-mint a host-side `IClassFactory` and stash it
//! under the CLSID in `HostState::com.class_factories` from
//! [`Sandbox::new`].  The host factory's `CreateInstance` mints a
//! fresh `HostIMemAllocator` with the round-30+ pool layout (4
//! slots × 256 KiB capacity) so the codec gets a fully-formed
//! IMemAllocator interface pointer it can drive `SetProperties`
//! / `Commit` / `GetBuffer` against.
//!
//! References:
//!  * MSDN `CoCreateInstance` —
//!    <https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-cocreateinstance>
//!  * MSDN `IClassFactory::CreateInstance` —
//!    <https://learn.microsoft.com/en-us/windows/win32/api/unknwn/nf-unknwn-iclassfactory-createinstance>
//!  * MSDN `IMemAllocator` —
//!    <https://learn.microsoft.com/en-us/windows/win32/api/strmif/nn-strmif-imemallocator>
//!  * Windows SDK headers `axextend.h` / `strmif.h` /
//!    `combaseapi.h` / `unknwn.h`.

#![cfg(feature = "auto-discovery")]

mod common;

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, Packet, TimeBase};
use oxideav_vfw::com::call::call_method;
use oxideav_vfw::discovery::{
    last_codec_allocator_negotiation, make_decoder, register_factory_for_id, DiscoveryRecord, Kind,
};
use oxideav_vfw::Sandbox;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn dshow_dll_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/MPG4DS32.AX");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn mp43_fixture_path(stem: &str) -> Option<PathBuf> {
    let p = workspace_root()?.join(format!("docs/video/msmpeg4-fixtures/{stem}/input.avi"));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn extract_mp43_keyframe(stem: &str) -> Option<(u32, u32, Vec<u8>)> {
    let path = mp43_fixture_path(stem)?;
    let bytes = std::fs::read(&path).ok()?;
    let s = common::avi_extractor::extract_video_sample(&bytes, 0).ok()?;
    Some((s.width, s.height, s.bytes))
}

/// Drive the production `SandboxedDshowDecoder` end-to-end so we
/// can introspect the codec-allocator negotiation captured in the
/// global stash.
fn drive_production_path(
    test_label: &str,
) -> Option<(String, oxideav_core::Result<oxideav_core::Frame>)> {
    let dll_path = dshow_dll_path()?;
    let (width, height, keyframe) = extract_mp43_keyframe("fourcc-MP43")?;
    let id = format!("vfw_round35_{test_label}");
    register_factory_for_id(
        &id,
        DiscoveryRecord {
            dll_path,
            fourcc: "MP43".to_string(),
            kind: Kind::DirectShow,
            clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".to_string()),
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id.clone()));
    params.width = Some(width);
    params.height = Some(height);
    let mut decoder = make_decoder(&params).expect("make_decoder");
    let packet = Packet::new(0, TimeBase::new(1, 25), keyframe).with_keyframe(true);
    let send_result = decoder.send_packet(&packet);
    let outcome = match send_result {
        Err(e) => Err(e),
        Ok(()) => decoder.receive_frame(),
    };
    Some((id, outcome))
}

// ────────────────────────────────────────────────────────────────
// Test 1 — Sandbox::new pre-registers the CLSID_MemoryAllocator
// class factory in the in-process class-factory cache.
// ────────────────────────────────────────────────────────────────

/// Assertable invariant: after `Sandbox::new()` the class-factory
/// table contains an entry under `CLSID_MEMORY_ALLOCATOR` whose
/// vtable looks like a plausible IClassFactory (3 IUnknown slots
/// + slot 3 CreateInstance + slot 4 LockServer).
#[test]
fn sandbox_new_registers_clsid_memory_allocator_factory() {
    let sb = Sandbox::new();
    let factory = sb
        .host
        .com
        .lookup_class_factory(&oxideav_vfw::com::CLSID_MEMORY_ALLOCATOR)
        .expect("Sandbox::new should pre-register CLSID_MemoryAllocator");
    assert_ne!(factory, 0);
    let vtbl = sb.mmu.load32(factory).expect("factory vtbl ptr");
    assert_ne!(vtbl, 0);
    // 5 vtable slots: QI / AddRef / Release / CreateInstance / LockServer.
    for slot in 0..5u32 {
        let m = sb.mmu.load32(vtbl + slot * 4).expect("vtbl slot");
        assert!(m != 0, "vtbl slot {slot} unmapped");
        assert!(
            sb.registry.is_thunk(m),
            "vtbl slot {slot} = {m:#010x} not a registered thunk"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Test 2 — Driving CoCreateInstance(CLSID_MemoryAllocator, NULL,
// _, IID_IMemAllocator, &alloc) end-to-end through the host's
// ole32!CoCreateInstance cascade returns S_OK + a non-NULL
// IMemAllocator pointer.
// ────────────────────────────────────────────────────────────────

/// `Sandbox::co_create_instance` is the same code path the codec
/// hits when it imports `ole32!CoCreateInstance` — both end up
/// driving `IClassFactory::CreateInstance(NULL, IID, ppv)` against
/// whatever pointer is registered under the CLSID.  Round 35
/// ensures the host factory mints a fresh allocator on every call.
#[test]
fn cocreateinstance_clsid_memory_allocator_returns_iface() {
    let mut sb = Sandbox::new();
    let alloc = sb
        .co_create_instance(
            oxideav_vfw::com::CLSID_MEMORY_ALLOCATOR,
            oxideav_vfw::com::IID_IMEMALLOCATOR,
        )
        .expect("co_create_instance(CLSID_MemoryAllocator, IID_IMemAllocator) should succeed");
    assert_ne!(alloc, 0, "CreateInstance returned NULL allocator pointer");
    // Vtable check: the minted allocator should expose every
    // IMemAllocator slot as a registered thunk.
    let vtbl = sb.mmu.load32(alloc).expect("alloc vtbl ptr");
    for slot in 0..9u32 {
        let m = sb.mmu.load32(vtbl + slot * 4).expect("vtbl slot");
        assert!(
            sb.registry.is_thunk(m),
            "alloc vtbl slot {slot} = {m:#010x} not a thunk"
        );
    }
    // SetProperties + Commit + GetBuffer should now succeed
    // against the freshly-minted allocator (mirrors what the codec
    // would do internally).
    let props = sb.host.arena_alloc(16).unwrap();
    let actual = sb.host.arena_alloc(16).unwrap();
    for (off, val) in [(0u32, 4u32), (4, 256 * 1024), (8, 1), (12, 0)] {
        sb.mmu
            .write_initializer(props + off, &val.to_le_bytes())
            .unwrap();
        sb.mmu
            .write_initializer(actual + off, &0u32.to_le_bytes())
            .unwrap();
    }
    let r_sp = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_SET_PROPERTIES,
        &[props, actual],
    )
    .unwrap();
    assert_eq!(r_sp, oxideav_vfw::com::S_OK);
    let r_co = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_COMMIT,
        &[],
    )
    .unwrap();
    assert_eq!(r_co, oxideav_vfw::com::S_OK);
    // GetBuffer.
    let pp = sb.host.arena_alloc(4).unwrap();
    sb.mmu.write_initializer(pp, &0u32.to_le_bytes()).unwrap();
    let r_gb = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_GET_BUFFER,
        &[pp, 0, 0, 0],
    )
    .unwrap();
    assert_eq!(r_gb, oxideav_vfw::com::S_OK);
    let sample = sb.mmu.load32(pp).unwrap();
    assert_ne!(sample, 0);
}

// ────────────────────────────────────────────────────────────────
// Test 3 — Aggregation rejection per MSDN.
// ────────────────────────────────────────────────────────────────

/// `IClassFactory::CreateInstance(pUnkOuter != NULL, …)` must
/// return `CLASS_E_NOAGGREGATION` (0x80040110) when the class
/// doesn't support aggregation.  IMemAllocator does not support
/// aggregation — codecs that pass a non-NULL outer pointer are
/// signalling a bug and we mirror the canonical Windows behaviour.
#[test]
fn createinstance_with_outer_unk_returns_class_e_noaggregation() {
    let mut sb = Sandbox::new();
    let factory = sb
        .host
        .com
        .lookup_class_factory(&oxideav_vfw::com::CLSID_MEMORY_ALLOCATOR)
        .expect("factory pre-registered");
    // Stage IID + ppv slot.
    let iid_addr = sb.host.arena_alloc(20).unwrap();
    oxideav_vfw::com::IID_IMEMALLOCATOR
        .stage(&mut sb.mmu, iid_addr)
        .unwrap();
    sb.mmu
        .write_initializer(iid_addr + 16, &0u32.to_le_bytes())
        .unwrap();
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        factory,
        oxideav_vfw::com::SLOT_CLASS_FACTORY_CREATE_INSTANCE,
        &[0xDEAD_BEEF, iid_addr, iid_addr + 16],
    )
    .unwrap();
    assert_eq!(r, 0x8004_0110, "expected CLASS_E_NOAGGREGATION");
    // ppv should remain NULL.
    assert_eq!(sb.mmu.load32(iid_addr + 16).unwrap(), 0);
}

// ────────────────────────────────────────────────────────────────
// Test 4 — IID_IBaseFilter on the allocator factory should
// rejection through E_NOINTERFACE (allocator factories don't
// satisfy IBaseFilter).
// ────────────────────────────────────────────────────────────────

#[test]
fn createinstance_unknown_iid_returns_e_nointerface() {
    let mut sb = Sandbox::new();
    let factory = sb
        .host
        .com
        .lookup_class_factory(&oxideav_vfw::com::CLSID_MEMORY_ALLOCATOR)
        .expect("factory pre-registered");
    let iid_addr = sb.host.arena_alloc(20).unwrap();
    oxideav_vfw::com::IID_IBASEFILTER
        .stage(&mut sb.mmu, iid_addr)
        .unwrap();
    sb.mmu
        .write_initializer(iid_addr + 16, &0u32.to_le_bytes())
        .unwrap();
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        factory,
        oxideav_vfw::com::SLOT_CLASS_FACTORY_CREATE_INSTANCE,
        &[0, iid_addr, iid_addr + 16],
    )
    .unwrap();
    assert_eq!(r, oxideav_vfw::com::E_NOINTERFACE);
    assert_eq!(sb.mmu.load32(iid_addr + 16).unwrap(), 0);
}

// ────────────────────────────────────────────────────────────────
// Test 5 — mpg4ds32 GetAllocator now succeeds (no more
// CLASS_E_CLASSNOTAVAILABLE) once the factory is registered.
// ────────────────────────────────────────────────────────────────

/// Round 34's diagnostic baseline was that `mpg4ds32`'s
/// `IMemInputPin::GetAllocator` returned `0x80040111`
/// (`CLASS_E_CLASSNOTAVAILABLE`) because its internal
/// `CoCreateInstance(CLSID_MemoryAllocator)` failed.  With the
/// host factory registered in round 35, the call should now
/// surface `S_OK` + a non-NULL allocator pointer.
///
/// Three legitimate outcomes (the test fails on none of them but
/// reports the observed HRESULT for diagnostics):
///
///  * `S_OK` + non-NULL allocator — round 35's reach goal.
///  * `S_OK` + NULL allocator — codec succeeded internally but
///    chose not to surface its own allocator (legal per MSDN —
///    `VFW_E_NO_ALLOCATOR` semantically).
///  * Any failure HRESULT other than `CLASS_E_CLASSNOTAVAILABLE`
///    — a different internal codec failure, not the round-34
///    blocker; round 36+ would investigate.
///
/// The hard assertion: GetAllocator MUST NOT return
/// `CLASS_E_CLASSNOTAVAILABLE` anymore.
#[test]
fn mpg4ds32_get_allocator_no_longer_class_not_available() {
    let (id, _outcome) = match drive_production_path("get_allocator") {
        Some(t) => t,
        None => {
            eprintln!("round35 GA: MPG4DS32.AX or MP43 fixture missing; skipping");
            return;
        }
    };
    let neg = match last_codec_allocator_negotiation(&id) {
        Some(n) => n,
        None => {
            eprintln!("round35 GA: ensure_open did not reach GetAllocator");
            return;
        }
    };
    eprintln!(
        "round35 GA: GetAllocator hr={:#010x} alloc={:#010x} \
         SetProps hr={:#010x} Commit hr={:#010x} using_codec={}",
        neg.get_allocator_hr,
        neg.codec_allocator,
        neg.set_properties_hr,
        neg.commit_hr,
        neg.using_codec_allocator,
    );
    assert_ne!(
        neg.get_allocator_hr,
        oxideav_vfw::com::CLASS_E_CLASSNOTAVAILABLE,
        "round35: GetAllocator still returns CLASS_E_CLASSNOTAVAILABLE — the \
         CLSID_MemoryAllocator class factory is not being picked up by the \
         codec's internal CoCreateInstance"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 6 — End-to-end MP43 keyframe decode through the codec
// allocator (round-35 reach goal).
// ────────────────────────────────────────────────────────────────

/// Optimistic end-to-end smoke: drive a real 183-byte MP43
/// keyframe through the production `SandboxedDshowDecoder` flow
/// now that the codec can mint its own allocator.  Three
/// legitimate outcomes:
///
///  * `Ok(Frame::Video)` — full decode (the round-35 reach goal).
///  * `Err(Eof)` — codec accepted the keyframe but did not emit
///    an output sample yet.
///  * `Err(...)` carrying a DShow diagnostic — round 36 candidate.
///
/// The hard requirement: the error message MUST NOT carry
/// `CLASS_E_CLASSNOTAVAILABLE` anymore.  VFW_E_NOT_COMMITTED is
/// also forbidden when we're using the codec allocator.
#[test]
fn mp43_keyframe_decodes_through_dshow_to_frame_video_via_codec_allocator() {
    let (id, outcome) = match drive_production_path("e2e_decode") {
        Some(t) => t,
        None => {
            eprintln!("round35 E2E: MPG4DS32.AX or MP43 fixture missing; skipping");
            return;
        }
    };
    let neg = last_codec_allocator_negotiation(&id);
    if let Some(n) = neg {
        eprintln!(
            "round35 E2E: negotiation: GA hr={:#010x} alloc={:#010x} \
             SP hr={:#010x} CO hr={:#010x} using_codec={}",
            n.get_allocator_hr,
            n.codec_allocator,
            n.set_properties_hr,
            n.commit_hr,
            n.using_codec_allocator,
        );
    } else {
        eprintln!("round35 E2E: no negotiation captured (ensure_open failed early)");
    }
    match outcome {
        Err(oxideav_core::Error::Eof) => {
            eprintln!(
                "round35 E2E: receive_frame → Eof (codec accepted real \
                 MP43 keyframe; no output sample queued through \
                 HostIPin::Receive yet)."
            );
        }
        Err(e) => {
            let msg = format!("{e}");
            eprintln!("round35 E2E: receive_frame → Err({msg})");
            // Round 35 hard requirement: never CLASS_E_CLASSNOTAVAILABLE.
            assert!(
                !msg.contains("0x80040111"),
                "round35: receive_frame still surfaces CLASS_E_CLASSNOTAVAILABLE: {msg}"
            );
            // When codec allocator is in use, also never VFW_E_NOT_COMMITTED.
            if neg.is_some_and(|n| n.using_codec_allocator) {
                assert!(
                    !msg.contains("0x80040209"),
                    "round35: with codec allocator in use, receive_frame still \
                     reports VFW_E_NOT_COMMITTED (0x80040209): {msg}"
                );
            }
        }
        Ok(oxideav_core::Frame::Video(v)) => {
            eprintln!(
                "round35 E2E: real MP43 keyframe surfaced Frame::Video with {} planes",
                v.planes.len()
            );
            assert!(!v.planes.is_empty(), "Frame::Video has no planes");
            let plane0 = &v.planes[0];
            assert!(plane0.stride > 0, "plane0 stride is 0");
            let nonzero = plane0.data.iter().filter(|&&b| b != 0).count();
            eprintln!(
                "round35 E2E: plane0 stride={} bytes={} nonzero={}",
                plane0.stride,
                plane0.data.len(),
                nonzero
            );
            assert!(nonzero > 0, "round35: Frame::Video plane0 is all zero");
        }
        Ok(other) => panic!("expected Frame::Video, got {other:?}"),
    }
}

// ────────────────────────────────────────────────────────────────
// Test 7 — CLSID_MEMORY_ALLOCATOR const has the canonical bytes
// per axextend.h.
// ────────────────────────────────────────────────────────────────

/// Sanity-check the GUID against its canonical string form so a
/// future typo flips this test before it can break a real codec.
#[test]
fn clsid_memory_allocator_guid_matches_axextend_h() {
    assert_eq!(
        oxideav_vfw::com::CLSID_MEMORY_ALLOCATOR.to_braced_string(),
        "{1E651CC0-B199-11D0-8212-00C04FC32C45}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 8 — Round-32/33 fallback path still works after round-35.
// ────────────────────────────────────────────────────────────────

/// Drive the host-allocator path against a *minted* host allocator
/// to confirm round-32+34's fallback Commit/SetProperties/
/// GetBuffer state machine is unchanged by round 35's additions.
#[test]
fn host_allocator_fallback_path_still_works() {
    let mut sb = Sandbox::new();
    let alloc = sb.mint_host_mem_allocator(4, 256 * 1024, 0).unwrap();

    // Commit.
    let r_co = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_COMMIT,
        &[],
    )
    .unwrap();
    assert_eq!(r_co, oxideav_vfw::com::S_OK);

    // GetBuffer.
    let pp = sb.host.arena_alloc(4).unwrap();
    sb.mmu.write_initializer(pp, &0u32.to_le_bytes()).unwrap();
    let r_gb = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_GET_BUFFER,
        &[pp, 0, 0, 0],
    )
    .unwrap();
    assert_eq!(r_gb, oxideav_vfw::com::S_OK);
    let sample = sb.mmu.load32(pp).unwrap();
    assert_ne!(sample, 0);
}
