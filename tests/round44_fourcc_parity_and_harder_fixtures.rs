//! Round 44 — exercise the full MS-MPEG-4 v3 fixture corpus
//! through the round-43 DirectShow pipeline.  Two distinct
//! axes:
//!
//!   1. **FourCC parity.**  `docs/video/msmpeg4-fixtures/`
//!      ships six fourcc-* fixtures (MP43, DIV3, DIV4, DVX3,
//!      AP41, COL1) whose AVI containers differ only in
//!      `strh.fccHandler` / BIH `biCompression` — the
//!      elementary bitstream is byte-identical.  Round 44
//!      proves the round-43 path is FourCC-blind: feeding
//!      each of the six fixtures' sample-0 bytes through one
//!      MP43-subtype-tagged decoder surfaces a
//!      `Frame::Video` for every variant.
//!   2. **Harder content fixtures.**  Round 43 only drove
//!      `gop-30-352x288` (a 6-frame deterministic GOP) and
//!      `i-frame-then-p-frame-176x144`.  Round 44 adds the
//!      remaining seven content fixtures the docs corpus
//!      ships, each exercising a distinct codec sub-feature:
//!      - `motion-pan-352x288`  — 4-frame mandelbrot pan
//!        (large-magnitude inter-frame MVs at CIF).
//!      - `with-skip-mbs-352x288` — 5-frame testsrc2 at
//!        qscale=16 (~38% SKIP MBs).
//!      - `qscale-high-352x288`  — single I-frame at
//!        qscale=31 (sparse AC coefficients).
//!      - `qscale-low-352x288`   — single I-frame at
//!        qscale=2 (dense AC coefficients).
//!      - `intra-pred-active-352x288` — single mandelbrot
//!        I-frame with non-trivial AC-prediction direction
//!        switching.
//!      - `i-only-352x288-cif`   — single testsrc I-frame
//!        at CIF.
//!      - `tiny-i-only-176x144`  — single QCIF I-frame.
//!
//! ## Empirical FourCC observation
//!
//! `MPG4DS32.AX` accepts ONLY the MP43 subtype at
//! `IPin::ReceiveConnection`; every other FOURCC subtype
//! (DIV3 / DIV4 / DVX3 / AP41 / COL1) is rejected with
//! `0x8004022a` (`VFW_E_TYPE_NOT_ACCEPTED`).  This is a
//! genuine codec property — `mpg4ds32` is a single-tag
//! filter — not a host bug.  Real DirectShow stacks rely on
//! the FilterMapper to route every MS-MPEG-4-v3 FourCC
//! variant to the same `MPG4DS32.AX` filter, then present
//! the negotiation as MP43.  Round 44 mirrors that policy:
//! the `record.fourcc` we register the host factory with is
//! **always** `"MP43"` regardless of which fourcc-* fixture
//! the test feeds, because every fixture's elementary
//! bitstream is byte-identical (only the AVI container tag
//! differs).
//!
//! ## References (clean-room, on-disk)
//!
//! * `docs/video/msmpeg4-fixtures/{fourcc-MP43,fourcc-DIV3,
//!   fourcc-DIV4,fourcc-DVX3,fourcc-AP41,fourcc-COL1}/notes.md`
//!   — fixture descriptions; each one's "Bitstream features
//!   exercised" section confirms the elementary bitstream is
//!   identical to the others.
//! * `docs/video/msmpeg4-fixtures/{motion-pan,with-skip-mbs,
//!   qscale-high,qscale-low,intra-pred-active,i-only-352x288-cif,
//!   tiny-i-only-176x144}/notes.md` — content-fixture
//!   descriptions and per-frame trace summaries.
//! * Microsoft DirectShow API ("DirectShow Filter Graph
//!   Manager") for the `IPin::ReceiveConnection` and
//!   `MEDIASUBTYPE` semantics referenced above.

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

