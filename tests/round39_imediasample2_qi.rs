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
// NOTE — these three tests were originally pinned to the r39-baseline
// trap signature (`rva=0x00007184` + a specific call-chain shape +
// the live `recheck_sample_slot13=0xfffe03a0` slot in the diagnostic
// blob).  Round 41 fixed the underlying `IMemAllocator::GetBuffer`
// arg-count bug (registered with `arg_dwords=4`, should have been
// 5) — Receive now returns S_OK and emits a frame, so the trap
// branch (and the diagnostic blob) is never built.  The tests are
// rewritten to assert the FIXED behaviour: a Video frame surfaces
// from the same one-shot keyframe drive.
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
    eprintln!("round39 success-tail (post-r41): {msg}");
    // The IsEqualGUID read at `0x7184` was the symptom of the
    // GetBuffer arg-count bug.  Once the dispatcher pops the
    // right number of bytes, that branch is never entered.
    assert!(
        !msg.contains("rva=0x00007184"),
        "r41 expected the 0x7184 trap to be GONE: {msg}"
    );
    assert!(
        msg.starts_with("ok: Video(VideoFrame"),
        "r41 expected a Video frame to surface from the keyframe: {msg}"
    );
}

#[test]
fn r39_pre_transform_helper_completes_qi() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round39 pre-transform: fixtures missing; skipping");
            return;
        }
    };
    eprintln!("round39 pre-transform (post-r41): {msg}");
    // The helper at `0x5e34` (which QIed pInSample for
    // IID_IMediaSample2) now completes through to Transform AND
    // Transform completes through to the success exit at
    // `0x65c0` — manifest as a frame surfacing from `Receive`.
    assert!(
        msg.starts_with("ok: Video(VideoFrame"),
        "r41 expected a Video frame after the helper completes: {msg}"
    );
}

#[test]
fn r39_input_sample_slot_13_unchanged_after_run() {
    let msg = match try_drive_one_keyframe() {
        Some(m) => m,
        None => {
            eprintln!("round39 slot-stability: fixtures missing; skipping");
            return;
        }
    };
    // Slot stability is intrinsically tested by the keyframe
    // round-tripping all the way to a Video frame: a corrupted
    // slot 13 would crash the codec before we got here.  Keep
    // the smoke test as a frame-emission assertion so any future
    // vtable-layout regression surfaces immediately.
    assert!(
        msg.starts_with("ok: Video(VideoFrame"),
        "r41 expected slot-13 stability via successful frame emission: {msg}"
    );
}
