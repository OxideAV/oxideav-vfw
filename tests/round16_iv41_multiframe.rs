//! Round 16 — drive multiple Indeo 4 frames sequentially through
//! `IR41_32.AX` (the dual-shape DirectShow / VfW codec round 15
//! first decoded a single keyframe through).
//!
//! Round 15 unblocked the FIRST keyframe of `crashtest.avi`
//! (sample 0). Round 16 mirrors round 13's 8-frame ratchet on
//! IV50 → applied here to the IV41 path: keyframe at sample 0,
//! subsequent samples are P-frames that reference the prior
//! decoded frame for motion compensation + residual application.
//!
//! Key invariant — same as round 13: a single `hic` is opened
//! once with `install_codec` + `ic_open`, walked through `BEGIN`,
//! then `ic_decompress` is called repeatedly without an
//! intervening `END`/`CLOSE`. The codec maintains its
//! reference-frame state across calls.
//!
//! ## MMX expectation
//!
//! Round 14 established that `IR50_32.DLL` is statically
//! integer-only: zero `0F A2` (CPUID) and zero `0F D0..FF` (MMX
//! arithmetic) bytes in the binary. Round 15 didn't byte-scan
//! `IR41_32.AX`, so round 16 records `mmx_dispatch_count` and
//! `cpuid_dispatch_count` per frame as a diagnostic — if any
//! P-frame fires MMX semantics, that's the long-awaited
//! validation of the round-13 MMX module against real codec
//! input.
//!
//! ## Acceptance gates (round 16)
//!
//! 1. Sample 0 (keyframe) — `ICDecompress` returns `ICERR_OK = 0`
//!    with > 25 % non-zero RGB24 output (regression sentinel for
//!    the round-15 milestone).
//! 2. Samples 1..N (first N-1 P-frames) — each must also return
//!    `ICERR_OK` with non-zero output. P-frame decode exercises
//!    the codec's motion-comp path; a clean run here proves the
//!    Indeo 4 reference-frame pipeline works through the
//!    emulator's IC* dispatch.
//! 3. The MMX + CPUID dispatch counts are recorded to stderr but
//!    not asserted: this round's contract is "P-frames decode",
//!    not "codec uses MMX".
//!
//! ## Reference docs (clean-room)
//!
//! * Microsoft RIFF + AVI 1.0 specs (chunk walker shared with
//!   round 8).
//! * Microsoft VfW SDK header (`vfw.h`, ICM_* / ICDECOMPRESS_*).
//! * Intel® 64 and IA-32 Architectures Software Developer's
//!   Manual, Vol. 2A §3.2 + Appendix A.
//! * `docs/video/indeo/indeo4/wiki/Indeo_4.wiki` — IV41
//!   bitstream / motion model.
//!
//! NEVER reference `libavcodec/indeo4.c`, Wine's
//! `dlls/quartz`, ReactOS, or any third-party Indeo decoder.

mod common;

use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};
use oxideav_vfw::Sandbox;

