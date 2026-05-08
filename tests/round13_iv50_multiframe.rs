//! Round 13 — drive multiple Indeo 5 frames sequentially through
//! `IR50_32.DLL`.
//!
//! Round 12 unblocked the FIRST keyframe of `cat_attack.avi`
//! (sample 0). Round 13 extends to sample 1.. (P-frames that
//! reference the prior decoded frame for motion compensation +
//! residual application).
//!
//! Key invariant: a single `hic` is opened once with
//! `install_codec` + `ic_open`, walked through `BEGIN`, then
//! `ic_decompress` is called repeatedly without an intervening
//! `END/CLOSE`. The codec maintains its reference-frame state
//! across calls; opening a fresh hic between frames would
//! discard the keyframe and the next P-frame would have nothing
//! to motion-compensate against.
//!
//! Round-13 acceptance:
//!
//! * Sample 0 (keyframe) — `ICDecompress` returns `ICERR_OK = 0`
//!   with > 25 % non-zero RGB24 output (regression sentinel for
//!   the round-12 milestone).
//! * Sample 1 (first P-frame) — must also return `ICERR_OK` with
//!   non-zero output. P-frame decode exercises MMX motion
//!   compensation in `IR50_32.DLL`; the round-7 scaffold trapped
//!   on every MMX byte, round-13 implements the MMX subset the
//!   IV50 P-frame body uses.
//!
//! Reference docs (clean-room):
//!
//! * Microsoft RIFF + AVI 1.0 specs (chunk walker shared with
//!   round-8).
//! * Microsoft VfW SDK header (`vfw.h`, ICM_* / ICDECOMPRESS_*).
//! * Intel® 64 and IA-32 Architectures Software Developer's
//!   Manual, Volume 2A/2B, MMX instruction set reference.
//!
//! NEVER reference `libavcodec/indeo5.c`, Wine, ReactOS, or any
//! other third-party Indeo decoder source.

mod common;

use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};
use oxideav_vfw::Sandbox;

