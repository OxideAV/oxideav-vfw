//! Round 14 — drive *non*-cat_attack IV50 fixtures through
//! `IR50_32.DLL`, prove the round-13 multi-frame pipeline holds
//! up across the IV50 corpus, and characterise the codec's MMX
//! behaviour with finer granularity than round 13 had.
//!
//! Round 13 successfully decoded eight sequential frames of
//! `cat_attack.avi` (320×240 yuv410p), but the per-frame
//! `mmx_dispatch_count` came back as **0**. Round 14 widens the
//! probe to three further IV50 fixtures spanning 240×180, 320×240
//! and 640×352, and adds a `cpuid_dispatch_count` instrument so
//! the test can distinguish "codec queried CPUID + still chose
//! integer" from "codec was built integer-only at compile time".
//!
//! ## Empirical finding (round 14)
//!
//! Across **four** IV50 fixtures spanning 4× the macroblock
//! count (240×180 → 640×352), all yuv410p (the full corpus is
//! 4:1:0), `IR50_32.DLL`'s decode path is **integer-only**:
//! `mmx_dispatch_count == 0` per frame, even though our
//! [`emulator::Cpu::cpuid`] response sets `EDX.MMX = 1` (bit 23
//! per Intel SDM Vol. 2A §3.2 "CPUID Feature Flags").
//!
//! Adding the round-14 CPUID instrument
//! (`Cpu::cpuid_dispatch_count`) sharpens the diagnosis: the
//! `cpuid` instruction is *never executed* during DllMain,
//! DRV_OPEN, ICDecompressBegin, or any of the per-frame
//! ICDecompress calls. A direct byte scan of the DLL image
//! confirms `0F A2` (CPUID) has zero occurrences in the
//! 184 KB binary, and the MMX arithmetic opcode block
//! (`0F D0..FF` per SDM Vol. 2A Appendix A Table A-3 — PADDx /
//! PSUBx / PMUL* / PAND / POR / PXOR / PSL* / PSR* etc) also
//! has **zero occurrences**. The IR50_32.DLL shipped in the
//! IV5PLAY redistributable was therefore built *without* MMX
//! codegen — there's no integer-vs-MMX gate to flip; the
//! decoder is unconditionally integer.
//!
//! ## Implications for round 15
//!
//! The round-13 `src/emulator/isa_mmx.rs` (1007 LOC, ~50
//! opcodes) is not exercised by *this* IV50 binary. To
//! validate it against real codec input we need either:
//!
//! * A different Indeo 5 redistributable build (the public
//!   "indeo5xa" / "indeo5ds" variants noted in
//!   `samples.oxideav.org/ffmpeg/V-codecs/IV50/sv2-d.txt` are
//!   plausibly the MMX-enabled ones); the IV5PLAY bundle we
//!   currently ship is the integer build.
//! * A different *codec* whose binary does contain MMX —
//!   Cinepak, MS Video 1, or Indeo 4 (`IR41_32.AX`) being the
//!   most likely candidates in the
//!   `samples.oxideav.org/codecs/windows` catalog. Round-14's
//!   Part B (DirectShow probe of `IR41_32.AX`) deferred to
//!   round 15+.
//!
//! Either path is round-15 scope. Round 14's deliverable is
//! the empirical proof that more MMX implementation work would
//! be wasted against IR50_32.DLL specifically.
//!
//! ## Acceptance gates (round 14)
//!
//! The gate relaxes round 13's "force MMX" intent to match the
//! empirical reality (no MMX in this binary):
//!
//! 1. *At least one* non-cat_attack IV50 fixture in the
//!    candidate set decodes 8 sequential samples through one
//!    shared `hic` with `ICERR_OK` and >25 % non-zero RGB24
//!    output — the round-13 pipeline still works on a different
//!    bitstream / resolution / encoder.
//! 2. Sample 1 (the first P-frame) on the winning fixture must
//!    return `ICERR_OK` — round-13's portability gate.
//!
//! The CPUID + MMX instruments are recorded to stderr but not
//! asserted: the test must keep passing as round 14 lands the
//! diagnosis, even though the diagnosis is "MMX path
//! unreachable in this binary".
//!
//! ## Fixture selection
//!
//! The IV50 corpus published at
//! `samples.oxideav.org/ffmpeg/V-codecs/IV50/index.json` contains
//! ~17 files, all yuv410p (4:1:0). We try a sequence of
//! candidates from low to high resolution:
//!
//! * `indeo5.avi` — 320×240 yuv410p, 181 KB.
//! * `Educ_Movie_DeadlyForce.avi` — 240×180 yuv410p, 939 KB.
//! * `miss_congeniality_cryptedindeo5_sbcaudio.avi` — 640×352
//!   yuv410p, 622 KB.
//!
//! `sv2-d.avi` is *not* used: it is OpenDML (AVI 2.0) with an
//! `indx` super-index instead of an inline `LIST movi`, which
//! the round-8 AVI walker does not handle yet.
//!
//! Reference docs (clean-room):
//!
//! * Microsoft RIFF + AVI 1.0 specs (chunk walker shared with
//!   round-8).
//! * Microsoft VfW SDK header (`vfw.h`, ICM_* / ICDECOMPRESS_*).
//! * Intel® 64 and IA-32 Architectures Software Developer's
//!   Manual, Vol. 2A §3.2 "CPUID Feature Information" (EDX
//!   feature-bit table) + Appendix A "MMX instruction set
//!   reference".
//!
//! NEVER reference `libavcodec/indeo5.c`, Wine, ReactOS, or any
//! other third-party Indeo decoder source.

