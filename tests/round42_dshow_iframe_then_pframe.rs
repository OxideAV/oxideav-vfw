//! Round 42 — drive a SECOND packet (P-frame) through the same
//! `SandboxedDshowDecoder` instance after the round-41 I-frame
//! breakthrough.
//!
//! ## Headline
//!
//! Round 41 landed the first end-to-end MP43 keyframe decode
//! through the DirectShow pipeline (IMemAllocator::GetBuffer
//! arg-count fix unblocking `CTransformFilter::Transform`).
//! Round 42 picks the natural follow-on: feed the SAME decoder
//! instance an I-frame followed by a P-frame and see what the
//! production path does on the second `send_packet` →
//! `receive_frame` round-trip.
//!
//! The fixture is `i-frame-then-p-frame-176x144` (DIV3-tagged but
//! the elementary bitstream is byte-identical to MP43; per
//! `docs/video/msmpeg4-fixtures/fourcc-MP43/notes.md` the only
//! container difference is the FourCC tag).  Sample 0 is an
//! I-frame (`use_skip_mb_code=0`), sample 1 is a P-frame
//! (`use_skip_mb_code=1`).
//!
//! Round 42's role is **measurement**: round 41 was the first run
//! that ever returned a `Frame::Video` from this pipeline, so we
//! had no data on whether the codec's internal state machine
//! survives a second `Receive` call against the same filter
//! instance.  The test is shaped to:
//!
//!   1. Confirm the I-frame still decodes (round-41 regression
//!      guard against the "second run" not breaking the first).
//!   2. Drive the P-frame and capture EXACTLY what happens — a
//!      clean second `Frame::Video` (best case), an `Eof` because
//!      no downstream sample queued (intermediate case), or an
//!      `Unsupported` carrying a diagnostic blob round 43 can
//!      mine (worst case, but still progress: we will know which
//!      slot fails and why).
//!
//! No assertion is made about which of these three outcomes
//! occurs; the test pins down the OBSERVED behaviour against
//! commit `e20b3d0` (round 41) so any regression — or any future
//! improvement — surfaces immediately.
//!
//! ## References (clean-room, on-disk)
//!
//! * `docs/video/msmpeg4-fixtures/i-frame-then-p-frame-176x144/
//!   notes.md` — fixture description, per-frame trace summary.
//! * `docs/video/msmpeg4-fixtures/msmpeg4-fixtures-and-traces.md`
//!   — corpus README.
//! * Microsoft "DirectShow API" via `axextend.h` /  `strmif.h`
//!   ICOM definitions for `IMemInputPin::Receive` and
//!   `CTransformFilter::Transform` semantics.

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