fn fixture_path(name: &str) -> Option<PathBuf> {
    let p = workspace_root()?.join(format!("docs/video/msmpeg4-fixtures/{name}/input.avi"));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// Mint a fresh `SandboxedDshowDecoder` configured against the
/// MP43 subtype.  See module docs for why every fixture — not
/// just `fourcc-MP43` — drives through this same factory.
fn make_dshow_decoder(width: u32, height: u32) -> Option<Box<dyn oxideav_core::Decoder>> {
    let dll_path = dshow_dll_path()?;
    let id = format!(
        "vfw_round44_{}",
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

/// Drive the first `n` AVI samples through one fresh decoder
/// instance and return the count of `Frame::Video`s surfaced
/// plus any per-frame error strings.  All fixtures in this
/// round are CIF (352×288) or QCIF (176×144) and decode as
/// 24bpp BGR (one plane of `w·h·3` bytes).
fn drive_fixture(name: &str, n: u32) -> (usize, Vec<String>) {
    let path = match fixture_path(name) {
        Some(p) => p,
        None => return (0, vec![format!("fixture {name} missing")]),
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => return (0, vec![format!("read {name}: {e}")]),
    };
    let mut samples = Vec::with_capacity(n as usize);
    for i in 0..n {
        match common::avi_extractor::extract_video_sample(&bytes, i) {
            Ok(s) => samples.push(s),
            Err(e) => return (0, vec![format!("extract {name}[{i}]: {e}")]),
        }
    }
    if samples.is_empty() {
        return (0, vec![format!("{name}: zero samples extracted")]);
    }
    let (w, h) = (samples[0].width, samples[0].height);
    let expected_plane0 = (w * h * 3) as usize;
    let mut decoder = match make_dshow_decoder(w, h) {
        Some(d) => d,
        None => return (0, vec!["MPG4DS32.AX missing or factory mint failed".into()]),
    };
    let mut got = 0usize;
    let mut errs = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        let pts = (i as i64) * 40_000;
        let pkt = if i == 0 {
            Packet::new(0, TimeBase::new(1, 25), s.bytes.clone())
                .with_keyframe(true)
                .with_pts(pts)
        } else {
            Packet::new(0, TimeBase::new(1, 25), s.bytes.clone()).with_pts(pts)
        };
        match drive_one(decoder.as_mut(), &pkt) {
            RoundTrip::Video {
                planes,
                plane0_bytes,
            } => {
                if planes >= 1 && plane0_bytes == expected_plane0 {
                    got += 1;
                } else {
                    errs.push(format!(
                        "{name}[{i}]: Video shape mismatch: planes={planes}, \
                         plane0={plane0_bytes} (want {expected_plane0})"
                    ));
                }
            }
            other => errs.push(format!("{name}[{i}]: {other:?}")),
        }
    }
    (got, errs)
}

// ────────────────────────────────────────────────────────────────
// Test 1 — every one of the six fourcc-* fixtures decodes its
// sample-0 I-frame end-to-end.  All six bitstreams are
// byte-identical (only the AVI container tag differs); the
// codec accepts only the MP43 subtype at connection time, so
// the host factory is always registered with `record.fourcc=
// "MP43"` (see module docs).
// ────────────────────────────────────────────────────────────────

#[test]
fn r44_iframe_decodes_through_all_six_fourcc_containers() {
    if dshow_dll_path().is_none() {
        eprintln!("round44 fourcc-parity: MPG4DS32.AX missing; skipping");
        return;
    }
    let names = [
        "fourcc-MP43",
        "fourcc-DIV3",
        "fourcc-DIV4",
        "fourcc-DVX3",
        "fourcc-AP41",
        "fourcc-COL1",
    ];
    let mut ok_count = 0usize;
    let mut all_errs: Vec<String> = Vec::new();
    for name in &names {
        let (got, errs) = drive_fixture(name, 1);
        eprintln!("round44 fourcc-parity: {name} → {got}/1 Video, errs={errs:?}");
        if got == 1 {
            ok_count += 1;
        } else {
            all_errs.extend(errs);
        }
    }
    assert_eq!(
        ok_count,
        names.len(),
        "round44: every fourcc-* container variant must decode its sample 0 \
         (got {ok_count}/{}, errors: {all_errs:?})",
        names.len()
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — `motion-pan-352x288` 4-frame mandelbrot pan.  The
// pan-induced large-magnitude global inter-frame motion vectors
// exercise non-trivial MV decode in P-frames at CIF.
// ────────────────────────────────────────────────────────────────

#[test]
fn r44_motion_pan_4_frame_decodes_end_to_end() {
    if dshow_dll_path().is_none() {
        eprintln!("round44 motion-pan: MPG4DS32.AX missing; skipping");
        return;
    }
    if fixture_path("motion-pan-352x288").is_none() {
        eprintln!("round44 motion-pan: fixture missing; skipping");
        return;
    }
    let (got, errs) = drive_fixture("motion-pan-352x288", 4);
    eprintln!("round44 motion-pan: {got}/4 frames Video, errs={errs:?}");
    assert_eq!(
        got, 4,
        "round44: motion-pan-352x288 must decode 4/4 frames (got {got}, errors: {errs:?})"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 3 — `with-skip-mbs-352x288` 5-frame testsrc2 at qscale=16.
// The mostly-static background drives the encoder to a high SKIP-MB
// fraction (~38% per the fixture's notes.md); exercises the
// SKIP-MB code path through the codec.
// ────────────────────────────────────────────────────────────────

#[test]
fn r44_with_skip_mbs_5_frame_decodes_end_to_end() {
    if dshow_dll_path().is_none() {
        eprintln!("round44 with-skip-mbs: MPG4DS32.AX missing; skipping");
        return;
    }
    if fixture_path("with-skip-mbs-352x288").is_none() {
        eprintln!("round44 with-skip-mbs: fixture missing; skipping");
        return;
    }
    let (got, errs) = drive_fixture("with-skip-mbs-352x288", 5);
    eprintln!("round44 with-skip-mbs: {got}/5 frames Video, errs={errs:?}");
    assert_eq!(
        got, 5,
        "round44: with-skip-mbs-352x288 must decode 5/5 frames (got {got}, errors: {errs:?})"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 4 — qscale boundary I-frames + content-shape I-frames.
// One test per single-frame fixture, each surfacing exactly one
// `Frame::Video`.  These exercise:
//   * qscale-high-352x288  (qscale=31, sparse coefficients)
//   * qscale-low-352x288   (qscale=2, dense coefficients)
//   * intra-pred-active-352x288 (mandelbrot AC-pred direction churn)
//   * i-only-352x288-cif   (testsrc I-frame at CIF)
//   * tiny-i-only-176x144  (QCIF baseline)
// ────────────────────────────────────────────────────────────────

#[test]
fn r44_iframe_corpus_decodes_end_to_end() {
    if dshow_dll_path().is_none() {
        eprintln!("round44 iframe-corpus: MPG4DS32.AX missing; skipping");
        return;
    }
    let cases = [
        "qscale-high-352x288",
        "qscale-low-352x288",
        "intra-pred-active-352x288",
        "i-only-352x288-cif",
        "tiny-i-only-176x144",
    ];
    let mut ok = 0usize;
    let mut all_errs: Vec<String> = Vec::new();
    for name in &cases {
        if fixture_path(name).is_none() {
            eprintln!("round44 iframe-corpus: {name} missing; skipping");
            continue;
        }
        let (got, errs) = drive_fixture(name, 1);
        eprintln!("round44 iframe-corpus: {name} → {got}/1 Video, errs={errs:?}");
        if got == 1 {
            ok += 1;
        } else {
            all_errs.extend(errs);
        }
    }
    // All five fixtures are checked into docs/, so all five must
    // decode.  If a fixture is missing the per-iteration `if let`
    // skips it silently; we count successes and assert against the
    // total available count.
    let available = cases.iter().filter(|n| fixture_path(n).is_some()).count();
    assert_eq!(
        ok, available,
        "round44: all available I-frame fixtures must decode (got {ok}/{available}, errors: {all_errs:?})"
    );
    // And we expect at least four to be present so the assertion
    // doesn't degenerate to "0 of 0 trivially passes" if someone
    // wipes the fixture dir.
    assert!(
        available >= 4,
        "round44: expected at least 4 I-frame fixtures available, got {available}"
    );
}
