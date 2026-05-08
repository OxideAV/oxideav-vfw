//! Round 23 — bit-exact (or PSNR-bounded) cross-check of the
//! mpg4c32 v3 keyframe decode against ffmpeg's reference output,
//! plus extension to a 2-frame I+P sequence.
//!
//! Round 22 closed `ICDecompressBegin` + landed the first MP43
//! keyframe decode (`ICERR_OK`, 76 032 B output for 176×144). The
//! sanity check at the time was "any non-zero byte in the first 1
//! KiB". Round 23 raises the bar:
//!
//!  A. Re-decode the same `fourcc-MP43/input.avi` keyframe and
//!     compare the BGR24 output against ffmpeg's
//!     `-pix_fmt bgr24 -f rawvideo` rendering of the same packet.
//!     Bit-exact when the buffers match; otherwise compute PSNR
//!     and pass when PSNR >= 30 dB (ample headroom for the
//!     YUV→BGR matrix difference that's expected between
//!     mpg4c32's internal converter and ffmpeg's swscale).
//!     Skipped when ffmpeg is not installed (`which ffmpeg` is
//!     not on `$PATH`) — the assertion only fires when the oracle
//!     is reachable.
//!
//!  B. Decode frames 0 and 1 of the
//!     `i-frame-then-p-frame-176x144` fixture (an I + P pair
//!     produced with `-vtag DIV3`; the elementary bitstream is a
//!     vanilla MSMPEG4 v3 stream, accepted by mpg4c32 with the
//!     fcc-handler tagged as `MP43`). Each ICDecompress call
//!     should return `ICERR_OK`; the codec maintains its
//!     reference-frame state across calls; sample 1 is fed with
//!     `ICDECOMPRESS_NOTKEYFRAME` per round-13's pattern.
//!
//! NEVER reference ffmpeg / libav / Wine source. ffmpeg is used
//! purely as a black-box oracle here, the same way round 17's
//! corpus walker uses ffmpeg-decoded `.yuv` snapshots as the
//! ground truth without inspecting libav's decoder internals.

mod common;

use oxideav_vfw::{Bih, Sandbox};
use std::path::PathBuf;
use std::process::Command;

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

fn mp43_fixture_path() -> Option<PathBuf> {
    let p = workspace_root()?.join("docs/video/msmpeg4-fixtures/fourcc-MP43/input.avi");
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

/// Locate `ffmpeg` on `PATH`. Returns `None` when ffmpeg is not
/// available (CI on a runner without ffmpeg, dev box with no
/// install). The oracle assertion is skipped gracefully in that
/// case.
fn ffmpeg_on_path() -> Option<PathBuf> {
    // `which ffmpeg`-equivalent that doesn't shell out — walk
    // `PATH` ourselves so the test stays portable.
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join("ffmpeg");
        if cand.is_file() {
            return Some(cand);
        }
        // Some hosts ship `ffmpeg.exe` (Windows). Cover that too.
        let cand_exe = dir.join("ffmpeg.exe");
        if cand_exe.is_file() {
            return Some(cand_exe);
        }
    }
    None
}

