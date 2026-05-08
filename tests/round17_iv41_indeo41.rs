//! Round 17 Part B — drive a **larger** IV41 fixture through
//! `IR41_32.AX` and surface its MMX dispatch count.
//!
//! Round 16 demonstrated 8 sequential `crashtest.avi` IV41
//! frames (240×180 yuv410p) decode `ICERR_OK` through the
//! emulator. The MMX dispatch counter stayed at 0 across all 8
//! frames, matching round 14's IR50 finding: `IR41_32.AX` is
//! statically integer-only.
//!
//! Round 17 Part B widens the test surface to `indeo41.avi`
//! (320×240 yuv410p, 13.4 MB), the next-larger IV41 fixture
//! published in the `samples.oxideav.org/ffmpeg/V-codecs/IV41`
//! directory. The hypothesis: a larger frame's motion-comp loop
//! has more macroblocks per frame — if `IR41_32.AX` carried any
//! MMX block-copy / motion-comp specialisation, the larger
//! fixture would surface it.
//!
//! ## Acceptance gates
//!
//! 1. Sample 0 (keyframe) — `ICDecompress` returns `ICERR_OK`
//!    with > 25 % non-zero RGB24 output.
//! 2. Sample 1 (first P-frame) — same milestone as round 16.
//! 3. At least 4 of 8 sequential frames must return `ICERR_OK`.
//! 4. `mmx_dispatch_count` is recorded to stderr and a
//!    `Round-17 MMX dispatch finding:` summary line is logged
//!    at the end. The MMX count is **not asserted**: round-13's
//!    module is correct semantics waiting for a binary that
//!    uses it.
//!
//! ## Round-17 expectation
//!
//! Round 14's byte scan of `IR50_32.DLL` found zero `0F D0..FF`
//! occurrences across 184 KB. The round-17 sibling test
//! `round17_corpus_specgap.rs` runs the same scan over
//! `IR41_32.AX` — see that test's stderr for the 0F D0..FF
//! count. If the count is 0, no fixture size will trigger MMX
//! dispatch; the test will record `mmx_dispatch_count = 0` and
//! confirm the SPECGAP. If the count is non-zero, this test
//! becomes the validation pathway for the MMX module's
//! semantics in real-codec context.
//!
//! ## Reference docs (clean-room)
//!
//! * Microsoft RIFF + AVI 1.0 specs.
//! * Microsoft VfW SDK header (`vfw.h`).
//! * `docs/video/indeo/indeo4/wiki/Indeo_4.wiki`.
//!
//! NEVER reference `libavcodec/indeo4.c`, Wine's
//! `dlls/quartz`, ReactOS, or any third-party Indeo decoder.

mod common;

use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};
use oxideav_vfw::Sandbox;

