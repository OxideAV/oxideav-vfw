//! Round 29 — wire `oxideav_core::Decoder` for VfW codecs.
//!
//! Threads the real ICDecompressBegin → ICDecompress → ICDecompressEnd
//! handshake through the [`oxideav_core::Decoder`] trait that
//! [`oxideav_vfw::discovery::make_decoder`] returns. This test
//! exercises the trait path against the same MP43 fixtures the
//! round-24 manual `Sandbox::ic_decompress` path already validates,
//! and asserts the trait path produces byte-identical output to the
//! manual path.
//!
//! NEVER reference ffmpeg / libav / Wine / ReactOS source. ffmpeg
//! is used purely as a black-box oracle elsewhere in the suite; this
//! test compares the trait path against the in-tree manual path,
//! both of which call into the same `mpg4c32.dll` through our PE
//! emulator.

#![cfg(feature = "auto-discovery")]

mod common;

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, Frame, Packet, PixelFormat, TimeBase};
use oxideav_vfw::discovery::{
    codec_id_for, make_decoder, output_pixel_format, register_factory_for_id, DiscoveryRecord, Kind,
};
use oxideav_vfw::win32::vfw32::ICDECOMPRESS_NOTKEYFRAME;
use oxideav_vfw::{Bih, Sandbox};

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

fn fixture_path(stem: &str) -> Option<PathBuf> {
    let p = workspace_root()?.join(format!("docs/video/msmpeg4-fixtures/{stem}/input.avi"));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// Reference path: drive `n` frames through the manual
/// `Sandbox::ic_decompress` API. Mirrors the round-24
/// `decode_n_frames_mp43` helper but only collects the per-frame
/// output bytes (not metrics).
fn manual_decode_n_frames(
    dll_bytes: &[u8],
    avi_bytes: &[u8],
    fourcc: &[u8; 4],
    n: u32,
) -> Result<Vec<Vec<u8>>, String> {
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(8_000_000_000);
    let img = sb
        .load("codec.dll", dll_bytes)
        .map_err(|e| format!("load: {e}"))?;
    sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .map_err(|e| format!("DllMain: {e}"))?;
    sb.install_codec(&img)
        .map_err(|e| format!("install_codec: {e}"))?;

    let s0 = common::avi_extractor::extract_video_sample(avi_bytes, 0)
        .map_err(|e| format!("avi sample 0: {e}"))?;
    let width = s0.width;
    let height = s0.height;

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*fourcc);
    let hic = sb
        .ic_open(fcc_video, fcc_handler, 2)
        .map_err(|e| format!("ic_open: {e}"))?;
    if hic == 0 {
        return Err("ic_open returned 0".into());
    }

    let bih_in_template = Bih {
        bi_size: 40,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: *fourcc,
        size_image: 0,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
    };
    let bih_out = Bih {
        bi_size: 40,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: [0; 4],
        size_image: width * height * 3,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
    };
    let q = sb
        .ic_decompress_query(hic, &bih_in_template, Some(&bih_out))
        .map_err(|e| format!("query: {e}"))?;
    if q != 0 {
        return Err(format!("query → {q:#010x}"));
    }
    let b = sb
        .ic_decompress_begin(hic, &bih_in_template, &bih_out)
        .map_err(|e| format!("begin: {e}"))?;
    if b != 0 {
        return Err(format!("begin → {b:#010x}"));
    }

    let cap = bih_out.size_image;
    let mut frames: Vec<Vec<u8>> = Vec::new();
    for i in 0..n {
        let s = match common::avi_extractor::extract_video_sample(avi_bytes, i) {
            Ok(s) => s,
            Err(_) => break,
        };
        let bih_in = Bih {
            size_image: s.bytes.len() as u32,
            ..bih_in_template.clone()
        };
        let flags = if i == 0 { 0 } else { ICDECOMPRESS_NOTKEYFRAME };
        let (rc, out) = sb
            .ic_decompress(hic, flags, &bih_in, &s.bytes, &bih_out, cap)
            .map_err(|e| format!("decompress(s{i}): {e}"))?;
        if rc != 0 {
            return Err(format!("decompress(s{i}) → {rc:#010x}"));
        }
        frames.push(out);
    }
    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);
    Ok(frames)
}