/// Invoke ffmpeg as a black-box validator: render the FIRST
/// video frame of `avi_path` as raw BGR24 (the same pixel format
/// the round-21 / round-22 ICDecompress test asks mpg4c32 to
/// produce). Returns the raw 3·W·H byte buffer.
///
/// `ffmpeg -hide_banner -loglevel error -i <avi> -frames:v 1
///         -pix_fmt bgr24 -f rawvideo -`
fn ffmpeg_decode_first_frame_bgr24(
    ffmpeg: &PathBuf,
    avi_path: &PathBuf,
) -> Result<Vec<u8>, String> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(avi_path)
        .args(["-frames:v", "1", "-pix_fmt", "bgr24", "-f", "rawvideo", "-"])
        .output()
        .map_err(|e| format!("ffmpeg spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffmpeg exit {:?}; stderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(out.stdout)
}

/// Per-channel PSNR (dB) for two equal-length BGR24 buffers.
/// Returns `f64::INFINITY` when the buffers are bit-identical.
fn psnr_db(actual: &[u8], reference: &[u8]) -> f64 {
    assert_eq!(actual.len(), reference.len(), "buffers must be same size");
    let mut sse: u64 = 0;
    for (a, r) in actual.iter().zip(reference.iter()) {
        let d = *a as i32 - *r as i32;
        sse += (d * d) as u64;
    }
    if sse == 0 {
        return f64::INFINITY;
    }
    let mse = sse as f64 / actual.len() as f64;
    10.0 * (255.0_f64 * 255.0 / mse).log10()
}

/// Drive mpg4c32: open + begin + return (hic, decoded BGR24,
/// width, height, sample-1-bytes-if-present). Used by both
/// sub-tests; sub-test A only consumes the keyframe + buffer,
/// sub-test B feeds the second sample through the same hic.
fn decode_with_mpg4c32_two_frames(
    dll_bytes: &[u8],
    avi_bytes: &[u8],
    expect_two: bool,
) -> Result<DecodeOutcome, String> {
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(4_000_000_000);
    let img = sb
        .load("mpg4c32.dll", dll_bytes)
        .map_err(|e| format!("load: {e}"))?;
    sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .map_err(|e| format!("DllMain: {e}"))?;
    sb.install_codec(&img)
        .map_err(|e| format!("install_codec: {e}"))?;

    let s0 = common::avi_extractor::extract_video_sample(avi_bytes, 0)
        .map_err(|e| format!("avi sample 0: {e}"))?;
    let s1_opt = if expect_two {
        match common::avi_extractor::extract_video_sample(avi_bytes, 1) {
            Ok(s) => Some(s),
            Err(e) => return Err(format!("avi sample 1: {e}")),
        }
    } else {
        None
    };
    let width = s0.width;
    let height = s0.height;

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"MP43");
    let hic = sb
        .ic_open(fcc_video, fcc_handler, 2)
        .map_err(|e| format!("ic_open: {e}"))?;
    if hic == 0 {
        return Err("ic_open returned 0 (codec rejected MP43)".into());
    }

    let input_sample0 = Bih {
        bi_size: 40,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: *b"MP43",
        size_image: s0.bytes.len() as u32,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
    };
    let output = Bih {
        bi_size: 40,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: [0; 4], // BI_RGB
        size_image: width * height * 3,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
    };
    let q = sb
        .ic_decompress_query(hic, &input_sample0, Some(&output))
        .map_err(|e| format!("ic_decompress_query: {e}"))?;
    if q != 0 {
        return Err(format!("ic_decompress_query → {q:#010x} (want 0)"));
    }
    let begin = sb
        .ic_decompress_begin(hic, &input_sample0, &output)
        .map_err(|e| format!("ic_decompress_begin: {e}"))?;
    if begin != 0 {
        return Err(format!("ic_decompress_begin → {begin:#010x} (want 0)"));
    }

    let cap = output.size_image;
    let (rc0, out0) = sb
        .ic_decompress(hic, 0, &input_sample0, &s0.bytes, &output, cap)
        .map_err(|e| format!("ic_decompress(s0): {e}"))?;
    if rc0 != 0 {
        return Err(format!("ic_decompress(s0) → {rc0:#010x} (want 0)"));
    }

    let mut frame1: Option<(u32, Vec<u8>)> = None;
    if let Some(s1) = s1_opt {
        let input_sample1 = Bih {
            size_image: s1.bytes.len() as u32,
            ..input_sample0.clone()
        };
        let pre = sb.cpu.instr_count;
        let result = sb.ic_decompress(
            hic,
            oxideav_vfw::win32::vfw32::ICDECOMPRESS_NOTKEYFRAME,
            &input_sample1,
            &s1.bytes,
            &output,
            cap,
        );
        let elapsed = sb.cpu.instr_count.saturating_sub(pre);
        match result {
            Ok((rc1, out1)) => {
                eprintln!(
                    "round23: P-frame {} bytes input, lr={:#010x}, {} non-zero, {} instrs",
                    s1.bytes.len(),
                    rc1,
                    out1.iter().filter(|&&b| b != 0).count(),
                    elapsed,
                );
                frame1 = Some((rc1, out1));
            }
            Err(e) => return Err(format!("ic_decompress(s1) trapped: {e}")),
        }
    }

    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);

    Ok(DecodeOutcome {
        width,
        height,
        frame0: out0,
        frame0_rc: rc0,
        frame1,
    })
}

#[derive(Debug)]
struct DecodeOutcome {
    width: u32,
    height: u32,
    frame0: Vec<u8>,
    frame0_rc: u32,
    /// `Some((lresult, bytes))` when a second-sample decode was
    /// attempted; `None` when only sample 0 was decoded.
    frame1: Option<(u32, Vec<u8>)>,
}