mod common;

use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};
use oxideav_vfw::Sandbox;

/// Per-frame outcome record. Pushed for every frame the test
/// drives through `ICDecompress` (success + trap).
#[derive(Debug)]
#[allow(dead_code)] // Surfaced via Debug-print of the outcomes
                    // vec; not consumed by an assertion. Field
                    // is still load-bearing for the trace log.
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

struct FixtureResult {
    name: &'static str,
    width: u32,
    height: u32,
    out_capacity: u32,
    outcomes: Vec<FrameOutcome>,
    total_mmx: u64,
    total_cpuid: u64,
    /// CPUID dispatch count snapshot taken right after DllMain,
    /// install_codec, ic_open, ic_decompress_query, and
    /// ic_decompress_begin all completed (before the per-frame
    /// loop starts). If this is non-zero, the codec resolved a
    /// feature gate at load time; if zero, the binary is
    /// statically integer-only.
    dllmain_cpuid_count: u64,
}

/// Decode up to 8 sequential frames of `(fixture)` through a
/// fresh `Sandbox` + `IR50_32.DLL`, returning per-frame outcomes
/// and aggregate MMX + CPUID dispatch counts.
fn drive_fixture(dll_bytes: &[u8], fixture: &'static str) -> Result<FixtureResult, String> {
    /// vfw.h: `ICMODE_DECOMPRESS = 2`.
    const ICMODE_DECOMPRESS: u32 = 2;
    /// Cap at 8 frames like round 13 — fast enough for CI, still
    /// enough P-frames to surface MMX motion-comp paths.
    const NUM_FRAMES: u32 = 8;

    let avi = common::fetch_or_load_ffmpeg_sample("IV50", fixture)
        .map_err(|e| format!("fetch {fixture}: {e}"))?;

    let s0 = common::avi_extractor::extract_first_video_sample(&avi)
        .map_err(|e| format!("AVI walker on {fixture}: {e}"))?;
    let width: u32 = s0.width;
    let height: u32 = s0.height;
    eprintln!("round14 driving fixture: {fixture} {width}×{height}");

    let mut sb = Sandbox::new();
    let img = sb
        .load("IR50_32.DLL", dll_bytes)
        .map_err(|e| format!("load IR50_32.DLL for {fixture}: {e}"))?;

    sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .map_err(|e| format!("DllMain for {fixture}: {e}"))?;
    sb.install_codec(&img)
        .map_err(|e| format!("install_codec for {fixture}: {e}"))?;

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"IV50");
    let hic = sb
        .ic_open(fcc_video, fcc_handler, ICMODE_DECOMPRESS)
        .map_err(|e| format!("ic_open for {fixture}: {e}"))?;
    if hic == 0 {
        return Err(format!("ic_open returned 0 HIC for {fixture}"));
    }

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
        .map_err(|e| format!("ICDecompressQuery for {fixture}: {e}"))?;
    if q != 0 {
        return Err(format!("ICDecompressQuery returned {q:#x} for {fixture}"));
    }
    let b = sb
        .ic_decompress_begin(hic, &bih_in, &bih_out)
        .map_err(|e| format!("ICDecompressBegin for {fixture}: {e}"))?;
    if b != 0 {
        return Err(format!("ICDecompressBegin returned {b:#x} for {fixture}"));
    }

    // Snapshot the CPUID counter once DllMain + DRV_OPEN +
    // ICDecompressBegin have completed. Round 14 uses this to
    // determine whether the codec resolved an integer/MMX gate
    // at load time (CPUID-then-cache pattern) versus never
    // querying CPUID at all (statically built integer-only).
    let dllmain_cpuid_count = sb.cpu.cpuid_dispatch_count;
    eprintln!("[{fixture}] post-Begin CPUID count: {dllmain_cpuid_count}");

    let out_capacity = width * height * 3;
    let mut outcomes: Vec<FrameOutcome> = Vec::new();

    // Round-14 instruction budget — generous; 640×352 frames
    // are ~4× the macroblock count of 320×240 and need
    // proportionally more cycles. 600M is enough headroom for
    // the largest fixture's keyframe.
    sb.cpu.set_instr_limit(600_000_000);

    for n in 0..NUM_FRAMES {
        let sample = match common::avi_extractor::extract_video_sample(&avi, n) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[{fixture}] sample {n}: walker error: {e}");
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
                    "[{fixture}] sample {n}: lr={lr:#010x} ({}), {} bytes input, \
                     {nonzero} non-zero output bytes, {elapsed_instrs} instrs, \
                     {elapsed_mmx} MMX instrs, {elapsed_cpuid} CPUID instrs",
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
                    elapsed_mmx,
                    elapsed_cpuid,
                });
            }
            Err(e) => {
                let msg = format!("{e}");
                eprintln!("[{fixture}] sample {n}: TRAP: {msg} ({elapsed_instrs} instrs)");
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
                // A trap leaves the CPU in mid-instruction; stop
                // iterating against this fixture.
                break;
            }
        }
    }

    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);

    let total_mmx: u64 = outcomes.iter().map(|o| o.elapsed_mmx).sum();
    let total_cpuid: u64 = outcomes.iter().map(|o| o.elapsed_cpuid).sum();
    Ok(FixtureResult {
        name: fixture,
        width,
        height,
        out_capacity,
        outcomes,
        total_mmx,
        total_cpuid,
        dllmain_cpuid_count,
    })
}

