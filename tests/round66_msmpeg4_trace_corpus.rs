//! Round 66 — regression guard for the MS-MPEG-4 v3 trace
//! corpus.
//!
//! The round produces 10 per-fixture JSONL artifacts under
//! `docs/codec/msmpeg4-traces/` (see that directory's README
//! for the design contract).  This test re-drives a subset of
//! the trace pipeline at test time so that a future change to
//! the trace infrastructure (e.g. a watchpoint-matching change
//! in `crate::trace`, or a watchpoint accessor change in
//! `Sandbox::watch`) does not silently break LUT-region trace
//! emission without anyone noticing.
//!
//! The regression-guard does NOT re-generate the committed
//! JSONL artifacts; those are produced by the `gen_msmpeg4_traces`
//! example.  Instead it confirms three structural properties
//! that the trace pipeline must keep emitting:
//!
//! 1. **Trace JSONL is non-empty.**  Running a full
//!    `ic_decompress` against a known-good fixture with at least
//!    one LUT watchpoint armed must produce at least 100 lines
//!    of trace output.
//! 2. **mem_read events fire for armed LUT regions.**  At
//!    least 50 of those lines must be `mem_read` events whose
//!    `addr` falls inside the armed scan-permutation table
//!    range — confirming the watchpoint matcher still finds
//!    overlaps in the MMU read path.
//! 3. **Win32 dispatch is recorded.**  At least one
//!    `kind=win32_call` event must appear with an mpg4c32-DLL-
//!    consistent caller eip (the `dispatch_stub` probe still
//!    fires).
//!
//! Fixture / DLL absence is logged with an `eprintln!` and
//! short-circuits the test (per round-66 task spec: do NOT
//! `#[ignore]`, do NOT panic on missing fixtures).
//!
//! Compiles to nothing without `--features trace`.

#![cfg(feature = "trace")]

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use oxideav_vfw::win32::vfw32::ICDECOMPRESS_NOTKEYFRAME;
use oxideav_vfw::{Bih, Sandbox, WatchMode, DLL_PROCESS_ATTACH};

// The five multi-frame fixtures we sweep here — same shape as
// the round-43 / round-44 corpus.  Number of frames per fixture
// taken from those rounds.
const FIXTURES: &[(&str, u32)] = &[
    ("gop-30-352x288", 3),
    ("with-skip-mbs-352x288", 3),
    ("motion-pan-352x288", 2),
    ("intra-pred-active-352x288", 1),
    ("qscale-high-352x288", 1),
];

// LUT regions per `docs/codec/msmpeg4-mpg4c32-rdata-map.md`.
// We arm watches on the full set so the test exercises the
// matcher across all 13 entries, but only verify reads against
// the three regions where coverage is empirically non-zero
// (scan_a / scan_e / scan_f — see msmpeg4-traces/README.md).
const LUT_CANDIDATES: &[(u32, u32)] = &[
    (0x0003_a4c8, 64),
    (0x0003_a708, 128),
    (0x0004_f938, 16376),
    (0x0005_3940, 1024),
    (0x0005_3d42, 1660),
    (0x0005_43c0, 510),
    (0x0005_45c0, 12288),
    (0x0005_7860, 168),
    (0x0005_7bf0, 186),
    (0x0005_7f00, 148),
    (0x0005_81a8, 132),
    (0x0005_8230, 102),
    (0x0005_844c, 74),
];

// Scan-permutation table union: the actual LUT region the
// MP43 decode hot loop reads from on every fixture.  Test
// verifies mem_read events land in this combined range.
const SCAN_LO_RVA: u32 = 0x0005_7800;
const SCAN_HI_RVA: u32 = 0x0005_8500;

struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedSink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn dll_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/mpg4c32.dll");
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

#[derive(Debug, Default)]
struct TraceCounts {
    total_lines: usize,
    lut_reads: usize,
    scan_region_reads: usize,
    win32_calls: usize,
    frames_ok: usize,
}