/// Trait path: register a synthetic `DiscoveryRecord` pointing at
/// the fixture DLL, build a `Decoder` via [`make_decoder`], and
/// drive `send_packet` / `receive_frame` for `n` frames.
fn trait_decode_n_frames(
    dll_path: PathBuf,
    avi_bytes: &[u8],
    fourcc: &str,
    n: u32,
    codec_id_label: &str,
) -> Result<Vec<TraitFrame>, String> {
    // Stash a DiscoveryRecord in the global table so make_decoder
    // can find it via codec_id.
    let codec_id_str = format!(
        "vfw_{}_round29_test_{codec_id_label}",
        fourcc.to_lowercase()
    );
    register_factory_for_id(
        &codec_id_str,
        DiscoveryRecord {
            dll_path,
            fourcc: fourcc.to_string(),
            kind: Kind::Vfw,
            clsid: None,
        },
    );

    let s0 = common::avi_extractor::extract_video_sample(avi_bytes, 0)
        .map_err(|e| format!("avi sample 0: {e}"))?;
    let width = s0.width;
    let height = s0.height;

    let mut params = CodecParameters::video(CodecId::new(codec_id_str.clone()));
    params.width = Some(width);
    params.height = Some(height);
    params.pixel_format = Some(PixelFormat::Bgr24);

    let mut decoder = make_decoder(&params).map_err(|e| format!("make_decoder: {e}"))?;
    let mut out: Vec<TraitFrame> = Vec::new();
    for i in 0..n {
        let s = match common::avi_extractor::extract_video_sample(avi_bytes, i) {
            Ok(s) => s,
            Err(_) => break,
        };
        let mut packet = Packet::new(0, TimeBase::new(1, 25), s.bytes);
        packet = packet.with_keyframe(i == 0);
        decoder
            .send_packet(&packet)
            .map_err(|e| format!("send_packet(s{i}): {e}"))?;
        let frame = decoder
            .receive_frame()
            .map_err(|e| format!("receive_frame(s{i}): {e}"))?;
        match frame {
            Frame::Video(v) => {
                if v.planes.len() != 1 {
                    return Err(format!(
                        "trait frame {i}: expected 1 plane, got {}",
                        v.planes.len()
                    ));
                }
                let plane = v.planes.into_iter().next().unwrap();
                out.push(TraitFrame {
                    stride: plane.stride,
                    bytes: plane.data,
                });
            }
            other => return Err(format!("trait frame {i}: expected Video, got {other:?}")),
        }
    }
    Ok(out)
}

#[derive(Debug)]
struct TraitFrame {
    stride: usize,
    bytes: Vec<u8>,
}

/// Flip a manual-path bottom-up BGR24 buffer into the trait path's
/// top-down storage so the two can be byte-compared. Mirrors the
/// flip in `SandboxedVfwDecoder::receive_frame`.
fn flip_bottom_up_to_top_down(raw: &[u8], width: u32, height: u32) -> Vec<u8> {
    let stride = (width * 3) as usize;
    let h = height as usize;
    let mut out = vec![0u8; stride * h];
    for row in 0..h {
        let src = (h - 1 - row) * stride;
        let dst = row * stride;
        out[dst..dst + stride].copy_from_slice(&raw[src..src + stride]);
    }
    out
}

/// Drive both paths against `(dll_name, fixture_stem, fourcc, n)`
/// and assert byte-equality. Skip cleanly if either fixture is
/// missing.
fn run_byte_equality_check(
    dll_name: &str,
    fixture_stem: &str,
    fourcc: &str,
    fourcc_bytes: &[u8; 4],
    n: u32,
    codec_id_label: &str,
) {
    let Some(dll) = binary_path(dll_name) else {
        eprintln!("round29[{fixture_stem}]: {dll_name} missing; skipping");
        return;
    };
    let Some(avi) = fixture_path(fixture_stem) else {
        eprintln!("round29[{fixture_stem}]: avi missing; skipping");
        return;
    };
    let dll_bytes = std::fs::read(&dll).unwrap();
    let avi_bytes = std::fs::read(&avi).unwrap();

    // Read width/height once for the flip helper.
    let s0 = common::avi_extractor::extract_first_video_sample(&avi_bytes).unwrap();

    let manual =
        manual_decode_n_frames(&dll_bytes, &avi_bytes, fourcc_bytes, n).expect("manual path");
    let traited =
        trait_decode_n_frames(dll, &avi_bytes, fourcc, n, codec_id_label).expect("trait path");

    assert_eq!(
        manual.len(),
        traited.len(),
        "round29[{fixture_stem}]: manual decoded {} frames, trait decoded {}",
        manual.len(),
        traited.len(),
    );

    for (i, (m, t)) in manual.iter().zip(traited.iter()).enumerate() {
        let m_top = flip_bottom_up_to_top_down(m, s0.width, s0.height);
        assert_eq!(
            t.stride,
            (s0.width * 3) as usize,
            "round29[{fixture_stem}] f{i}: stride mismatch (got {}, want {})",
            t.stride,
            s0.width * 3,
        );
        assert_eq!(
            t.bytes.len(),
            m_top.len(),
            "round29[{fixture_stem}] f{i}: byte count mismatch (trait {} vs manual {})",
            t.bytes.len(),
            m_top.len(),
        );
        if t.bytes != m_top {
            // First-difference diagnostic.
            let mut diff = 0usize;
            let mut first_diff = None;
            for (k, (a, b)) in t.bytes.iter().zip(m_top.iter()).enumerate() {
                if a != b {
                    diff += 1;
                    if first_diff.is_none() {
                        first_diff = Some((k, *a, *b));
                    }
                }
            }
            panic!(
                "round29[{fixture_stem}] f{i}: trait vs manual diverged in {diff} bytes \
                 (first @ {first_diff:?}; total {})",
                t.bytes.len(),
            );
        }
        eprintln!(
            "round29[{fixture_stem}] f{i}: trait == manual ({} bytes)",
            t.bytes.len()
        );
    }
}