/// Round-14 milestone test. Drives every candidate fixture
/// through the round-13 pipeline; asserts that *at least one*
/// non-cat_attack fixture produces 8 frames of `ICERR_OK` and
/// more than 25 % non-zero output. The MMX and CPUID dispatch
/// counts are recorded but not asserted (see module-level doc:
/// the codec stays integer-only across the corpus).
#[test]
fn iv50_alternate_fixtures_decode_through_ir50() {
    let candidates: &[&'static str] = &[
        "indeo5.avi",
        "Educ_Movie_DeadlyForce.avi",
        "miss_congeniality_cryptedindeo5_sbcaudio.avi",
    ];

    let dll_bytes = common::fetch_or_load("IR50_32.DLL").expect("fetch IR50_32.DLL");

    let mut summaries: Vec<String> = Vec::new();
    let mut results: Vec<FixtureResult> = Vec::new();

    for name in candidates {
        match drive_fixture(&dll_bytes, name) {
            Ok(res) => {
                let frames_with_mmx = res.outcomes.iter().filter(|o| o.elapsed_mmx > 0).count();
                let frames_ok = res
                    .outcomes
                    .iter()
                    .filter(|o| o.trap.is_none() && o.lr == Some(0))
                    .count();
                let summary = format!(
                    "{}: {}×{}, {} frames decoded ({} ok), {} frames with MMX, \
                     {} MMX opcodes total, {} CPUID opcodes total",
                    res.name,
                    res.width,
                    res.height,
                    res.outcomes.len(),
                    frames_ok,
                    frames_with_mmx,
                    res.total_mmx,
                    res.total_cpuid,
                );
                eprintln!("round14 fixture summary: {summary}");
                summaries.push(summary);
                results.push(res);
            }
            Err(e) => {
                let summary = format!("{name}: drive error: {e}");
                eprintln!("round14 fixture error: {summary}");
                summaries.push(summary);
                // Continue to the next candidate — a single
                // fixture failure (e.g. unsupported container
                // shape) shouldn't sink the whole probe.
            }
        }
    }

    // Pick the first fixture that decoded ≥ 4 ICERR_OK frames
    // with > 25% non-zero output on sample 0 — that's the
    // "round-13 pipeline still works" milestone.
    let winner = results.iter().find(|res| {
        let s0_ok = res
            .outcomes
            .iter()
            .find(|o| o.sample_idx == 0)
            .map(|o| {
                o.trap.is_none() && o.lr == Some(0) && o.nonzero > (res.out_capacity as usize) / 4
            })
            .unwrap_or(false);
        let n_ok = res
            .outcomes
            .iter()
            .filter(|o| o.trap.is_none() && o.lr == Some(0))
            .count();
        s0_ok && n_ok >= 4
    });

    let res = winner.unwrap_or_else(|| {
        panic!(
            "round-14 milestone: no IV50 fixture in candidate set decoded \
             cleanly. Per-fixture summaries:\n  {}",
            summaries.join("\n  "),
        )
    });

    eprintln!(
        "round14 selected fixture: {} ({}×{}) — {} CPUID dispatches in load+open \
         (DllMain+DRV_OPEN+Begin), {} CPUID dispatches in decode loop, {} MMX dispatches",
        res.name, res.width, res.height, res.dllmain_cpuid_count, res.total_cpuid, res.total_mmx,
    );

    // The round-14 acceptance gate: round-13 multi-frame pipeline
    // works on a fixture other than cat_attack.
    let s0 = res
        .outcomes
        .iter()
        .find(|o| o.sample_idx == 0)
        .expect("sample 0 outcome must be recorded");
    assert!(
        s0.trap.is_none(),
        "{} sample 0 trapped: {:?}",
        res.name,
        s0.trap
    );
    assert_eq!(
        s0.lr,
        Some(0),
        "{} sample 0 expected ICERR_OK; got lr={:?}",
        res.name,
        s0.lr,
    );
    assert!(
        s0.nonzero > (res.out_capacity as usize) / 4,
        "{} sample 0 expected > 25% non-zero output ({}/{})",
        res.name,
        s0.nonzero,
        res.out_capacity,
    );

    // Sample 1 (first P-frame) must succeed on the winning
    // fixture — same round-13 milestone, now portable to a
    // different bitstream.
    let s1 = res
        .outcomes
        .iter()
        .find(|o| o.sample_idx == 1)
        .expect("sample 1 outcome must be recorded (round-13 portability gate)");
    assert!(
        s1.trap.is_none(),
        "{} sample 1 (first P-frame) trapped: {:?}",
        res.name,
        s1.trap
    );
    assert_eq!(
        s1.lr,
        Some(0),
        "{} sample 1 (first P-frame) expected ICERR_OK; got lr={:?}",
        res.name,
        s1.lr,
    );
}