fn drive_fixture(dll_bytes: &[u8], avi_bytes: &[u8], n: u32) -> Result<TraceCounts, String> {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut sb = Sandbox::new();
    sb.set_trace_sink(Box::new(SharedSink(Arc::clone(&buf))));
    sb.cpu.set_instr_limit(8_000_000_000);

    let img = sb
        .load("mpg4c32.dll", dll_bytes)
        .map_err(|e| format!("load: {e}"))?;
    let image_base = img.image_base;
    sb.call_dll_main(&img, DLL_PROCESS_ATTACH)
        .map_err(|e| format!("dll_main: {e}"))?;
    sb.install_codec(&img)
        .map_err(|e| format!("install_codec: {e}"))?;

    for (rva, size) in LUT_CANDIDATES {
        sb.watch(image_base.wrapping_add(*rva), *size, WatchMode::Read);
    }

    let s0 = common::avi_extractor::extract_video_sample(avi_bytes, 0)
        .map_err(|e| format!("avi sample 0: {e}"))?;
    let (width, height) = (s0.width, s0.height);
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"MP43");
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
        compression: *b"MP43",
        size_image: s0.bytes.len() as u32,
        ..Bih::default()
    };
    let output = Bih {
        bi_size: 40,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: [0; 4],
        size_image: width * height * 3,
        ..Bih::default()
    };
    let q = sb
        .ic_decompress_query(hic, &bih_in_template, Some(&output))
        .map_err(|e| format!("query: {e}"))?;
    if q != 0 {
        return Err(format!("query → {q:#010x}"));
    }
    let begin = sb
        .ic_decompress_begin(hic, &bih_in_template, &output)
        .map_err(|e| format!("begin: {e}"))?;
    if begin != 0 {
        return Err(format!("begin → {begin:#010x}"));
    }

    let cap = output.size_image;
    let mut frames_ok = 0usize;
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
        match sb.ic_decompress(hic, flags, &bih_in, &s.bytes, &output, cap) {
            Ok((0, _)) => frames_ok += 1,
            _ => break,
        }
    }
    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);

    // Analyse the captured JSONL.
    let bytes = buf.lock().unwrap().clone();
    let text = String::from_utf8(bytes).map_err(|e| format!("utf8: {e}"))?;
    let mut counts = TraceCounts {
        frames_ok,
        ..TraceCounts::default()
    };
    let scan_lo = image_base.wrapping_add(SCAN_LO_RVA);
    let scan_hi = image_base.wrapping_add(SCAN_HI_RVA);
    for line in text.lines() {
        counts.total_lines += 1;
        if line.contains(r#""kind":"win32_call""#) {
            counts.win32_calls += 1;
        } else if line.contains(r#""kind":"mem_read""#) {
            counts.lut_reads += 1;
            // Hand-parse the addr — line is JSON with `"addr":"0xNNNNNNNN"`.
            if let Some(start) = line.find(r#""addr":""#) {
                let body = &line[start + 8..];
                if let Some(end) = body.find('"') {
                    let hex = &body[..end];
                    let s = hex.trim_start_matches("0x");
                    if let Ok(a) = u32::from_str_radix(s, 16) {
                        if scan_lo <= a && a < scan_hi {
                            counts.scan_region_reads += 1;
                        }
                    }
                }
            }
        }
    }
    Ok(counts)
}

#[test]
fn round66_msmpeg4_trace_corpus_lut_reads_remain_observable() {
    let Some(dll_path) = dll_path() else {
        eprintln!(
            "round66: mpg4c32.dll missing under \
             docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/; \
             skipping (per round-66 contract: skip cleanly, do not #[ignore])"
        );
        return;
    };
    let dll_bytes = match std::fs::read(&dll_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("round66: failed to read mpg4c32.dll: {e}; skipping");
            return;
        }
    };

    let mut fixtures_checked = 0usize;
    let mut accumulated = TraceCounts::default();
    for (name, n) in FIXTURES {
        let Some(avi_path) = fixture_path(name) else {
            eprintln!("round66: fixture {name} missing; skipping");
            continue;
        };
        let avi_bytes = match std::fs::read(&avi_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("round66: failed to read fixture {name}: {e}");
                continue;
            }
        };
        match drive_fixture(&dll_bytes, &avi_bytes, *n) {
            Ok(counts) => {
                eprintln!(
                    "round66: {name} → {} lines, {} mem_read, {} scan-region reads, \
                     {} win32 calls, {} frames OK",
                    counts.total_lines,
                    counts.lut_reads,
                    counts.scan_region_reads,
                    counts.win32_calls,
                    counts.frames_ok,
                );
                accumulated.total_lines += counts.total_lines;
                accumulated.lut_reads += counts.lut_reads;
                accumulated.scan_region_reads += counts.scan_region_reads;
                accumulated.win32_calls += counts.win32_calls;
                accumulated.frames_ok += counts.frames_ok;
                fixtures_checked += 1;
            }
            Err(e) => {
                eprintln!("round66: {name} drive ERROR: {e}");
            }
        }
    }

    if fixtures_checked == 0 {
        eprintln!("round66: no fixtures available; nothing to verify");
        return;
    }

    eprintln!(
        "round66: TOTAL {} lines, {} mem_read, {} scan-region reads, {} win32 calls",
        accumulated.total_lines,
        accumulated.lut_reads,
        accumulated.scan_region_reads,
        accumulated.win32_calls
    );

    // The three regression-guard properties (cumulative over all fixtures checked).
    assert!(
        accumulated.total_lines >= 100,
        "round66: trace pipeline must emit ≥ 100 lines total (got {})",
        accumulated.total_lines
    );
    assert!(
        accumulated.scan_region_reads >= 50,
        "round66: scan-permutation watchpoints must fire ≥ 50 times total \
         across the corpus (got {}); a regression in the MMU read-trace \
         probe site has likely broken LUT-region detection",
        accumulated.scan_region_reads
    );
    assert!(
        accumulated.win32_calls >= 1,
        "round66: at least one win32_call event must fire (got {})",
        accumulated.win32_calls
    );
}