// ---- A: bit-exact / PSNR keyframe oracle -------------------------------

#[test]
fn mp43_keyframe_matches_ffmpeg_oracle_psnr() {
    let Some(dll) = mpg4c32_path() else {
        eprintln!("round23: mpg4c32.dll missing; skipping");
        return;
    };
    let Some(avi) = mp43_fixture_path() else {
        eprintln!("round23: fourcc-MP43 fixture missing; skipping");
        return;
    };
    let dll_bytes = std::fs::read(&dll).unwrap();
    let avi_bytes = std::fs::read(&avi).unwrap();

    let outcome = match decode_with_mpg4c32_two_frames(&dll_bytes, &avi_bytes, false) {
        Ok(o) => o,
        Err(e) => panic!("round23 A: decode failed: {e}"),
    };
    assert_eq!(outcome.frame0_rc, 0, "round23 A: keyframe rc must be 0");
    assert_eq!(
        outcome.frame0.len() as u32,
        outcome.width * outcome.height * 3,
        "round23 A: keyframe BGR24 buffer size",
    );
    let nonzero = outcome.frame0.iter().filter(|&&b| b != 0).count();
    assert!(
        nonzero > outcome.frame0.len() / 4,
        "round23 A: keyframe expected > 25% non-zero bytes ({}/{})",
        nonzero,
        outcome.frame0.len()
    );

    let Some(ffmpeg) = ffmpeg_on_path() else {
        eprintln!("round23 A: ffmpeg not on PATH — skipping oracle assertion");
        return;
    };
    eprintln!("round23 A: ffmpeg = {}", ffmpeg.display());

    let oracle = match ffmpeg_decode_first_frame_bgr24(&ffmpeg, &avi) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("round23 A: ffmpeg decode skipped — {e}");
            return;
        }
    };
    assert_eq!(
        oracle.len(),
        outcome.frame0.len(),
        "round23 A: ffmpeg oracle vs ours buffer-size mismatch ({} vs {})",
        oracle.len(),
        outcome.frame0.len(),
    );

    if oracle == outcome.frame0 {
        eprintln!("round23 A: BIT-EXACT match against ffmpeg oracle");
        return;
    }

    let psnr = psnr_db(&outcome.frame0, &oracle);
    let differ = oracle
        .iter()
        .zip(outcome.frame0.iter())
        .filter(|(a, b)| a != b)
        .count();
    eprintln!(
        "round23 A: oracle differs from ours in {} of {} bytes; PSNR = {:.2} dB",
        differ,
        oracle.len(),
        psnr
    );
    eprintln!(
        "round23 A: first 12 bytes — ours={:02x?}, oracle={:02x?}",
        &outcome.frame0[..12],
        &oracle[..12]
    );

    // YUV→BGR conversion floor between mpg4c32's internal
    // converter and ffmpeg's swscale is the only expected source
    // of drift on a flat-color keyframe. 30 dB is a comfortable
    // floor — visually-indistinguishable rendering of solid color
    // sits well above 40 dB. If the codec ever takes a structural
    // wrong turn (mis-decoded slice header, MV pred mismatch),
    // PSNR collapses to single digits.
    assert!(
        psnr >= 30.0,
        "round23 A: PSNR {:.2} dB < 30 dB floor (oracle vs ours diverged structurally)",
        psnr
    );
}

// ---- B: I + P sequential decode through frames 0..N -------------------