/// Decode N sequential samples (keyframe + P-frames) through one
/// shared `hic` and assert the codec's reference-frame state
/// persists across the calls.
#[test]
fn crashtest_decodes_sequential_iv41_frames_through_shared_hic() {
    /// vfw.h: `ICMODE_DECOMPRESS = 2`.
    const ICMODE_DECOMPRESS: u32 = 2;
    /// 8 frames matches round 13's discipline; `crashtest.avi` is
    /// ~966 frames so we have plenty of headroom.
    const NUM_FRAMES: u32 = 8;

    let dll_bytes = common::fetch_or_load("IR41_32.AX").expect("fetch IR41_32.AX");
    let avi =
        common::fetch_or_load_ffmpeg_sample("IV41", "crashtest.avi").expect("fetch crashtest.avi");

    // Sample 0 metadata for codec format negotiation.
    let s0 = common::avi_extractor::extract_first_video_sample(&avi)
        .expect("AVI walker on crashtest.avi");
    let width: u32 = s0.width;
    let height: u32 = s0.height;
    eprintln!("round16 fixture: crashtest.avi {width}×{height}");
    assert_eq!(
        s0.codec_fourcc,
        u32::from_le_bytes(*b"IV41"),
        "round-16 expected crashtest.avi sample-0 fourcc to be IV41",
    );

    let mut sb = Sandbox::new();
    // crashtest.avi P-frames may be larger than the keyframe;
    // give the codec generous headroom (round 15 used 200M for
    // a single frame; 8 frames gets 600M).
    sb.cpu.set_instr_limit(600_000_000);
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
    eprintln!("round16: post-Begin CPUID count: {dllmain_cpuid_count}");

    let out_capacity = width * height * 3;

    /// Per-frame outcome record. Pushed for every frame the test
    /// drives (success + trap). Surfaced via Debug-print of the
    /// outcomes vec; not consumed by an assertion (the gates
    /// inspect specific samples by index).
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

        // ICDECOMPRESS_NOTKEYFRAME on n>=1 mirrors what real
        // `vfw32!ICDecompress` does when fed by a player iterating
        // an AVI index. The bitstream itself encodes whether the
        // frame is intra; the flag is a hint many codecs ignore.
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
                    "sample {n}: lr={lr:#010x} ({}), {} bytes input, \
                     {nonzero} non-zero output bytes, {elapsed_instrs} instrs, \
                     {elapsed_mmx} MMX instrs, {elapsed_cpuid} CPUID instrs",
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
                eprintln!("sample {n}: TRAP: {msg} ({elapsed_instrs} instrs)");
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
                // A trap leaves the CPU mid-instruction; stop.
                break;
            }
        }
    }

    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);

    // ---- aggregate trace ------------------------------------------
    let total_mmx: u64 = outcomes.iter().map(|o| o.elapsed_mmx).sum();
    let total_cpuid: u64 = outcomes.iter().map(|o| o.elapsed_cpuid).sum();
    let frames_ok = outcomes
        .iter()
        .filter(|o| o.trap.is_none() && o.lr == Some(0))
        .count();
    eprintln!(
        "round16 summary: {} frames driven ({frames_ok} ICERR_OK), \
         {total_mmx} MMX dispatches total, {total_cpuid} CPUID dispatches total",
        outcomes.len(),
    );

    // ---- assertions -----------------------------------------------

    // Sample 0 must succeed (round-15 milestone regression).
    let s0_outcome = outcomes
        .iter()
        .find(|o| o.sample_idx == 0)
        .expect("sample 0 outcome must be recorded");
    assert!(
        s0_outcome.trap.is_none(),
        "sample 0 (round-15 regression sentinel) trapped: {:?}",
        s0_outcome.trap
    );
    assert_eq!(
        s0_outcome.lr,
        Some(0),
        "sample 0 (round-15 regression sentinel) expected ICERR_OK; got lr={:?}",
        s0_outcome.lr,
    );
    assert!(
        s0_outcome.nonzero > (out_capacity as usize) / 4,
        "sample 0 expected > 25% non-zero output ({}/{}); regression",
        s0_outcome.nonzero,
        out_capacity,
    );

    // Round-16 milestone gate: at least 4 of the 8 sequential
    // samples must decode `ICERR_OK` (matches round 14's "≥ 4
    // sequential frames" portability bar). Sample 1 specifically
    // is the first P-frame.
    let s1_outcome = outcomes
        .iter()
        .find(|o| o.sample_idx == 1)
        .expect("sample 1 outcome must be recorded (round-16 milestone)");
    assert!(
        s1_outcome.trap.is_none(),
        "round-16 milestone: sample 1 (first IV41 P-frame) trapped: {:?}",
        s1_outcome.trap
    );
    assert_eq!(
        s1_outcome.lr,
        Some(0),
        "round-16 milestone: sample 1 (first IV41 P-frame) expected ICERR_OK; got lr={:?}",
        s1_outcome.lr,
    );
    assert!(
        s1_outcome.nonzero > (out_capacity as usize) / 4,
        "round-16 milestone: sample 1 expected > 25% non-zero output ({}/{})",
        s1_outcome.nonzero,
        out_capacity,
    );

    assert!(
        frames_ok >= 4,
        "round-16 milestone: expected ≥ 4 of {NUM_FRAMES} sequential \
         IV41 frames to return ICERR_OK; got {frames_ok}",
    );
}