#[test]
fn indeo41_320x240_decodes_with_mmx_dispatch_recorded() {
    /// vfw.h: `ICMODE_DECOMPRESS = 2`.
    const ICMODE_DECOMPRESS: u32 = 2;
    /// Mirror round-16 / round-13 cadence — 8 sequential frames
    /// is enough to see motion-comp paths surface but stays
    /// within CI's wall-clock budget.
    const NUM_FRAMES: u32 = 8;

    let dll_bytes = common::fetch_or_load("IR41_32.AX").expect("fetch IR41_32.AX");
    let avi =
        common::fetch_or_load_ffmpeg_sample("IV41", "indeo41.avi").expect("fetch indeo41.avi");

    let s0 =
        common::avi_extractor::extract_first_video_sample(&avi).expect("AVI walker on indeo41.avi");
    let width: u32 = s0.width;
    let height: u32 = s0.height;
    eprintln!("round17B fixture: indeo41.avi {width}×{height}");
    assert_eq!(
        s0.codec_fourcc,
        u32::from_le_bytes(*b"IV41"),
        "round-17B expected indeo41.avi sample-0 fourcc to be IV41",
    );
    // Round-17B specifically tests a LARGER fixture than round 16's
    // 240×180 crashtest.avi. If the corpus ever swaps indeo41.avi
    // for a smaller alternate, this assertion catches the regression.
    assert!(
        width * height > 240 * 180,
        "round-17B precondition: indeo41.avi must be larger than \
         round-16's 240×180 crashtest.avi (got {width}×{height})"
    );

    let mut sb = Sandbox::new();
    // 320×240 has ~75% more macroblocks than 240×180; round 16
    // used 600M instructions at 240×180 for 8 frames, so 1.2G
    // is a safe headroom for 8 frames at 320×240.
    sb.cpu.set_instr_limit(1_200_000_000);
    let img = sb.load("IR41_32.AX", &dll_bytes).expect("load IR41_32.AX");

    let _ = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .expect("DllMain");
    sb.install_codec(&img).expect("install_codec");

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"IV41");
    let hic = sb
        .ic_open(fcc_video, fcc_handler, ICMODE_DECOMPRESS)
        .expect("ic_open IV41");
    assert_ne!(hic, 0, "ICOpen IV41 must mint a non-zero HIC");

    // Round-17 priority-3 cross-check: `ICGetInfo` must now produce
    // a non-empty buffer with the fcc-derived szName even though
    // IR41 returns 0 bytes from ICM_GETINFO. We don't assert the
    // exact length (the wrapper synthesises a `cb`-sized buffer),
    // just that the IV41 ASCII surfaces at the szName offset.
    match sb.ic_get_info(hic, 96) {
        Ok(info) => {
            eprintln!(
                "round17B: ICGetInfo returned {} bytes (priority-3 short-return fallback)",
                info.len()
            );
            if info.len() >= 24 + 8 {
                assert_eq!(
                    info[24], b'I',
                    "round-17 priority 3: szName[0] must surface as 'I' (IV41)"
                );
                assert_eq!(info[26], b'V', "round-17 priority 3: szName[1] = 'V'");
                assert_eq!(info[28], b'4', "round-17 priority 3: szName[2] = '4'");
                assert_eq!(info[30], b'1', "round-17 priority 3: szName[3] = '1'");
            }
        }
        Err(e) => panic!("round-17B: ICGetInfo trapped: {e}"),
    }

    let bih_in = Bih {
        bi_size: BIH_SIZE,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: *b"IV41",
        size_image: s0.bytes.len() as u32,
        ..Default::default()
    };
    let bih_out = Bih {
        bi_size: BIH_SIZE,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: [0; 4], // BI_RGB
        size_image: width * height * 3,
        ..Default::default()
    };

    let q = sb
        .ic_decompress_query(hic, &bih_in, Some(&bih_out))
        .expect("ICDecompressQuery should not trap");
    assert_eq!(q, 0, "ICDecompressQuery → ICERR_OK");
    let b = sb
        .ic_decompress_begin(hic, &bih_in, &bih_out)
        .expect("ICDecompressBegin should not trap");
    assert_eq!(b, 0, "ICDecompressBegin → ICERR_OK");

    let dllmain_cpuid_count = sb.cpu.cpuid_dispatch_count;
    eprintln!("round17B: post-Begin CPUID count: {dllmain_cpuid_count}");

    let out_capacity = width * height * 3;

    /// Per-frame outcome record. Same shape as round 16; we
    /// surface the per-frame MMX dispatch counter explicitly.
    #[derive(Debug)]
    #[allow(dead_code)]
    struct FrameOutcome {
        sample_idx: u32,
        sample_size: u32,
        lr: Option<u32>,
        nonzero: usize,
        trap: Option<String>,
        elapsed_instrs: u64,
        elapsed_mmx: u64,
        elapsed_cpuid: u64,
    }
    let mut outcomes: Vec<FrameOutcome> = Vec::new();

    for n in 0..NUM_FRAMES {
        let sample = match common::avi_extractor::extract_video_sample(&avi, n) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("sample {n}: walker error: {e}");
                break;
            }
        };

        let bih_in_n = Bih {
            size_image: sample.bytes.len() as u32,
            ..bih_in.clone()
        };

        let flags = if n == 0 {
            0
        } else {
            oxideav_vfw::win32::vfw32::ICDECOMPRESS_NOTKEYFRAME
        };

        let pre = sb.cpu.instr_count;
        let pre_mmx = sb.cpu.mmx_dispatch_count;
        let pre_cpuid = sb.cpu.cpuid_dispatch_count;
        let result = sb.ic_decompress(hic, flags, &bih_in_n, &sample.bytes, &bih_out, out_capacity);
        let elapsed_instrs = sb.cpu.instr_count.saturating_sub(pre);
        let elapsed_mmx = sb.cpu.mmx_dispatch_count.saturating_sub(pre_mmx);
        let elapsed_cpuid = sb.cpu.cpuid_dispatch_count.saturating_sub(pre_cpuid);

        match result {
            Ok((lr, out)) => {
                let nonzero = out.iter().filter(|&&b| b != 0).count();
                eprintln!(
                    "round17B sample {n}: lr={lr:#010x} ({}), {} bytes input, \
                     {nonzero} non-zero output bytes, {elapsed_instrs} instrs, \
                     {elapsed_mmx} MMX, {elapsed_cpuid} CPUID",
                    lr as i32,
                    sample.bytes.len(),
                );
                outcomes.push(FrameOutcome {
                    sample_idx: n,
                    sample_size: sample.bytes.len() as u32,
                    lr: Some(lr),
                    nonzero,
                    trap: None,
                    elapsed_instrs,
                    elapsed_mmx,
                    elapsed_cpuid,
                });
            }
            Err(e) => {
                let msg = format!("{e}");
                eprintln!("round17B sample {n}: TRAP: {msg} ({elapsed_instrs} instrs)");
                outcomes.push(FrameOutcome {
                    sample_idx: n,
                    sample_size: sample.bytes.len() as u32,
                    lr: None,
                    nonzero: 0,
                    trap: Some(msg),
                    elapsed_instrs,
                    elapsed_mmx,
                    elapsed_cpuid,
                });
                break;
            }
        }
    }

    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);

    let total_mmx: u64 = outcomes.iter().map(|o| o.elapsed_mmx).sum();
    let total_cpuid: u64 = outcomes.iter().map(|o| o.elapsed_cpuid).sum();
    let frames_ok = outcomes
        .iter()
        .filter(|o| o.trap.is_none() && o.lr == Some(0))
        .count();
    eprintln!(
        "round17B summary: indeo41.avi {width}×{height} — {} frames driven \
         ({frames_ok} ICERR_OK), {total_mmx} MMX dispatches total, \
         {total_cpuid} CPUID dispatches total",
        outcomes.len(),
    );
    eprintln!(
        "Round-17 MMX dispatch finding: total_mmx={total_mmx} \
         across 8 IV41 frames at 320×240 — {}",
        if total_mmx == 0 {
            "SPECGAP confirmed (statically integer-only binary, no MMX path to validate)"
        } else {
            "REAL-CODEC MMX VALIDATION (round-13 module exercised in production context)"
        }
    );

    // ---- assertions -----------------------------------------------

    let s0_outcome = outcomes
        .iter()
        .find(|o| o.sample_idx == 0)
        .expect("sample 0 outcome must be recorded");
    assert!(
        s0_outcome.trap.is_none(),
        "round-17B sample 0 trapped: {:?}",
        s0_outcome.trap
    );
    assert_eq!(
        s0_outcome.lr,
        Some(0),
        "round-17B sample 0 expected ICERR_OK; got lr={:?}",
        s0_outcome.lr,
    );
    assert!(
        s0_outcome.nonzero > (out_capacity as usize) / 4,
        "round-17B sample 0 expected > 25% non-zero output ({}/{})",
        s0_outcome.nonzero,
        out_capacity,
    );

    let s1_outcome = outcomes
        .iter()
        .find(|o| o.sample_idx == 1)
        .expect("sample 1 outcome must be recorded");
    assert!(
        s1_outcome.trap.is_none(),
        "round-17B sample 1 (first P-frame) trapped: {:?}",
        s1_outcome.trap
    );
    assert_eq!(
        s1_outcome.lr,
        Some(0),
        "round-17B sample 1 expected ICERR_OK; got lr={:?}",
        s1_outcome.lr,
    );
    assert!(
        s1_outcome.nonzero > (out_capacity as usize) / 4,
        "round-17B sample 1 expected > 25% non-zero output ({}/{})",
        s1_outcome.nonzero,
        out_capacity,
    );

    assert!(
        frames_ok >= 4,
        "round-17B milestone: expected ≥ 4 of {NUM_FRAMES} sequential \
         IV41 frames to return ICERR_OK; got {frames_ok}",
    );
}
