//! Round 39 — `IID_IMediaSample2` host-side QI support.
//!
//! Round 38 disasm of MPG4DS32.AX RVA `0x4064f3` (the QI inside
//! `CTransformFilter::Transform`) identified the IID being requested
//! by the codec as `{36B73884-C2C8-11CF-8B46-00805F6CEF60}` =
//! `IID_IMediaSample2`.  A second QI for the same IID lives in the
//! pre-Transform helper at RVA `0x5e73`.
//!
//! Returning `E_NOINTERFACE` (the round-30..38 baseline) forced the
//! codec down a fallback branch that wrote per-sample property
//! mirrors via individual `IMediaSample` slot calls.  Round 39 wires
//! the host vtable up to:
//!
//!  1. Recognise `IID_IMEDIASAMPLE2` in `sample_qi`.
//!  2. Mint a 21-slot vtable (was 18) carrying live thunks for
//!     `IMediaSample::SetMediaTime` (slot 18, previously absent —
//!     a NULL slot the cleanup branch at RVA `0x4065bd` would
//!     have called if the GetMediaTime check ever returned S_OK)
//!     plus `IMediaSample2::GetProperties` (slot 19) and
//!     `IMediaSample2::SetProperties` (slot 20).
//!  3. Round-trip the public `AM_SAMPLE2_PROPERTIES` struct
//!     (`cbData` / `dwSampleFlags` / `lActual` / `pbBuffer` /
//!     `cbBuffer` / `pMediaType`) so the codec's `SetProperties`
//!     write-back path at RVA `0x6545` doesn't reject our sample.
//!
//! The `Receive` trap at RVA `0x7184` is unchanged (still
//! `IsEqualGUID(NULL+0x1c, &GUID_NULL)`) but now reached via the
//! Transform success-tail at RVA `0x65c0` instead of the failure-
//! cleanup tail at `0x6560`.  See `r39_transform_success_tail_taken`
//! below.

#![cfg(feature = "auto-discovery")]

mod common;

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, Packet, TimeBase};
use oxideav_vfw::discovery::{make_decoder, register_factory_for_id, DiscoveryRecord, Kind};

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

fn try_drive_one_keyframe() -> Option<String> {
    let dll_path = dshow_dll_path()?;
    let (width, height, keyframe) = extract_mp43_keyframe("fourcc-MP43")?;
    let id = format!(
        "vfw_round39_qi2_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
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
    let mut decoder = make_decoder(&params).ok()?;
    let packet = Packet::new(0, TimeBase::new(1, 25), keyframe).with_keyframe(true);
    let _ = decoder.send_packet(&packet);
    let outcome = decoder.receive_frame();
    Some(match outcome {
        Ok(other) => format!("ok: {other:?}"),
        Err(e) => format!("{e}"),
    })
}

// ────────────────────────────────────────────────────────────────
// Test 1 — Transform's success-tail at `0x65c0` is reached (proving
// the `IID_IMediaSample2` QI inside Transform now returns S_OK).
// The trap chain previously included `0x6560` (cleanup branch);
// after r39 it goes through `0x65c0` instead.
// ────────────────────────────────────────────────────────────────

#[test]
fn r39_transform_success_tail_taken() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round39 success-tail: fixtures missing; skipping");
            return;
        }
    };
    eprintln!("round39 success-tail: {msg}");
    // Trap site unchanged from r38 baseline (still IsEqualGUID
    // reading NULL+0x1c at `0x7184`).
    assert!(
        msg.contains("rva=0x00007184"),
        "r39 trap site preserved at 0x7184: {msg}"
    );
    // Transform now exits via `0x65c0` (success xor eax, eax) and
    // NOT via `0x6560` (QI failure cleanup branch).
    assert!(
        msg.contains("0x000065c0"),
        "r39 expected success-tail RVA 0x65c0 in call chain: {msg}"
    );
    assert!(
        !msg.contains("0x00006560"),
        "r39 expected the QI-failure cleanup RVA 0x6560 to be ABSENT \
         (Transform should now succeed): {msg}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — pre-Transform helper at `0x5e34` ALSO QIs pInSample for
// IMediaSample2.  Its success-tail at `0x5f24` should appear in the
// chain after r39 (indicating the helper's QI now succeeds).
// ────────────────────────────────────────────────────────────────

#[test]
fn r39_pre_transform_helper_completes_qi() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round39 pre-transform: fixtures missing; skipping");
            return;
        }
    };
    eprintln!("round39 pre-transform: {msg}");
    // The pre-Transform helper at `0x5e34` runs `[IsEqualGUID
    // (this+0x1c, &kStaticGUID)]` then QIs pInSample for
    // IMediaSample2.  Its success path at `0x5f24` was previously
    // absent from the chain (the QI returned E_NOINTERFACE so the
    // helper took an early-exit failure return).  Round 39 wires
    // the QI to S_OK so the success path runs through.
    assert!(
        msg.contains("0x00005f24"),
        "r39 expected helper success RVA 0x5f24: {msg}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 3 — input sample's vtable slot 13 (GetMediaType) remains the
// host thunk after the codec runs.  Confirms r39's vtable-resize
// (18 → 21 slots) didn't relocate the existing slots.
// ────────────────────────────────────────────────────────────────

#[test]
fn r39_input_sample_slot_13_unchanged_after_run() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round39 slot-stability: fixtures missing; skipping");
            return;
        }
    };
    assert!(
        msg.contains("recheck_sample_slot13=0xfffe03a0"),
        "r39 expected input sample slot 13 to remain the host thunk: {msg}"
    );
}