#[test]
fn mp43_i_plus_p_two_frame_decode() {
    let Some(dll) = mpg4c32_path() else {
        eprintln!("round23 B: mpg4c32.dll missing; skipping");
        return;
    };
    let Some(avi) = ip_fixture_path() else {
        eprintln!("round23 B: i-frame-then-p-frame fixture missing; skipping");
        return;
    };
    let dll_bytes = std::fs::read(&dll).unwrap();
    let avi_bytes = std::fs::read(&avi).unwrap();

    let outcome = match decode_with_mpg4c32_two_frames(&dll_bytes, &avi_bytes, true) {
        Ok(o) => o,
        Err(e) => panic!("round23 B: decode failed: {e}"),
    };
    assert_eq!(
        outcome.frame0_rc, 0,
        "round23 B: I-frame ICDecompress must return ICERR_OK"
    );
    let nz0 = outcome.frame0.iter().filter(|&&b| b != 0).count();
    assert!(
        nz0 > outcome.frame0.len() / 4,
        "round23 B: I-frame expected > 25% non-zero output ({}/{})",
        nz0,
        outcome.frame0.len(),
    );
    let (rc1, out1) = outcome
        .frame1
        .as_ref()
        .expect("round23 B: P-frame outcome must be recorded");
    assert_eq!(
        *rc1, 0,
        "round23 B: P-frame ICDecompress must return ICERR_OK (got {rc1:#010x})"
    );
    let nz1 = out1.iter().filter(|&&b| b != 0).count();
    assert!(
        nz1 > out1.len() / 4,
        "round23 B: P-frame expected > 25% non-zero output ({}/{})",
        nz1,
        out1.len(),
    );

    eprintln!(
        "round23 B: I-frame {} non-zero / P-frame {} non-zero (cap = {})",
        nz0,
        nz1,
        outcome.frame0.len(),
    );

    // PSNR vs the per-frame ffmpeg BGR24 oracle is informational
    // only — the I+P fixture used here is a `-vtag DIV3` encode,
    // ffmpeg routes it through msmpeg4v3 just like the codec
    // does. We don't assert a PSNR floor here because the test
    // is gating "the codec accepts a P-frame and produces ICERR_OK
    // output"; bit-exact comparison comes online once round-24+
    // brings the host-side YUV→BGR converter (mpg4c32 ships its
    // own) under regression coverage.
    let Some(ffmpeg) = ffmpeg_on_path() else {
        return;
    };
    let oracle0 = match ffmpeg_decode_first_frame_bgr24(&ffmpeg, &avi) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("round23 B: ffmpeg oracle skipped — {e}");
            return;
        }
    };
    if oracle0.len() == outcome.frame0.len() {
        let psnr = psnr_db(&outcome.frame0, &oracle0);
        eprintln!(
            "round23 B: I-frame PSNR vs ffmpeg oracle = {:.2} dB (informational)",
            psnr
        );
    }
}

// ---- state-field audit (informational, no fixed assertion) ----

