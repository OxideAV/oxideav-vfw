//! Round 21 — drive ICOpen + ICDecompressBegin + ICDecompress
//! against an MS-MPEG-4 v3 keyframe.
//!
//! The earlier rounds proved the integer + MMX dispatch
//! surface is sufficient for IV50 / IV41 (Indeo 4 + 5);
//! round-21 lit up x87 FPU semantics + lower-cased the
//! ICOPEN fcc fields, which together unblocked the strict
//! mpg4c32 DRV_OPEN handler.
//!
//! Reach goal: `ICDecompress(hic, FLAG_KEYFRAME, ...)` returns
//! `ICERR_OK` and the codec writes some bytes to the output
//! buffer. Bit-perfect cross-checking against
//! `expected.yuv` is deferred — the round-21 milestone is
//! "the codec actually executes its keyframe-decode path".
//!
//! Reference docs: `docs/winmf/winmf-emulator.md` §"Milestone
//! 3.1", MSDN `ICDecompress` documentation.

mod common;

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

fn fixture_path() -> Option<PathBuf> {
    let p = workspace_root()?.join("docs/video/msmpeg4-fixtures/fourcc-MP43/input.avi");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

#[test]
fn mp43_drv_open_returns_nonzero_hic() {
    let Some(p) = mpg4c32_path() else {
        eprintln!("round21: mpg4c32.dll missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(500_000_000);
    let img = sb.load("mpg4c32.dll", &bytes).unwrap();
    sb.host.trace_stubs = true;

    let _ = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .unwrap();
    sb.install_codec(&img).unwrap();

    let fcc_video = u32::from_le_bytes(*b"VIDC"); // host-side; vfw32 lower-cases
    let fcc_handler = u32::from_le_bytes(*b"MP43"); // ditto
    let hic = sb
        .ic_open(fcc_video, fcc_handler, 2 /* ICMODE_DECOMPRESS */)
        .unwrap();
    assert_ne!(hic, 0, "ICOpen('VIDC','MP43') should return a non-zero hic");
    eprintln!("round21: hic={hic:#010x}");
    let _ = sb.ic_close(hic);
}

#[test]
fn mp43_keyframe_decompress_through_real_codec() {
    let Some(dll) = mpg4c32_path() else {
        eprintln!("round21: mpg4c32.dll missing; skipping");
        return;
    };
    let Some(avi) = fixture_path() else {
        eprintln!("round21: fourcc-MP43 fixture missing; skipping");
        return;
    };
    let dll_bytes = std::fs::read(&dll).unwrap();
    let avi_bytes = std::fs::read(&avi).unwrap();

    // Extract the first video sample (= the keyframe) from
    // the AVI fixture.
    let sample = match common::avi_extractor::extract_first_video_sample(&avi_bytes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("round21: extract first sample failed: {e}");
            return;
        }
    };
    eprintln!(
        "round21: fixture: fourcc={:?}, {}x{}, sample 0 = {} bytes",
        std::str::from_utf8(&sample.codec_fourcc.to_le_bytes()).unwrap_or("?"),
        sample.width,
        sample.height,
        sample.sample_size,
    );
    assert_eq!(sample.codec_fourcc, u32::from_le_bytes(*b"MP43"));

    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(2_000_000_000);
    let img = sb.load("mpg4c32.dll", &dll_bytes).unwrap();
    let _ = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .unwrap();
    sb.install_codec(&img).unwrap();

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"MP43");
    let hic = sb.ic_open(fcc_video, fcc_handler, 2).unwrap();
    if hic == 0 {
        eprintln!("round21: ICOpen rejected MP43 — bailing");
        return;
    }
    eprintln!("round21: hic={hic:#010x}");

    // Build input + output BITMAPINFOHEADER's. Input format:
    // MP43, the YUV4:2:0 fourcc-tagged keyframe. Output: YUY2
    // (mpg4c32 supports YUY2 natively per the ICM_DECOMPRESS_QUERY
    // path).
    let input = Bih {
        bi_size: 40,
        width: sample.width as i32,
        height: sample.height as i32,
        planes: 1,
        bit_count: 24,
        compression: *b"MP43",
        size_image: sample.sample_size,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
    };
    // RGB24 BI_RGB output — vfw32 codecs are required to
    // support 24-bit packed RGB as a fallback decompression
    // target.
    let output = Bih {
        bi_size: 40,
        width: sample.width as i32,
        height: sample.height as i32,
        planes: 1,
        bit_count: 24,
        compression: [0; 4], // BI_RGB
        size_image: sample.width * sample.height * 3,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
    };
    let q = sb.ic_decompress_query(hic, &input, Some(&output));
    eprintln!("round21: ICDecompressQuery → {q:?}");
    assert_eq!(
        q.unwrap(),
        0,
        "round21: ICDecompressQuery should return ICERR_OK"
    );
    let begin = sb
        .ic_decompress_begin(hic, &input, &output)
        .expect("round22: ICDecompressBegin must not trap");
    eprintln!("round22: ICDecompressBegin → {begin:#x}");
    assert_eq!(
        begin, 0,
        "round22: ICDecompressBegin should return ICERR_OK (= 0). \
         Round 21 left this returning -100 (ICERR_INTERNAL); round \
         22 unblocked it via the v3 wrapper-handshake plant + \
         FSIN/FCOS/FRNDINT/FSCALE/FPREM x87 sub-forms."
    );
    let cap = output.size_image;
    let result = sb.ic_decompress(hic, 0, &input, &sample.bytes, &output, cap);
    match result {
        Ok((rc, out)) => {
            eprintln!(
                "round22: ICDecompress rc={rc:#x}, output {} bytes (first 32: {:02x?})",
                out.len(),
                &out[..32.min(out.len())]
            );
            assert_eq!(
                rc, 0,
                "round22: ICDecompress should return ICERR_OK on the first keyframe"
            );
            assert_eq!(out.len() as u32, cap);
            // Round-22 reach goal: confirm the codec wrote
            // SOMETHING into the output buffer (vs. early-exiting
            // with a zero-fill from `arena_alloc`). Bit-perfect
            // YUV / RGB-24 cross-checking against an
            // ffmpeg-reference is deferred — round 22's milestone
            // is "the codec actually executes its keyframe-decode
            // body".
            let any_nonzero = out.iter().take(1024).any(|&b| b != 0);
            assert!(
                any_nonzero,
                "round22: ICDecompress wrote 0 non-zero bytes — output buffer untouched"
            );
        }
        Err(e) => panic!(
            "round22: ICDecompress trapped: {e}; eip={:#010x}",
            sb.cpu.regs.eip
        ),
    }
    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);
}