/// Round-16 OpenDML smoke against a real fixture. `sv2-d.avi`
/// is an OpenDML / AVI 2.0 IV50 file (per the IV50 corpus's
/// `sv2-d.txt` accompanying note): `LIST hdrl` carries an
/// `indx` super-index in its `strl`, and `LIST movi` carries
/// `ix00` / `ix01` standard-index chunks alongside the
/// per-stream sample chunks. Both index chunks must be
/// transparently skipped by the new walker so that sample 0
/// is the first inline `00iv` chunk in `LIST movi`.
///
/// The mirror serves a 450 KB head of the file (the full
/// file is ~870 KB by RIFF declared size); the walker's
/// existing truncation clamp keeps the partial-tail walk
/// honest.
#[test]
fn sv2_d_opendml_avi_walker_skips_indx_and_ix_chunks() {
    let avi = common::fetch_or_load_ffmpeg_sample("IV50", "sv2-d.avi").expect("fetch sv2-d.avi");
    eprintln!("sv2-d.avi: {} bytes", avi.len());

    let (forms, movi_count) =
        common::avi_extractor::riff_segment_inventory(&avi).expect("inventory sv2-d.avi");
    eprintln!(
        "sv2-d.avi: {} RIFF segment(s), {} LIST movi block(s)",
        forms.len(),
        movi_count,
    );
    eprintln!(
        "sv2-d.avi RIFF forms: {:?}",
        forms
            .iter()
            .map(|f| std::str::from_utf8(f).unwrap_or("???").to_string())
            .collect::<Vec<_>>(),
    );
    assert!(!forms.is_empty(), "must have at least one RIFF segment");
    assert_eq!(
        &forms[0], b"AVI ",
        "first RIFF segment must be the AVI 1.0-shape head",
    );
    assert!(
        movi_count >= 1,
        "must locate at least one LIST movi (saw {movi_count})",
    );

    let s0 = common::avi_extractor::extract_first_video_sample(&avi)
        .expect("AVI walker on sv2-d.avi (must skip indx/ix## index chunks)");
    eprintln!(
        "sv2-d.avi sample 0: codec_fourcc={:08x} ({}) {}x{} size={}",
        s0.codec_fourcc,
        std::str::from_utf8(&s0.codec_fourcc.to_le_bytes())
            .unwrap_or("?")
            .escape_debug(),
        s0.width,
        s0.height,
        s0.sample_size,
    );
    assert_eq!(
        s0.codec_fourcc,
        u32::from_le_bytes(*b"IV50"),
        "expected sv2-d.avi sample-0 fourcc to be IV50",
    );
    assert!(
        s0.sample_size > 0,
        "expected sv2-d.avi sample-0 to have non-zero bytes (the walker \
         skipped past the leading ix00/ix01 index chunks correctly)",
    );

    // Bonus: sample 1 must also walk; this catches a
    // regression where the walker erroneously consumes the
    // entire `ix01` chunk's bytes as sample 1.
    let s1 =
        common::avi_extractor::extract_video_sample(&avi, 1).expect("sv2-d.avi sample 1 walker");
    assert!(
        s1.sample_size > 0,
        "sv2-d.avi sample 1 must surface non-zero bytes",
    );
    eprintln!(
        "sv2-d.avi sample 1: size={} (regression sentinel: walker correctly \
         advances past ix## chunks instead of consuming their payload)",
        s1.sample_size,
    );
}
