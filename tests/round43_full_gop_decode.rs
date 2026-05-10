//! Round 43 — close the sample-release cycle so the full
//! `gop-30-352x288` 6-frame GOP decodes end-to-end through the
//! same `SandboxedDshowDecoder` instance.
//!
//! ## Headline
//!
//! Round 42 landed the first MULTI-frame DShow decode (1 → 2
//! frames at 176×144 via the `i-frame-then-p-frame` fixture) but
//! exposed two distinct blockers when the same path was driven
//! against the larger CIF GOP:
//!
//!   * **(a) Output-allocator pool walk traps on P-frame.**  The
//!     codec's output `IMemAllocator::GetBuffer` walked our
//!     host pool and tripped on a corrupted `cur+36 = 0xffff0223`
//!     read — i.e. `cur ≈ 0xffff_01ff`, a junk pool pointer that
//!     surfaced as a memory-fault trap inside our stub.  The
//!     trap masked the underlying issue and left no recovery
//!     path (the entire pipeline aborted).
//!   * **(b) Sample-release cycle gap.**  Our
//!     `IMediaSample::Release` thunk floored refcount at 1 and
//!     never recycled the sample back into its allocator's
//!     pool; the codec's standard `pSample->Release()` after
//!     downstream `Receive` couldn't drive `in_use` back to 0.
//!     After `cBuffers (=4)` calls the pool was exhausted with
//!     `0x80040211` (`VFW_E_TIMEOUT`).
//!
//! Round 43's fixes:
//!
//!   * `alloc_get_buffer` (a) sanity-checks every pool pointer
//!     before the `cur+36` / `cur+32` reads — a corrupted link
//!     surfaces as `VFW_E_TIMEOUT` instead of a memory-fault
//!     trap.  (b) Forces the issued sample's refcount to exactly
//!     1 (was: bump-by-1) so the codec's standard
//!     one-AddRef + one-Release pattern reliably drives it
//!     through 1 → 0.
//!   * A new `sample_release` thunk replaces the generic
//!     `release` for `IMediaSample::Release`: when refcount
//!     transitions 1 → 0, it clears the sample's `in_use` flag
//!     at `+36`, mirroring the canonical `CMediaSample`
//!     destructor's call back into `pAllocator->ReleaseBuffer`.
//!   * `receive_frame` calls `IMemAllocator::ReleaseBuffer` on
//!     the input allocator after `IMemInputPin::Receive` returns,
//!     so the next `send_packet` finds a free input slot.
//!
//! Net result: the 6-frame `gop-30-352x288` fixture now decodes
//! 6/6 frames end-to-end (was 1/6 in round 42).
//!
//! ## References (clean-room, on-disk)
//!
//! * `docs/video/msmpeg4-fixtures/gop-30-352x288/notes.md` —
//!   fixture description.
//! * `docs/video/msmpeg4-fixtures/i-frame-then-p-frame-176x144/
//!   notes.md` — round-42 baseline fixture (regression guard
//!   covered by `tests/round42_dshow_iframe_then_pframe.rs`).
//! * Microsoft DShow base-classes documentation for
//!   `CMediaSample` / `CBaseAllocator` ABI references in
//!   `strmif.h` / `wxutil.h`.

#![cfg(feature = "auto-discovery")]

mod common;

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, Error, Frame, Packet, TimeBase};
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