/// Inspect the codec's per-instance state at the offsets where the
/// round-22 disasm flagged read-out activity for the
/// later-decode paths: `[esi+0xa0..0xc0]`, plus the relocation
/// target `[esi+0x15b0..0x15c4]` written at
/// `mpg4c32!DriverProc+0x2b41`. Used by the round-23 audit to
/// verify the wrapper-handshake plant lands in the field range
/// the decoder reads back from. Not a regression assertion —
/// printed for the trace log.
#[test]
fn mp43_state_field_audit() {
    let Some(dll) = mpg4c32_path() else {
        eprintln!("round23 audit: mpg4c32.dll missing; skipping");
        return;
    };
    let Some(avi) = mp43_fixture_path() else {
        eprintln!("round23 audit: fixture missing; skipping");
        return;
    };
    let dll_bytes = std::fs::read(&dll).unwrap();
    let avi_bytes = std::fs::read(&avi).unwrap();

    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(4_000_000_000);
    let img = sb.load("mpg4c32.dll", &dll_bytes).unwrap();
    sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .unwrap();
    sb.install_codec(&img).unwrap();

    let s0 = common::avi_extractor::extract_first_video_sample(&avi_bytes).unwrap();
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"MP43");
    let hic = sb.ic_open(fcc_video, fcc_handler, 2).unwrap();
    if hic == 0 {
        eprintln!("round23 audit: ICOpen rejected; bailing");
        return;
    }
    let driver_id = sb.host.hics.get(&hic).map(|e| e.driver_id).unwrap_or(0);
    eprintln!("round23 audit: driver_id = {driver_id:#010x}");

    let input = Bih {
        bi_size: 40,
        width: s0.width as i32,
        height: s0.height as i32,
        planes: 1,
        bit_count: 24,
        compression: *b"MP43",
        size_image: s0.bytes.len() as u32,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
    };
    let output = Bih {
        bi_size: 40,
        width: s0.width as i32,
        height: s0.height as i32,
        planes: 1,
        bit_count: 24,
        compression: [0; 4],
        size_image: s0.width * s0.height * 3,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
    };
    let _ = sb.ic_decompress_query(hic, &input, Some(&output));

    // Snapshot the state region BEFORE / AFTER ICDecompressBegin
    // so we can see what the codec writes inside [esi+0xa0..+0xc8]
    // and the relocation target at [esi+0x15b0..+0x15c4].
    let snap = |sb: &Sandbox, off: u32, len: usize| -> Vec<u8> {
        let mut v = vec![0u8; len];
        for (i, b) in v.iter_mut().enumerate() {
            *b = sb.mmu.load8(driver_id + off + i as u32).unwrap_or(0);
        }
        v
    };
    let pre_a0 = snap(&sb, 0xa0, 0x28); // [+0xa0..+0xc8] = 40 bytes
    let pre_15b0 = snap(&sb, 0x15b0, 0x14); // [+0x15b0..+0x15c4]
    eprintln!("round23 audit: pre  [+0xa0..+0xc8]   = {pre_a0:02x?}");
    eprintln!("round23 audit: pre  [+0x15b0..+0x15c4] = {pre_15b0:02x?}");

    let begin = sb.ic_decompress_begin(hic, &input, &output).unwrap();
    eprintln!("round23 audit: ICDecompressBegin -> {begin:#010x}");
    let post_a0 = snap(&sb, 0xa0, 0x28);
    let post_15b0 = snap(&sb, 0x15b0, 0x14);
    eprintln!("round23 audit: post [+0xa0..+0xc8]   = {post_a0:02x?}");
    eprintln!("round23 audit: post [+0x15b0..+0x15c4] = {post_15b0:02x?}");

    let cap = output.size_image;
    let result = sb.ic_decompress(hic, 0, &input, &s0.bytes, &output, cap);
    let after_decompress_a0 = snap(&sb, 0xa0, 0x28);
    let after_decompress_15b0 = snap(&sb, 0x15b0, 0x14);
    eprintln!("round23 audit: ICDecompress      -> {result:?}");
    eprintln!("round23 audit: post-decompress [+0xa0..+0xc8]   = {after_decompress_a0:02x?}");
    eprintln!("round23 audit: post-decompress [+0x15b0..+0x15c4] = {after_decompress_15b0:02x?}");

    // The PRE snapshot is taken *before* ICDecompressBegin, so the
    // codec hasn't seen any state writes yet — these bytes are
    // whatever DRV_OPEN's `malloc(0xc8)` left in the buffer (the
    // codec zero-fills, so `pre` is normally all zeros). The POST
    // snapshot is the interesting one: it must show the round-22
    // wrapper-handshake plant at [+0xb4..+0xc8] (sentinel == 1
    // followed by the 16-byte GUID), AND any additional fields the
    // BEGIN handler writes inside the same window. Verify the plant
    // landed bit-perfectly in the POST snapshot.
    assert_eq!(
        u32::from_le_bytes(post_a0[0x14..0x18].try_into().unwrap()),
        1,
        "round23 audit: post [+0xb4] sentinel != 1 — wrapper-handshake plant regressed",
    );
    let post_guid = &post_a0[0x18..0x28];
    let want_guid: [u8; 16] = [
        0x30, 0x6e, 0xc6, 0xb4, 0x80, 0x01, 0xd3, 0x11, 0xbb, 0xc6, 0x00, 0x60, 0x08, 0x32, 0x00,
        0x64,
    ];
    assert_eq!(
        post_guid, &want_guid,
        "round23 audit: post [+0xb8..+0xc8] GUID mismatch — wrapper-handshake plant regressed",
    );

    // Round-23 audit findings (informational, traced for the
    // commit log, no fixed assertion):
    //
    // * `[+0xa0..+0xb4]` — codec writes `01 00 00 00` at offset
    //   `+0xa4` during ICDecompressBegin. Likely a "frame index"
    //   or "beginframe-state ready" sentinel.
    // * `[+0xb4]` — refcount sentinel from the round-22 plant
    //   (==1).
    // * `[+0xb8..+0xc8]` — 16-byte GUID
    //   `b4c66e30-0180-11d3-bbc6-006008320064`.
    // * `[+0x15b0..+0x15c4]` — copy target documented in round-22's
    //   notes (mpg4c32!DriverProc+0x2b41). This region remains
    //   zero through both BEGIN and the keyframe DECOMPRESS — i.e.
    //   the copy path the round-22 audit suspected for state
    //   relocation does NOT fire on a sane MP43 keyframe under our
    //   wrapper-plant. No additional planting is needed.

    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);
}
