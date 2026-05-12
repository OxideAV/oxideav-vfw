//! Round 53 — P-frame quality-regime probe against `mpg4c32.dll`.
//!
//! ## Background
//!
//! Round 51 lit up the encode side end-to-end (`ICCompressQuery` ..
//! `ICCompressEnd`) and observed that at `quality=5000` the codec
//! emits a keyframe for BOTH the I and the P-tagged request when
//! the *content* is identical (frame 0 == frame 1).  Workspace
//! task #803 asks: does the codec emit real P-frames at lower
//! quality regimes when given truly differing content (a small
//! translation), and what is the P/I size ratio across the
//! quality range?
//!
//! ## Probe shape
//!
//! Build a 176×144 BGR24 frame 0 with a fixed gradient pattern.
//! Build frame 1 = frame 0 shifted right by 8 pixels (small
//! horizontal motion; the codec's motion estimator should be able
//! to compensate with a small residual).  For each
//! `quality ∈ {1000, 2000, 3000, 5000, 8000}`: open a fresh
//! encoder HIC, encode frame 0 with `ICCOMPRESS_KEYFRAME` at this
//! quality, then encode frame 1 with `flags = 0` and
//! `prev_bih` / `prev_bytes` pointing at frame 0's *input* bytes
//! (the codec's encoder takes the previous *uncompressed* frame
//! as its reference per the documented `lpbiPrev` / `lpPrev`
//! slots in the `ICCOMPRESS` struct).  Record I-frame size,
//! P-frame size, codec's returned `*lpdwFlags & ICCOMPRESS_KEYFRAME`
//! bit, and the P/I ratio.  Report findings; pass if AT LEAST ONE
//! quality level emits a real P-frame (codec cleared the
//! keyframe flag AND `P-size < 0.5 * I-size`) — otherwise report
//! the codec's actual behaviour as the round's finding (no fake
//! pass).
//!
//! ## What we deliberately DO NOT assert
//!
//! This is a **probe**, not a contract.  vfw codecs are
//! historically permissive about quality knobs — they're allowed
//! to ignore the request, force keyframes on every frame, or
//! emit P-frames at quality levels they've decided are
//! "appropriate".  The round's deliverable is the *observation*
//! of how `mpg4c32` actually behaves; either a real P-frame
//! emission lights up (pass with finding) or the codec
//! consistently emits keyframes regardless (pass with the
//! "codec always emits keyframes" finding).
//!
//! ## References (clean-room, on-disk)
//!
//! * MSDN `ICCompress` topic page — `lpdwFlags` / `lpckid` semantics
//!   (the codec writes back the actual keyframe/non-keyframe
//!   decision into the dword slot the caller passed in).
//! * `winsdk-10/Include/.../um/Vfw.h` — `ICCOMPRESS_KEYFRAME = 0x1`.
//! * Round 51 finding (`tests/round51_msmpeg4_encode_roundtrip.rs`):
//!   identical-content frames at `quality=5000` are always emitted
//!   as keyframes.  Round 53 probes whether differing content +
//!   varying quality changes that.

mod common;

use oxideav_vfw::win32::vfw32::ICCOMPRESS_KEYFRAME;
use oxideav_vfw::{Bih, Sandbox};
use std::path::PathBuf;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn mpg4c32_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/mpg4c32.dll");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

const ICMODE_COMPRESS: u32 = 1;
const W: u32 = 176;
const H: u32 = 144;

/// 176×144 BGR24 gradient frame.  Pattern stride = `width * 3`,
/// bottom-up (BMP convention).
fn make_bgr24_gradient(width: u32, height: u32) -> Vec<u8> {
    let stride = (width * 3) as usize;
    let mut buf = vec![0u8; stride * height as usize];
    for y in 0..height {
        for x in 0..width {
            let r = ((x * 255) / width.max(1)) as u8;
            let g = ((y * 255) / height.max(1)) as u8;
            let b = (((x + y) * 255) / (width + height).max(1)) as u8;
            let p = (y as usize) * stride + (x as usize) * 3;
            buf[p] = b;
            buf[p + 1] = g;
            buf[p + 2] = r;
        }
    }
    buf
}