/// Decode three sequential samples (keyframe + two P-frames)
/// through one shared `hic`. The codec's reference-frame state
/// must persist across the calls.
#[test]
fn cat_attack_decodes_sequential_frames_through_shared_hic() {
    /// vfw.h: `ICMODE_DECOMPRESS = 2`.
    const ICMODE_DECOMPRESS: u32 = 2;
    /// We try to decode this many samples in sequence (sample 0
    /// keyframe + 1..N P-frames). cat_attack.avi has 174 frames
    /// per the AVI walker; we cap at 8 so the test stays fast
    /// enough for CI while still exercising the full P-frame
    /// pipeline (≥ 7 P-frames is plenty to surface any
    /// MMX-opcode gap).
    const NUM_FRAMES: u32 = 8;

    let dll_bytes = common::fetch_or_load("IR50_32.DLL").expect("fetch IR50_32.DLL");
    let avi = common::fetch_or_load_ffmpeg_sample("IV50", "cat_attack.avi")
        .expect("fetch cat_attack.avi");

    // Sample 0 metadata for codec format negotiation.
    let s0 = common::avi_extractor::extract_first_video_sample(&avi)
        .expect("AVI walker on cat_attack.avi");
    let width: u32 = s0.width;
    let height: u32 = s0.height;

    let mut sb = Sandbox::new();
    let img = sb.load("IR50_32.DLL", &dll_bytes).expect("load");

    let _ = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .expect("DllMain");
    sb.install_codec(&img).expect("install_codec");

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"IV50");
    let hic = sb
        .ic_open(fcc_video, fcc_handler, ICMODE_DECOMPRESS)
        .expect("ic_open");
    assert_ne!(hic, 0, "ICOpen must mint a non-zero HIC");

    let bih_in = Bih {
        bi_size: BIH_SIZE,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: *b"IV50",
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

    let out_capacity = width * height * 3;

    // Per-frame outcome record. We push every frame (success or
    // trap) and emit a summary line so the test report reads as a
    // self-describing trace.
    #[derive(Debug)]
    #[allow(dead_code)] // sample_size + elapsed_instrs are only
                        // surfaced through Debug-print of the
                        // outcomes vec; they're not consumed by an
                        // assertion. The Debug field is still
                        // load-bearing for the trace log a failed
                        // run prints to stderr.
    struct FrameOutcome {
        sample_idx: u32,
        sample_size: u32,
        lr: Option<u32>,
        nonzero: usize,
        trap: Option<String>,
        elapsed_instrs: u64,
    }
    let mut outcomes: Vec<FrameOutcome> = Vec::new();

    // Round-13 instruction budget — keyframe takes ~3M, each P
    // frame is comparable. 200M is plenty of slack.
    sb.cpu.set_instr_limit(200_000_000);

    for n in 0..NUM_FRAMES {
        let sample = match common::avi_extractor::extract_video_sample(&avi, n) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("sample {n}: walker error: {e}");
                break;
            }
        };

        // Reflect the per-frame size in the BITMAPINFOHEADER. The
        // header itself is otherwise constant across the stream.
        let bih_in_n = Bih {
            size_image: sample.bytes.len() as u32,
            ..bih_in.clone()
        };

        // Sample 0 is the keyframe; everything later is a P-frame.
        // ICDECOMPRESS_NOTKEYFRAME hints the codec that the input
        // should be motion-compensated against the prior decoded
        // frame rather than processed as a fresh sync frame. (The
        // bitstream itself encodes whether the frame is intra; the
        // flag is a hint and many codecs ignore it. Pass it on
        // sample >= 1 because that's what real `vfw32!ICDecompress`
        // does when fed by a player iterating an AVI index.)
        let flags = if n == 0 {
            0
        } else {
            oxideav_vfw::win32::vfw32::ICDECOMPRESS_NOTKEYFRAME
        };

        let pre = sb.cpu.instr_count;
        let pre_mmx = sb.cpu.mmx_dispatch_count;
        let result = sb.ic_decompress(hic, flags, &bih_in_n, &sample.bytes, &bih_out, out_capacity);
        let elapsed_instrs = sb.cpu.instr_count.saturating_sub(pre);
        let elapsed_mmx = sb.cpu.mmx_dispatch_count.saturating_sub(pre_mmx);

        match result {
            Ok((lr, out)) => {
                let nonzero = out.iter().filter(|&&b| b != 0).count();
                eprintln!(
                    "sample {n}: lr={lr:#010x} ({}), {} bytes input, \
                     {nonzero} non-zero output bytes, {elapsed_instrs} instrs, \
                     {elapsed_mmx} MMX instrs",
                    lr as i32,
                    sample.bytes.len()
                );
                outcomes.push(FrameOutcome {
                    sample_idx: n,
                    sample_size: sample.bytes.len() as u32,
                    lr: Some(lr),
                    nonzero,
                    trap: None,
                    elapsed_instrs,
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
                });
                // A trap leaves the CPU/MMU in an indeterminate
                // state mid-instruction. Stop iterating; the
                // already-recorded outcomes are the round's
                // useful signal.
                break;
            }
        }
    }

    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);

    // ---- assertions ------------------------------------------------

    // Sample 0 must succeed (round-12 milestone regression).
    let s0_outcome = outcomes
        .iter()
        .find(|o| o.sample_idx == 0)
        .expect("sample 0 outcome must be recorded");
    assert!(
        s0_outcome.trap.is_none(),
        "sample 0 (round-12 regression sentinel) trapped: {:?}",
        s0_outcome.trap
    );
    assert_eq!(
        s0_outcome.lr,
        Some(0),
        "sample 0 (round-12 regression sentinel) expected ICERR_OK; got lr={:?}",
        s0_outcome.lr
    );
    assert!(
        s0_outcome.nonzero > (out_capacity as usize) / 4,
        "sample 0 expected > 25% non-zero output ({}/{}); regression",
        s0_outcome.nonzero,
        out_capacity
    );

    // Sample 1 must succeed — the round-13 milestone gate.
    let s1_outcome = outcomes
        .iter()
        .find(|o| o.sample_idx == 1)
        .expect("sample 1 outcome must be recorded (round-13 milestone)");
    assert!(
        s1_outcome.trap.is_none(),
        "round-13 milestone: sample 1 (first P-frame) trapped: {:?}",
        s1_outcome.trap
    );
    assert_eq!(
        s1_outcome.lr,
        Some(0),
        "round-13 milestone: sample 1 (first P-frame) expected ICERR_OK; got lr={:?}",
        s1_outcome.lr
    );
    assert!(
        s1_outcome.nonzero > (out_capacity as usize) / 4,
        "round-13 milestone: sample 1 expected > 25% non-zero output ({}/{})",
        s1_outcome.nonzero,
        out_capacity
    );
}