fn ip_fixture_path() -> Option<PathBuf> {
    let p = workspace_root()?
        .join("docs/video/msmpeg4-fixtures/i-frame-then-p-frame-176x144/input.avi");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// Extract the first two video samples (I-frame, P-frame) from the
/// `i-frame-then-p-frame-176x144` AVI fixture.  Both samples carry
/// the byte-identical MS-MPEG-4-v3 elementary bitstream the
/// `mpg4ds32.ax` codec accepts; the AVI strh.fccHandler at offset
/// 0x74 says `DIV3` (so does our extractor), but the codec's
/// connection-time FourCC negotiation goes by the AM_MEDIA_TYPE
/// subtype we plumb in via `register_factory_for_id` below.
fn extract_i_then_p() -> Option<(u32, u32, Vec<u8>, Vec<u8>)> {
    let path = ip_fixture_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let s0 = common::avi_extractor::extract_video_sample(&bytes, 0).ok()?;
    let s1 = common::avi_extractor::extract_video_sample(&bytes, 1).ok()?;
    // Sanity: dims must match across samples.
    assert_eq!(s0.width, s1.width);
    assert_eq!(s0.height, s1.height);
    Some((s0.width, s0.height, s0.bytes, s1.bytes))
}

/// Outcome of one `send_packet` + `receive_frame` round-trip,
/// flattened into a string the assertions can pattern-match
/// without owning `oxideav_core::Frame` (which is non-trivially
/// comparable).
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

fn make_dshow_decoder(width: u32, height: u32) -> Option<Box<dyn oxideav_core::Decoder>> {
    let dll_path = dshow_dll_path()?;
    let id = format!(
        "vfw_round42_ip_{}",
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

// ────────────────────────────────────────────────────────────────
// Test 1 — drive I+P through the same decoder instance and pin
// the per-packet outcomes.
//
// Round 41's `r41_mp43_keyframe_decodes_after_getbuffer_arg_count_
// fix` test only ever drives ONE packet.  Round 42 drives TWO.
// The assertion shape: outcome strings recorded in
// arrival order; we always record what we got, then the
// assertion checks that:
//
//   * Frame 0 (I-frame) MUST surface `RoundTrip::Video { ... }`
//     (regression guard for round 41).
//   * Frame 1 (P-frame) MAY be Video / Eof / OtherErr — we
//     accept any of the three but require the diagnostic blob
//     to be informative when it's an error.
//
// The actual frame-1 outcome is logged via `eprintln!` so a
// `cargo test -- --nocapture` run surfaces what the codec did,
// driving the round-43 dispatch.
// ────────────────────────────────────────────────────────────────

#[test]
fn r42_iframe_then_pframe_through_same_decoder() {
    let (width, height, iframe, pframe) = match extract_i_then_p() {
        Some(t) => t,
        None => {
            eprintln!("round42 I+P: fixture missing; skipping");
            return;
        }
    };
    let mut decoder = match make_dshow_decoder(width, height) {
        Some(d) => d,
        None => {
            eprintln!("round42 I+P: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    eprintln!(
        "round42 I+P: I-frame {} bytes + P-frame {} bytes at {}×{}",
        iframe.len(),
        pframe.len(),
        width,
        height,
    );

    // Frame 0 — I-frame.  Must surface a Video frame (round-41
    // regression guard).
    let p0 = Packet::new(0, TimeBase::new(1, 25), iframe).with_keyframe(true);
    let r0 = drive_one(decoder.as_mut(), &p0);
    eprintln!("round42 I+P: frame 0 (I) → {r0:?}");
    let expected_bytes = (width * height * 3) as usize;
    match &r0 {
        RoundTrip::Video {
            planes,
            plane0_bytes,
        } => {
            assert!(*planes >= 1, "I-frame Video must have >=1 plane");
            assert_eq!(
                *plane0_bytes, expected_bytes,
                "I-frame plane0 must be w·h·3 bytes (the BGR24 surface)"
            );
        }
        other => {
            panic!("round42 I+P: round-41 regression — frame 0 must surface Video, got {other:?}",)
        }
    }

    // Frame 1 — P-frame.  May surface Video (best case), Eof
    // (codec accepted the input but didn't queue a downstream
    // sample), or OtherErr carrying a diagnostic blob.  Record
    // what happens and gate only on "the path didn't panic".
    let p1 = Packet::new(0, TimeBase::new(1, 25), pframe).with_pts(40_000);
    let r1 = drive_one(decoder.as_mut(), &p1);
    eprintln!("round42 I+P: frame 1 (P) → {r1:?}");

    // The pipeline must not regress to NeedMore — we always
    // delivered a packet before calling receive_frame.
    assert!(
        !matches!(r1, RoundTrip::NeedMore),
        "round42: frame 1 should not be NeedMore (we sent a packet first); got {r1:?}",
    );

    // If frame 1 errored, the diagnostic must mention the DShow
    // pathway so we can dispatch a focused round 43.  An error
    // shape we know is uninformative is also OK — but a
    // wholly-empty error message is not.
    if let RoundTrip::OtherErr(msg) = &r1 {
        assert!(
            !msg.is_empty(),
            "round42: P-frame error message must be non-empty",
        );
        assert!(
            msg.contains("DShow")
                || msg.contains("Receive")
                || msg.contains("vfw discovery")
                || msg.contains("Transform")
                || msg.contains("trapped"),
            "round42: P-frame error message lacks any DShow diagnostic anchor: {msg}",
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Test 2 — codec-id reflects the registration FourCC.
//
// A trivial guard so the I+P fixture's strh.fccHandler `DIV3`
// vs. the registration's MP43 doesn't get conflated by a future
// refactor.  The codec sees the `register_factory_for_id`
// FourCC, which is what plumbs into the AM_MEDIA_TYPE.subtype
// during ReceiveConnection.
// ────────────────────────────────────────────────────────────────

#[test]
fn r42_codec_id_reflects_registered_fourcc() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round42 codec-id: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let id = "vfw_round42_codec_id_check";
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path,
            fourcc: "MP43".to_string(),
            kind: Kind::DirectShow,
            clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".to_string()),
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(176);
    params.height = Some(144);
    let decoder = make_decoder(&params).expect("make_decoder constructs lazily");
    assert_eq!(decoder.codec_id().as_str(), id);
}

// ────────────────────────────────────────────────────────────────
// Test 3 — drive the gop-30-352x288 fixture's 6-frame GOP through
// the same decoder instance.  Sample 0 is I, samples 1..=5 are P.
//
// Compared to test 1's 2-frame I+P at 176×144, this one stresses:
//
//   * 4× the per-frame surface area (352×288×3 = 304_128 bytes).
//   * Five back-to-back P-frames against a single shared
//     reference.  The codec's reference-frame management lives
//     inside `mpg4ds32`'s private state, so we have no insight
//     into what it expects beyond "submit the next coded sample
//     and observe what comes out".
//   * The fixture's encoder used `-vtag DIV3`, but the connection
//     advertises `MP43` per the registration; if the negotiated
//     subtype matters for P-frame reference management the codec
//     would fail at frame 1.
//
// Like test 1, the assertions accept any of Video / Eof /
// OtherErr per frame and just record the per-frame outcome
// vector.  Headline measurement: how many of the 6 frames
// surface as Video.
// ────────────────────────────────────────────────────────────────

#[test]
fn r42_gop30_six_frame_run_through_dshow() {
    let path = match workspace_root()
        .map(|r| r.join("docs/video/msmpeg4-fixtures/gop-30-352x288/input.avi"))
    {
        Some(p) if p.is_file() => p,
        _ => {
            eprintln!("round42 gop-30: fixture missing; skipping");
            return;
        }
    };
    let bytes = std::fs::read(&path).expect("read gop-30 fixture");
    // Pull all 6 samples up front so the decoder loop has to
    // do nothing but I/O.
    let mut frames: Vec<(u32, u32, Vec<u8>)> = Vec::new();
    for idx in 0..6u32 {
        match common::avi_extractor::extract_video_sample(&bytes, idx) {
            Ok(s) => frames.push((s.width, s.height, s.bytes)),
            Err(e) => {
                eprintln!("round42 gop-30: extract sample {idx}: {e}; aborting at idx {idx}");
                break;
            }
        }
    }
    if frames.len() < 2 {
        eprintln!("round42 gop-30: <2 samples extractable; skipping");
        return;
    }
    let (width, height, _) = frames[0].clone();
    let mut decoder = match make_dshow_decoder(width, height) {
        Some(d) => d,
        None => {
            eprintln!("round42 gop-30: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    eprintln!(
        "round42 gop-30: extracted {} samples at {}×{} (sizes={:?})",
        frames.len(),
        width,
        height,
        frames.iter().map(|(_, _, b)| b.len()).collect::<Vec<_>>(),
    );
    let mut outcomes: Vec<RoundTrip> = Vec::with_capacity(frames.len());
    for (i, (_w, _h, payload)) in frames.iter().enumerate() {
        let pts = (i as i64) * 40_000; // 25 fps == 40 ms
        let pkt = if i == 0 {
            Packet::new(0, TimeBase::new(1, 25), payload.clone())
                .with_keyframe(true)
                .with_pts(pts)
        } else {
            Packet::new(0, TimeBase::new(1, 25), payload.clone()).with_pts(pts)
        };
        let r = drive_one(decoder.as_mut(), &pkt);
        eprintln!("round42 gop-30: frame {i} → {r:?}");
        outcomes.push(r);
    }
    // Frame 0 (I) MUST be Video — round-41 regression guard.
    assert!(
        matches!(&outcomes[0], RoundTrip::Video { .. }),
        "round42 gop-30: frame 0 must surface Video (round-41 regression); got {:?}",
        outcomes[0],
    );
    // Count Video outcomes — that's the headline number for the
    // workspace-README row update.
    let video_count = outcomes
        .iter()
        .filter(|o| matches!(o, RoundTrip::Video { .. }))
        .count();
    eprintln!(
        "round42 gop-30: {video_count} / {} frames surfaced Video",
        outcomes.len(),
    );
    // Sanity: the path must not regress to NeedMore — a packet
    // was always sent before each receive_frame.
    for (i, o) in outcomes.iter().enumerate() {
        assert!(
            !matches!(o, RoundTrip::NeedMore),
            "round42 gop-30: frame {i} should not be NeedMore; got {o:?}",
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Test 4 — the I+P fixture exists at the documented path AND
// extracts cleanly to two video samples.  Decouples fixture
// availability from the (more expensive) end-to-end driver test.
// ────────────────────────────────────────────────────────────────

#[test]
fn r42_fixture_extracts_two_video_samples() {
    let path = match ip_fixture_path() {
        Some(p) => p,
        None => {
            eprintln!("round42 fixture: i-frame-then-p-frame-176x144/input.avi missing; skipping");
            return;
        }
    };
    let bytes = std::fs::read(&path).expect("read fixture");
    let s0 = common::avi_extractor::extract_video_sample(&bytes, 0).expect("sample 0");
    let s1 = common::avi_extractor::extract_video_sample(&bytes, 1).expect("sample 1");
    assert_eq!(s0.width, 176);
    assert_eq!(s0.height, 144);
    assert_eq!(s1.width, 176);
    assert_eq!(s1.height, 144);
    // Per fixture notes.md, the strh.fccHandler is DIV3.
    assert_eq!(&s0.codec_fourcc.to_le_bytes(), b"DIV3");
    assert_eq!(&s1.codec_fourcc.to_le_bytes(), b"DIV3");
    // I-frame has a non-trivial bitstream; P-frame is smaller
    // (mostly skip-MBs against a slowly moving testsrc).
    assert!(!s0.bytes.is_empty(), "I-frame bytes should be non-empty");
    assert!(!s1.bytes.is_empty(), "P-frame bytes should be non-empty");
    // sample 2 must NOT exist (the fixture was encoded with
    // `-frames:v 2`).
    assert!(
        common::avi_extractor::extract_video_sample(&bytes, 2).is_err(),
        "round42: fixture should have exactly 2 samples"
    );
}