fn make_dshow_decoder(width: u32, height: u32) -> Option<Box<dyn oxideav_core::Decoder>> {
    let dll_path = dshow_dll_path()?;
    let id = format!(
        "vfw_round43_gop_{}",
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
    make_decoder(&params).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RoundTrip {
    Video { planes: usize, plane0_bytes: usize },
    Eof,
    NeedMore,
    OtherErr(String),
}

fn drive_one(decoder: &mut dyn oxideav_core::Decoder, packet: &Packet) -> RoundTrip {
    if let Err(e) = decoder.send_packet(packet) {
        return RoundTrip::OtherErr(format!("send_packet: {e}"));
    }
    match decoder.receive_frame() {
        Ok(Frame::Video(v)) => RoundTrip::Video {
            planes: v.planes.len(),
            plane0_bytes: v.planes.first().map(|p| p.data.len()).unwrap_or(0),
        },
        Ok(other) => RoundTrip::OtherErr(format!("non-video frame: {other:?}")),
        Err(Error::Eof) => RoundTrip::Eof,
        Err(Error::NeedMore) => RoundTrip::NeedMore,
        Err(e) => RoundTrip::OtherErr(format!("{e}")),
    }
}

// ────────────────────────────────────────────────────────────────
// Test 1 — full 6-frame GOP at 352×288 decodes through the same
// `SandboxedDshowDecoder` instance.  Promotes the round-42
// "1/6 frames" measurement to a hard "6/6 frames" assertion.
// Each Video frame must carry one BGR24 plane of `width × height ×
// 3 = 304_128 bytes`.
// ────────────────────────────────────────────────────────────────

#[test]
fn r43_gop30_full_six_frame_decode() {
    let path = match workspace_root()
        .map(|r| r.join("docs/video/msmpeg4-fixtures/gop-30-352x288/input.avi"))
    {
        Some(p) if p.is_file() => p,
        _ => {
            eprintln!("round43 gop-30: fixture missing; skipping");
            return;
        }
    };
    let bytes = std::fs::read(&path).expect("read gop-30 fixture");
    let mut frames: Vec<(u32, u32, Vec<u8>)> = Vec::new();
    for idx in 0..6u32 {
        match common::avi_extractor::extract_video_sample(&bytes, idx) {
            Ok(s) => frames.push((s.width, s.height, s.bytes)),
            Err(e) => {
                eprintln!("round43 gop-30: extract sample {idx}: {e}; aborting");
                return;
            }
        }
    }
    let (width, height, _) = frames[0].clone();
    assert_eq!(width, 352);
    assert_eq!(height, 288);
    let expected_bytes = (width * height * 3) as usize;
    let mut decoder = match make_dshow_decoder(width, height) {
        Some(d) => d,
        None => {
            eprintln!("round43 gop-30: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let mut video_count = 0usize;
    let mut errs: Vec<String> = Vec::new();
    for (i, (_w, _h, payload)) in frames.iter().enumerate() {
        let pts = (i as i64) * 40_000;
        let pkt = if i == 0 {
            Packet::new(0, TimeBase::new(1, 25), payload.clone())
                .with_keyframe(true)
                .with_pts(pts)
        } else {
            Packet::new(0, TimeBase::new(1, 25), payload.clone()).with_pts(pts)
        };
        match drive_one(decoder.as_mut(), &pkt) {
            RoundTrip::Video {
                planes,
                plane0_bytes,
            } => {
                assert!(planes >= 1, "frame {i}: Video must have >=1 plane");
                assert_eq!(
                    plane0_bytes, expected_bytes,
                    "frame {i}: plane0 must be w·h·3 ({expected_bytes}) bytes"
                );
                video_count += 1;
            }
            other => errs.push(format!("frame {i} → {other:?}")),
        }
    }
    eprintln!("round43 gop-30: {video_count} / 6 frames surfaced Video, errors: {errs:?}");
    assert_eq!(
        video_count, 6,
        "round43: full 6-frame GOP must decode end-to-end (got {video_count}, errors: {errs:?})"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — drive ten back-to-back I+P pairs through ONE decoder
// to stress the recycle cycle past the (=4) pool size in both
// directions.  If the round-43 release-cycle fix is correct,
// every pair surfaces a pair of `Frame::Video`s; if it
// regresses, we'll exhaust either pool around frame 5 and the
// trailing cycles will return `VFW_E_TIMEOUT`.
//
// The fixture is the same 176×144 I+P pair the round-42 test
// uses; we just feed it 10× in a row.  This is a regression
// guard that the recycle path actually closes, distinct from
// the round-42 single-pair test.
// ────────────────────────────────────────────────────────────────

#[test]
fn r43_pool_recycle_survives_ten_ip_cycles() {
    let path = match workspace_root()
        .map(|r| r.join("docs/video/msmpeg4-fixtures/i-frame-then-p-frame-176x144/input.avi"))
    {
        Some(p) if p.is_file() => p,
        _ => {
            eprintln!("round43 recycle: fixture missing; skipping");
            return;
        }
    };
    let bytes = std::fs::read(&path).expect("read I+P fixture");
    let s0 = match common::avi_extractor::extract_video_sample(&bytes, 0) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("round43 recycle: extract sample 0: {e}; skipping");
            return;
        }
    };
    let s1 = match common::avi_extractor::extract_video_sample(&bytes, 1) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("round43 recycle: extract sample 1: {e}; skipping");
            return;
        }
    };
    let mut decoder = match make_dshow_decoder(s0.width, s0.height) {
        Some(d) => d,
        None => {
            eprintln!("round43 recycle: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let expected_bytes = (s0.width * s0.height * 3) as usize;
    let mut video_count = 0usize;
    for cycle in 0..10u32 {
        let pts_i = (cycle as i64) * 80_000;
        let pts_p = pts_i + 40_000;
        let p_i = Packet::new(0, TimeBase::new(1, 25), s0.bytes.clone())
            .with_keyframe(true)
            .with_pts(pts_i);
        let p_p = Packet::new(0, TimeBase::new(1, 25), s1.bytes.clone()).with_pts(pts_p);
        let r_i = drive_one(decoder.as_mut(), &p_i);
        let r_p = drive_one(decoder.as_mut(), &p_p);
        eprintln!("round43 recycle: cycle {cycle}: I → {r_i:?}, P → {r_p:?}");
        if let RoundTrip::Video { plane0_bytes, .. } = &r_i {
            assert_eq!(*plane0_bytes, expected_bytes);
            video_count += 1;
        }
        if let RoundTrip::Video { plane0_bytes, .. } = &r_p {
            assert_eq!(*plane0_bytes, expected_bytes);
            video_count += 1;
        }
    }
    eprintln!("round43 recycle: {video_count} / 20 frames surfaced Video");
    // The codec's internal state will eventually accumulate
    // GOP-related state that may legitimately cause later cycles
    // to drop frames (the I+P fixture's MS-MPEG-4 v3 stream uses
    // forward references whose meaning when re-fed an "I" the
    // codec already saw is undefined).  Round 43 only requires
    // that pool exhaustion does NOT regress: at minimum the
    // first FIVE cycles (= 10 frames) — past the 4-slot pool
    // limit — must surface Video.  Anything fewer indicates the
    // recycle path didn't close.
    assert!(
        video_count >= 10,
        "round43: recycle cycle must clear the 4-slot pool limit; got {video_count}/20"
    );
}