#[test]
fn output_pixel_format_is_bgr24() {
    assert_eq!(output_pixel_format(), PixelFormat::Bgr24);
}

#[test]
fn make_decoder_without_width_constructs_with_get_format_probe_path() {
    // Round 30 — width is now optional on `CodecParameters`. When
    // missing, the decoder probes the codec via
    // `ICM_DECOMPRESS_GET_FORMAT` on first `send_packet` and
    // populates dims from the codec's reply. `make_decoder` itself
    // succeeds; the failure (if any) surfaces from the probe.
    let id = "vfw_mp43_round30_test_no_width";
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path: PathBuf::from("/dev/null"),
            fourcc: "MP43".into(),
            kind: Kind::Vfw,
            clsid: None,
        },
    );
    let params = CodecParameters::video(CodecId::new(id));
    let decoder = make_decoder(&params).expect("make_decoder constructs lazily");
    assert_eq!(decoder.codec_id().as_str(), id);
}

#[test]
fn make_decoder_dshow_kind_now_constructs_lazily() {
    // Round 30 — DShow path constructs a `SandboxedDshowDecoder`
    // at make_decoder time. The real DLL load + IPin handshake
    // run on first `send_packet`; failure surfaces from
    // `receive_frame` carrying r31-followup diagnostics.
    let id = "vfw_round30_dshow_lazy_construct";
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path: PathBuf::from("/dev/null"),
            fourcc: "WMV3".into(),
            kind: Kind::DirectShow,
            clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".into()),
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(176);
    params.height = Some(144);
    let decoder = make_decoder(&params).expect("DShow make_decoder constructs lazily");
    assert_eq!(decoder.codec_id().as_str(), id);
}

/// `codec_id_for` is exposed through the `discovery` module so test
/// authors and downstream tooling can synthesise the same id the
/// `register()` cascade builds. Smoke-test it stays stable.
#[test]
fn codec_id_for_matches_round28_format() {
    let path = PathBuf::from("/some/path/MPG4C32.DLL");
    assert_eq!(codec_id_for(&path, "MP43"), "vfw_mp43_mpg4c32");
}

// ──────────────────────── End-to-end MP43 ────────────────────────

#[test]
fn mp43_trait_path_byte_equals_manual_path_gop_30() {
    run_byte_equality_check(
        "mpg4c32.dll",
        "gop-30-352x288",
        "MP43",
        b"MP43",
        3,
        "gop_30",
    );
}

#[test]
fn mp43_trait_path_byte_equals_manual_path_with_skip_mbs() {
    run_byte_equality_check(
        "mpg4c32.dll",
        "with-skip-mbs-352x288",
        "MP43",
        b"MP43",
        3,
        "with_skip_mbs",
    );
}

#[test]
fn mp43_trait_path_byte_equals_manual_path_motion_pan() {
    run_byte_equality_check(
        "mpg4c32.dll",
        "motion-pan-352x288",
        "MP43",
        b"MP43",
        3,
        "motion_pan",
    );
}

#[test]
fn mp43_trait_path_byte_equals_manual_path_intra_pred() {
    run_byte_equality_check(
        "mpg4c32.dll",
        "intra-pred-active-352x288",
        "MP43",
        b"MP43",
        1,
        "intra_pred",
    );
}

#[test]
fn mp43_trait_path_first_frame_returns_bgr24_video_frame() {
    // Light-weight smoke test: just confirm one keyframe surfaces
    // as a `Frame::Video` with a single 24bpp plane and the right
    // byte budget. Skips when the fixture is missing.
    let Some(dll) = binary_path("mpg4c32.dll") else {
        eprintln!("round29: mpg4c32.dll missing; skipping");
        return;
    };
    let Some(avi) = fixture_path("gop-30-352x288") else {
        eprintln!("round29: gop-30 avi missing; skipping");
        return;
    };
    let avi_bytes = std::fs::read(&avi).unwrap();
    let frames =
        trait_decode_n_frames(dll, &avi_bytes, "MP43", 1, "first_frame_smoke").expect("trait path");
    assert_eq!(frames.len(), 1);
    let f = &frames[0];
    assert_eq!(f.stride, 352 * 3);
    assert_eq!(f.bytes.len(), 352 * 288 * 3);
    let nz = f.bytes.iter().filter(|&&b| b != 0).count();
    assert!(
        nz > f.bytes.len() / 4,
        "round29: trait keyframe should have >25% non-zero bytes (nz={nz} of {})",
        f.bytes.len(),
    );
}