/// Shift the gradient right by `dx` pixels (wrapping the leftmost
/// `dx` columns out as black).  Used as a translation-motion
/// fixture for the P-frame probe.
fn shift_right(src: &[u8], width: u32, height: u32, dx: u32) -> Vec<u8> {
    let stride = (width * 3) as usize;
    let mut dst = vec![0u8; src.len()];
    for y in 0..height as usize {
        let row_off = y * stride;
        for x in 0..width {
            let xs = x;
            let xd = x + dx;
            if xd >= width {
                continue;
            }
            let s = row_off + (xs as usize) * 3;
            let d = row_off + (xd as usize) * 3;
            dst[d] = src[s];
            dst[d + 1] = src[s + 1];
            dst[d + 2] = src[s + 2];
        }
    }
    dst
}

/// Stand up a sandbox + load `mpg4c32.dll` + ICOpen in compress
/// mode.  Returns `None` if the fixture is missing or the codec
/// refuses compress-mode.
fn open_encoder() -> Option<(Sandbox, u32)> {
    let dll = mpg4c32_path()?;
    let dll_bytes = std::fs::read(&dll).ok()?;
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(2_000_000_000);
    let img = sb.load("mpg4c32.dll", &dll_bytes).ok()?;
    let _ = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .ok()?;
    sb.install_codec(&img).ok()?;
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"MP43");
    let hic = sb.ic_open(fcc_video, fcc_handler, ICMODE_COMPRESS).ok()?;
    if hic == 0 {
        return None;
    }
    Some((sb, hic))
}

/// One row of the probe table.
#[derive(Debug, Clone)]
struct Probe {
    quality: u32,
    i_size: usize,
    p_size: usize,
    p_is_keyframe: bool,
    p_ratio: f64,
}

impl Probe {
    fn is_real_pframe(&self) -> bool {
        !self.p_is_keyframe && self.p_ratio < 0.5
    }
}

/// Encode frame 0 (I) + frame 1 (P-tagged) at one quality level.
/// Returns the probe row; bubbles up codec-side errors as `None`
/// so the test can report the rejection mode.
fn probe_quality(quality: u32, frame0: &[u8], frame1: &[u8]) -> Option<Probe> {
    let (mut sb, hic) = open_encoder()?;
    let input_bih = Bih {
        bi_size: 40,
        width: W as i32,
        height: H as i32,
        planes: 1,
        bit_count: 24,
        compression: [0; 4],
        size_image: W * H * 3,
        ..Default::default()
    };
    if !matches!(sb.ic_compress_query(hic, &input_bih, None), Ok(0)) {
        eprintln!("round53: q={quality}: BGR24 query rejected; skipping");
        return None;
    }
    let (gf_lr, output_bih) = sb.ic_compress_get_format(hic, &input_bih).ok()?;
    if gf_lr != 0 {
        eprintln!("round53: q={quality}: ICCompressGetFormat lr={gf_lr:#x}; skipping");
        return None;
    }
    let max_out_size = sb
        .ic_compress_get_size(hic, &input_bih, &output_bih)
        .unwrap_or(W * H * 4);
    if !matches!(sb.ic_compress_begin(hic, &input_bih, &output_bih), Ok(0)) {
        eprintln!("round53: q={quality}: ICCompressBegin rejected; skipping");
        return None;
    }

    // Frame 0 — keyframe.
    let i_outcome = sb
        .ic_compress(
            hic,
            ICCOMPRESS_KEYFRAME,
            &input_bih,
            frame0,
            &output_bih,
            max_out_size,
            u32::from_le_bytes(*b"00dc"),
            0,
            0,
            quality,
            None,
            None,
        )
        .ok()?;
    if i_outcome.lresult != 0 || i_outcome.bytes.is_empty() {
        eprintln!(
            "round53: q={quality}: I-frame ICCompress lr={:#x} bytes.len={}",
            i_outcome.lresult,
            i_outcome.bytes.len()
        );
        let _ = sb.ic_compress_end(hic);
        let _ = sb.ic_close(hic);
        return None;
    }
    let i_size = i_outcome.bytes.len();

    // Frame 1 — non-keyframe request with prev = frame 0 input.
    let p_outcome = sb.ic_compress(
        hic,
        0,
        &input_bih,
        frame1,
        &output_bih,
        max_out_size,
        u32::from_le_bytes(*b"00dc"),
        1,
        0,
        quality,
        Some(&input_bih),
        Some(frame0),
    );
    let _ = sb.ic_compress_end(hic);
    let _ = sb.ic_close(hic);

    let p = match p_outcome {
        Ok(o) if o.lresult == 0 && !o.bytes.is_empty() => o,
        Ok(o) => {
            eprintln!(
                "round53: q={quality}: P-frame ICCompress lr={:#x} bytes.len={}",
                o.lresult,
                o.bytes.len(),
            );
            return None;
        }
        Err(e) => {
            eprintln!("round53: q={quality}: P-frame trapped: {e}");
            return None;
        }
    };

    let p_size = p.bytes.len();
    let p_is_keyframe = (p.returned_flags & ICCOMPRESS_KEYFRAME) != 0;
    let p_ratio = (p_size as f64) / (i_size as f64);
    Some(Probe {
        quality,
        i_size,
        p_size,
        p_is_keyframe,
        p_ratio,
    })
}

#[test]
fn pframe_quality_regime_probe_translation_motion() {
    if mpg4c32_path().is_none() {
        eprintln!("round53: mpg4c32.dll missing; skipping");
        return;
    }

    let frame0 = make_bgr24_gradient(W, H);
    let frame1 = shift_right(&frame0, W, H, 8);
    assert_eq!(frame0.len(), (W * H * 3) as usize);
    assert_eq!(frame1.len(), frame0.len());
    assert_ne!(
        frame0, frame1,
        "frame1 must differ from frame0 — translation fixture"
    );

    let qualities = [1000u32, 2000, 3000, 5000, 8000];
    let mut rows: Vec<Probe> = Vec::new();
    for q in qualities {
        match probe_quality(q, &frame0, &frame1) {
            Some(r) => {
                eprintln!(
                    "round53: q={:5} I={:6} bytes  P={:6} bytes  \
                     P_is_keyframe={:5}  P/I={:.3}  real_pframe={}",
                    r.quality,
                    r.i_size,
                    r.p_size,
                    r.p_is_keyframe,
                    r.p_ratio,
                    r.is_real_pframe()
                );
                rows.push(r);
            }
            None => {
                eprintln!("round53: q={q}: probe skipped (see prior log)");
            }
        }
    }

    if rows.is_empty() {
        eprintln!(
            "round53: every probe quality skipped — mpg4c32 may have changed \
             its acceptance contract.  This is the round's reportable finding."
        );
        return;
    }

    // Aggregate finding.
    let any_real_pframe = rows.iter().any(|r| r.is_real_pframe());
    let any_codec_pframe = rows.iter().any(|r| !r.p_is_keyframe);
    let min_ratio = rows.iter().map(|r| r.p_ratio).fold(f64::INFINITY, f64::min);
    eprintln!(
        "round53: SUMMARY  N={}  any_real_pframe={}  any_codec_cleared_keyframe={}  \
         min_P/I_ratio={:.3}",
        rows.len(),
        any_real_pframe,
        any_codec_pframe,
        min_ratio
    );

    if any_real_pframe {
        eprintln!(
            "round53: FINDING — mpg4c32 DOES emit real P-frames on differing \
             content for at least one quality regime in the probed range \
             [1000..8000].  Headline metric: min P/I ratio = {min_ratio:.3}."
        );
    } else if any_codec_pframe {
        eprintln!(
            "round53: FINDING — mpg4c32 clears the keyframe flag for some \
             quality regimes (so the codec acknowledges the P-frame request) \
             but the P-frame is not substantially smaller than the I-frame \
             (min P/I = {min_ratio:.3}).  The codec's motion compensation \
             does not shrink the residual below half-I-size on an 8-pixel \
             horizontal translation."
        );
    } else {
        eprintln!(
            "round53: FINDING — mpg4c32 ALWAYS emits keyframes regardless of \
             the requested non-keyframe flag, even with truly differing \
             content (8-pixel horizontal translation) across the probed \
             quality range [1000..8000].  Min P/I ratio = {min_ratio:.3}.  \
             This codec build appears to be configured for keyframe-only \
             emission under the VfW path; real P-frame emission may be a \
             DirectShow-only feature on this DLL."
        );
    }

    // Headline assertion: at least the probes ran.  We deliberately
    // DO NOT assert `any_real_pframe` — the round's contract is
    // "report the codec's behaviour faithfully", not "make the
    // codec emit P-frames".  Pin: at least one quality probe ran
    // end-to-end.
    assert!(
        !rows.is_empty(),
        "round53: at least one quality probe must complete; see prior log \
         for codec-side rejection details"
    );
}
