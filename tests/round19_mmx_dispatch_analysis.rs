//! Round 19 — **Lead A: trace-coverage analysis of MMX-byte
//! reachability in `IR41_32.AX` and `IR50_32.DLL`.**
//!
//! ## Why this test exists
//!
//! Round 17's byte-scan demonstrated that the three Indeo
//! redistributables in our corpus DO contain MMX-arithmetic
//! opcodes (`0F D0..FF`, per Intel SDM Vol. 2A Table A-3) and
//! `CPUID` (`0F A2`, Vol. 2B):
//!
//! | Binary       | size (B) | `0F D0..FF` count | `0F A2` count |
//! | ------------ | -------- | ----------------- | ------------- |
//! | IR32_32.DLL  |   199168 |              146 |             0 |
//! | IR50_32.DLL  |   739328 |             2518 |             2 |
//! | IR41_32.AX   |   848384 |             1094 |             2 |
//!
//! …and yet rounds 12..17 multi-frame decode tests all report
//! `mmx_dispatch_count = 0` and `cpuid_dispatch_count = 0`. The
//! suspected reason: a CPUID-feature-bit branch we miss at
//! `DRV_LOAD` time selects an integer-only fast path, OR the MMX
//! bytes live exclusively in the encoder side of the codec
//! (`ICCompress*`) which our decode-only pipeline never touches.
//!
//! Round 19 attacks the first hypothesis directly. We:
//!
//! 1. Parse the PE32 image headers to locate every executable
//!    section's `[file_off, file_off + size_of_raw_data)` range.
//! 2. Byte-scan **only the executable sections** (data sections
//!    will produce false positives on the byte pattern; we want
//!    the real instruction-stream count).
//! 3. Record each `0F D0..FF` and `0F A2` byte's `(file_off, va)`
//!    where `va = image_base + virtual_address + (file_off -
//!    pointer_to_raw_data)`.
//! 4. Drive the standard 8-frame `indeo41.avi` (IV41) decode
//!    pipeline with `Cpu::enable_visited_eip_tracking()` on,
//!    accumulating the set of every distinct entry-EIP the
//!    interpreter ever stepped through.
//! 5. **Set-difference**: which MMX-byte VAs are members of the
//!    visited-EIP set (= reached) and which are not (= unreachable
//!    via the decode path).
//! 6. For the unreached `0F A2` (CPUID) sites, dump the 32 bytes
//!    immediately preceding the opcode. The CPUID instruction is
//!    almost always preceded by `mov eax, <leaf>` (`b8 XX 00 00
//!    00`); the surrounding bytes tell us whether the codec is
//!    even querying CPUID at runtime or if both `0F A2`
//!    occurrences live in dead code.
//!
//! ## What this test asserts
//!
//! * The PE32 image parses + at least one executable section is
//!   found (sanity tripwire vs. corrupt fixture).
//! * The 8-frame decode runs to completion with
//!   `frames_ok >= 4`. (Same milestone as round 17 Part B.)
//! * The byte-scan finds at least one MMX-byte and at least one
//!   CPUID-byte in `IR41_32.AX`'s executable section. (If the
//!   round-17 byte-scan was right, it must.)
//!
//! Crucially we DO NOT assert how many MMX bytes were reached —
//! the test is a research instrument, not a milestone gate.
//! The findings are recorded to stderr so the round-19 commit
//! message + CHANGELOG can quote the exact set-difference.
//!
//! ## Reference docs (clean-room)
//!
//! * Microsoft "PE Format" specification §"Section Table" (RVA
//!   ↔ file-offset arithmetic, executable section flag).
//! * Intel® 64 and IA-32 Architectures Software Developer's
//!   Manual Vol. 2A Appendix A Table A-3 (`0F D0..FF` MMX block).
//! * Intel® SDM Vol. 2B `CPUID` reference (`0F A2`).
//! * `docs/winmf/winmf-emulator.md` — the crate's own design
//!   contract (instruction-set + trace mode).
//! * `docs/video/indeo/indeo4/wiki/Indeo_4.wiki` — the only
//!   docs reference for the codec under test; consulted only for
//!   per-frame keyframe-vs-P-frame discrimination.
//!
//! NEVER reference `libavcodec/indeo4.c`, Wine's `dlls/quartz`,
//! ReactOS, or any third-party Indeo decoder source.

mod common;

use oxideav_vfw::pe::header::{self, IMAGE_SCN_MEM_EXECUTE};
use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};
use oxideav_vfw::Sandbox;
use std::collections::BTreeSet;

/// One occurrence of an opcode byte pattern in a PE32 binary.
#[derive(Debug, Clone)]
struct OpcodeHit {
    /// File offset of the leading `0F` byte.
    file_off: u32,
    /// Virtual address of the leading `0F` byte after PE
    /// loading: `image_base + virtual_address + (file_off -
    /// pointer_to_raw_data)`.
    va: u32,
    /// The second byte of the two-byte opcode. For `0F D0..FF`
    /// this disambiguates which MMX op (PSUBUSB / PMULLW / …);
    /// for `0F A2` it's always `0xA2`.
    second_byte: u8,
}

/// Pattern category — used to group the byte-scan output.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum HitKind {
    /// `0F D0..FF` — MMX arithmetic / saturation / pack /
    /// unpack / shift / compare / move (per SDM Vol. 2A Table
    /// A-3).
    MmxArith,
    /// `0F A2` — CPUID.
    Cpuid,
}

impl HitKind {
    fn classify(second: u8) -> Option<Self> {
        if second == 0xA2 {
            Some(Self::Cpuid)
        } else if (0xD0..=0xFF).contains(&second) {
            Some(Self::MmxArith)
        } else {
            None
        }
    }
}

/// Scan-result aggregate: every `0F D0..FF` / `0F A2` opcode-byte
/// hit found in the executable sections, plus a running count of
/// scanned bytes and a per-section description.
type ScanResult = (Vec<(HitKind, OpcodeHit)>, u32, Vec<String>);

/// Scan the executable sections of `bytes` for `0F D0..FF` and
/// `0F A2`. Returns `(hits, total_exec_bytes_scanned, exec_section_descriptions)`.
fn scan_executable_sections(bytes: &[u8]) -> Result<ScanResult, Box<dyn std::error::Error>> {
    let parsed = header::parse(bytes).map_err(|e| format!("PE parse: {e}"))?;
    let image_base = parsed.optional.image_base;
    let mut hits: Vec<(HitKind, OpcodeHit)> = Vec::new();
    let mut total_exec_bytes = 0u32;
    let mut descriptions: Vec<String> = Vec::new();

    for sh in &parsed.sections {
        if (sh.characteristics & IMAGE_SCN_MEM_EXECUTE) == 0 {
            continue;
        }
        let raw_off = sh.pointer_to_raw_data as usize;
        let raw_size = sh.size_of_raw_data as usize;
        let virt_size = sh.virtual_size as usize;
        let scan_size = raw_size.min(virt_size);
        let end = raw_off.saturating_add(scan_size);
        if end > bytes.len() {
            descriptions.push(format!(
                "{}: file-offset {raw_off:#x}+{scan_size} exceeds image size {} — \
                 scanning the in-bounds prefix",
                sh.name,
                bytes.len(),
            ));
        }
        let end = end.min(bytes.len());
        let section = &bytes[raw_off..end];
        let section_va_start = image_base.wrapping_add(sh.virtual_address);
        descriptions.push(format!(
            "{} VA={section_va_start:#010x} file_off={raw_off:#x} \
             size={scan_size} (X)",
            sh.name,
        ));
        total_exec_bytes = total_exec_bytes.saturating_add(section.len() as u32);

        let mut i = 0usize;
        while i + 1 < section.len() {
            if section[i] == 0x0F {
                let second = section[i + 1];
                if let Some(kind) = HitKind::classify(second) {
                    let file_off = (raw_off + i) as u32;
                    let va = section_va_start.wrapping_add(i as u32);
                    hits.push((
                        kind,
                        OpcodeHit {
                            file_off,
                            va,
                            second_byte: second,
                        },
                    ));
                }
            }
            i += 1;
        }
    }
    Ok((hits, total_exec_bytes, descriptions))
}

/// Format the 32 bytes preceding `file_off` as a hex listing,
/// suitable for jotting next to a "this opcode-byte was not
/// reached" line. Used to surface the gating branch.
fn preceding_bytes_hex(bytes: &[u8], file_off: u32, n: usize) -> String {
    let off = file_off as usize;
    let lo = off.saturating_sub(n);
    let slice = &bytes[lo..off];
    let mut s = String::with_capacity(slice.len() * 3);
    for b in slice {
        s.push_str(&format!("{b:02x} "));
    }
    s.trim_end().to_string()
}

/// Format the `n` bytes starting at `file_off` as a hex listing.
/// The companion to [`preceding_bytes_hex`]; used to look at the
/// instruction stream AFTER a CPUID so we can read the feature-
/// bit test the codec performs on the result.
fn following_bytes_hex(bytes: &[u8], file_off: u32, n: usize) -> String {
    let off = file_off as usize;
    let hi = (off + n).min(bytes.len());
    let slice = &bytes[off..hi];
    let mut s = String::with_capacity(slice.len() * 3);
    for b in slice {
        s.push_str(&format!("{b:02x} "));
    }
    s.trim_end().to_string()
}

/// Per-globals snapshot of the codec's "use MMX" decision flags
/// in `IR41_32.AX` after the DRV_LOAD-time CPUID block ran.
/// Addresses derived from the round-19 disassembly of file_off
/// 0x31ab8..0x31b40 (see test stderr output for the byte trace
/// the snapshot key positions are derived from).
#[derive(Debug, Default, Clone, Copy)]
struct Iv41CpuFlagSnapshot {
    /// `[0x1c4a9a38]` — "use MMX kernels" decision; expected to
    /// be 1 if MMX-bit is reported AND the per-instance enable
    /// was non-zero.
    use_mmx_flag: u32,
    /// `[0x1c4a9a54]` — raw MMX feature mask (0x800000 if MMX
    /// reported, 0xFFFFFFFF if family>=6 + MMX path bypassed).
    mmx_mask: u32,
    /// `[0x1c4a9a40]` — SSE feature mask (0x02000000 if SSE).
    sse_mask: u32,
    /// `[0x1c4a9a58]` — captured family byte.
    family: u32,
    /// `[0x1c4a9a3c]` — codec mode flag (the codec compares this
    /// to 1 / 2 to choose between integer / MMX dispatchers).
    mode: u32,
}

/// Drive `indeo41.avi` through the IR41 pipeline with unique-EIP
/// tracking on. Mirrors the round-17B test almost verbatim; the
/// new bit is `Cpu::enable_visited_eip_tracking()` + the
/// `take_visited_eips` at the end. Returns
/// `(frames_ok, total_instr_count, visited_eips, snapshot)`.
#[allow(clippy::type_complexity)]
fn run_iv41_with_tracking(
) -> Result<(usize, u64, BTreeSet<u32>, Iv41CpuFlagSnapshot), Box<dyn std::error::Error>> {
    /// vfw.h: `ICMODE_DECOMPRESS = 2`.
    const ICMODE_DECOMPRESS: u32 = 2;
    const NUM_FRAMES: u32 = 8;

    let dll_bytes = common::fetch_or_load("IR41_32.AX")?;
    let avi = common::fetch_or_load_ffmpeg_sample("IV41", "indeo41.avi")?;
    let s0 = common::avi_extractor::extract_first_video_sample(&avi)?;
    let width = s0.width;
    let height = s0.height;

    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(1_500_000_000);
    sb.cpu.enable_visited_eip_tracking();

    let img = sb.load("IR41_32.AX", &dll_bytes)?;
    sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)?;
    sb.install_codec(&img)?;

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"IV41");
    let hic = sb.ic_open(fcc_video, fcc_handler, ICMODE_DECOMPRESS)?;
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

    let q = sb.ic_decompress_query(hic, &bih_in, Some(&bih_out))?;
    assert_eq!(q, 0, "ICDecompressQuery → ICERR_OK");
    let b = sb.ic_decompress_begin(hic, &bih_in, &bih_out)?;
    assert_eq!(b, 0, "ICDecompressBegin → ICERR_OK");

    let out_capacity = width * height * 3;
    let mut frames_ok = 0usize;
    for n in 0..NUM_FRAMES {
        let sample = match common::avi_extractor::extract_video_sample(&avi, n) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("round19: AVI walker stopped at sample {n}: {e}");
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
        match sb.ic_decompress(hic, flags, &bih_in_n, &sample.bytes, &bih_out, out_capacity) {
            Ok((0, _out)) => frames_ok += 1,
            Ok((lr, _)) => eprintln!(
                "round19: sample {n} returned non-OK lr={lr:#x} ({})",
                lr as i32,
            ),
            Err(e) => {
                eprintln!("round19: sample {n} trapped: {e}");
                break;
            }
        }
    }

    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);

    // Snapshot the codec's MMX-decision globals AFTER decode so
    // we capture whatever final state the codec settled on. The
    // addresses live in IR41's data section; load via the MMU
    // (each is a u32 little-endian).
    let snapshot = Iv41CpuFlagSnapshot {
        use_mmx_flag: sb.mmu.load32(0x1c4a_9a38).unwrap_or(0),
        mmx_mask: sb.mmu.load32(0x1c4a_9a54).unwrap_or(0),
        sse_mask: sb.mmu.load32(0x1c4a_9a40).unwrap_or(0),
        family: sb.mmu.load32(0x1c4a_9a58).unwrap_or(0),
        mode: sb.mmu.load32(0x1c4a_9a3c).unwrap_or(0),
    };

    let total_instr = sb.cpu.instr_count;
    let visited = sb.cpu.take_visited_eips();
    Ok((frames_ok, total_instr, visited, snapshot))
}

/// Lead-A test: byte-scan IR41_32.AX's executable sections,
/// run the IV41 decode pipeline with unique-EIP tracking on,
/// then compute the set-difference.
#[test]
fn ir41_mmx_byte_reachability_during_iv41_decode() {
    let dll_bytes = common::fetch_or_load("IR41_32.AX").expect("fetch IR41_32.AX");
    let parsed = header::parse(&dll_bytes).expect("parse PE32");
    assert_eq!(
        parsed.file.machine,
        header::IMAGE_FILE_MACHINE_I386,
        "IR41_32.AX must be I386"
    );

    let (hits, total_exec_bytes, sec_descs) =
        scan_executable_sections(&dll_bytes).expect("scan exec sections");
    eprintln!(
        "round19: IR41_32.AX executable sections ({} bytes total):",
        total_exec_bytes,
    );
    for d in &sec_descs {
        eprintln!("  {d}");
    }

    let mut mmx_hits: Vec<&OpcodeHit> = hits
        .iter()
        .filter(|(k, _)| *k == HitKind::MmxArith)
        .map(|(_, h)| h)
        .collect();
    let mut cpuid_hits: Vec<&OpcodeHit> = hits
        .iter()
        .filter(|(k, _)| *k == HitKind::Cpuid)
        .map(|(_, h)| h)
        .collect();
    mmx_hits.sort_by_key(|h| h.va);
    cpuid_hits.sort_by_key(|h| h.va);

    eprintln!(
        "round19: IR41_32.AX exec-section byte-scan — \
         MMX-arith (0F D0..FF): {} occurrences, CPUID (0F A2): {} occurrences",
        mmx_hits.len(),
        cpuid_hits.len(),
    );

    // Sanity: round 17 reported 1094 0F D0..FF + 2 0F A2 across
    // the whole binary. Our exec-section-restricted scan will
    // produce fewer (data-section false positives are excluded)
    // but should still find a meaningful number.
    assert!(
        !mmx_hits.is_empty(),
        "round19 precondition: IR41_32.AX must have at least one \
         0F D0..FF byte in an executable section"
    );
    assert!(
        !cpuid_hits.is_empty(),
        "round19 precondition: IR41_32.AX must have at least one \
         0F A2 byte in an executable section"
    );

    // ---- Run the decode pipeline ----------------------------------
    let (frames_ok, total_instr, visited, snapshot) =
        run_iv41_with_tracking().expect("run IV41 pipeline");

    eprintln!(
        "round19: decode-pipeline result — {frames_ok}/8 frames OK, \
         {total_instr} instructions executed, {} unique EIPs visited",
        visited.len(),
    );
    eprintln!(
        "round19: post-decode codec global snapshot — \
         [0x1c4a9a38] use_mmx={:#x}, [0x1c4a9a54] mmx_mask={:#x}, \
         [0x1c4a9a40] sse_mask={:#x}, [0x1c4a9a58] family={:#x}, \
         [0x1c4a9a3c] mode={:#x}",
        snapshot.use_mmx_flag, snapshot.mmx_mask, snapshot.sse_mask, snapshot.family, snapshot.mode,
    );

    // ---- Set-difference: MMX-byte VAs reached vs not --------------

    let mmx_reached: Vec<&OpcodeHit> = mmx_hits
        .iter()
        .copied()
        .filter(|h| visited.contains(&h.va))
        .collect();
    let mmx_unreached: Vec<&OpcodeHit> = mmx_hits
        .iter()
        .copied()
        .filter(|h| !visited.contains(&h.va))
        .collect();
    let cpuid_reached: Vec<&OpcodeHit> = cpuid_hits
        .iter()
        .copied()
        .filter(|h| visited.contains(&h.va))
        .collect();
    let _cpuid_unreached: Vec<&OpcodeHit> = cpuid_hits
        .iter()
        .copied()
        .filter(|h| !visited.contains(&h.va))
        .collect();

    eprintln!(
        "round19: MMX-arith reachability — {}/{} reached during decode",
        mmx_reached.len(),
        mmx_hits.len(),
    );
    eprintln!(
        "round19: CPUID reachability — {}/{} reached during decode",
        cpuid_reached.len(),
        cpuid_hits.len(),
    );

    // Sample of reached MMX bytes (up to 5).
    if !mmx_reached.is_empty() {
        eprintln!("round19: sample of reached MMX-arith bytes:");
        for h in mmx_reached.iter().take(5) {
            eprintln!(
                "  va={:#010x} file_off={:#x} second_byte=0x{:02x}",
                h.va, h.file_off, h.second_byte,
            );
        }
    }

    // Every CPUID site — there are only ~2 — with surrounding
    // bytes. Even if both were unreached, this tells us whether
    // the codec is statically built for CPUID-feature dispatch
    // and whether the test of MMX bit might happen elsewhere.
    eprintln!("round19: CPUID (0F A2) sites — full inventory:");
    for h in &cpuid_hits {
        let preceding = preceding_bytes_hex(&dll_bytes, h.file_off, 64);
        let following = following_bytes_hex(&dll_bytes, h.file_off, 96);
        let reached = visited.contains(&h.va);
        eprintln!(
            "  va={:#010x} file_off={:#x} reached={reached}",
            h.va, h.file_off,
        );
        eprintln!("    preceding 32 B: {preceding}");
        eprintln!("    following 96 B: {following}");
    }

    // For the unreached MMX-arith sites, dump the FIRST 5 + LAST
    // 5 sites' surrounding context. (Dumping all ~1000 would be
    // unmanageable.)
    if !mmx_unreached.is_empty() {
        eprintln!(
            "round19: first 5 unreached MMX-arith bytes \
             (preceding 32 B for each):"
        );
        for h in mmx_unreached.iter().take(5) {
            let preceding = preceding_bytes_hex(&dll_bytes, h.file_off, 32);
            eprintln!(
                "  va={:#010x} file_off={:#x} 0F {:02x}",
                h.va, h.file_off, h.second_byte,
            );
            eprintln!("    preceding: {preceding}");
        }
        if mmx_unreached.len() > 5 {
            eprintln!(
                "round19: last 5 unreached MMX-arith bytes \
                 (preceding 32 B for each):"
            );
            for h in mmx_unreached.iter().rev().take(5).rev() {
                let preceding = preceding_bytes_hex(&dll_bytes, h.file_off, 32);
                eprintln!(
                    "  va={:#010x} file_off={:#x} 0F {:02x}",
                    h.va, h.file_off, h.second_byte,
                );
                eprintln!("    preceding: {preceding}");
            }
        }
    }

    // ---- Round-19 summary line for the commit message --------------
    eprintln!(
        "Round-19 reachability finding (Lead A): IR41_32.AX exec-section MMX-arith \
         {}/{} reached, CPUID {}/{} reached during 8-frame indeo41.avi IV41 decode \
         ({} unique EIPs across {} instructions).",
        mmx_reached.len(),
        mmx_hits.len(),
        cpuid_reached.len(),
        cpuid_hits.len(),
        visited.len(),
        total_instr,
    );

    // ---- Acceptance criterion -------------------------------------
    //
    // Per the dispatch prompt: "at least ONE MMX opcode dispatched
    // in real-codec context, OR a documented finding why the
    // decode-path MMX bytes are unreachable + a plan for round 20."
    //
    // We don't fail on `mmx_reached == 0`; the documented finding
    // is the test's stderr output, the CHANGELOG, and the round-19
    // commit. We DO fail if the multi-frame pipeline regressed.
    assert!(
        frames_ok >= 4,
        "round19 milestone: expected ≥ 4 of 8 IV41 frames to return \
         ICERR_OK; got {frames_ok}",
    );

    // Cheap regression: the visited-EIP set should be substantial.
    // 0 visited EIPs would mean tracking was never engaged.
    assert!(
        visited.len() > 1000,
        "round19 sanity: tracked-visited-EIP set must be substantial \
         (got {} unique EIPs)",
        visited.len(),
    );
}

/// Companion: the same set-difference for the CPUID hits in
/// `IR50_32.DLL`. Mirrors the round-13 `cat_attack.avi` 8-frame
/// IV50 pipeline; if EITHER of IR50's two `0F A2` CPUIDs is
/// reached, the gating is by-CPUID-feature-bit (and we'd flip
/// SSE bits in a round-20 follow-up). If neither is reached,
/// the gating is static — the MMX paths are unconditionally
/// not selected on this build.
#[test]
fn ir50_cpuid_reachability_during_iv50_decode() {
    use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};

    /// vfw.h: `ICMODE_DECOMPRESS = 2`.
    const ICMODE_DECOMPRESS: u32 = 2;
    const NUM_FRAMES: u32 = 8;

    let dll_bytes = common::fetch_or_load("IR50_32.DLL").expect("fetch IR50_32.DLL");
    let avi = common::fetch_or_load_ffmpeg_sample("IV50", "cat_attack.avi")
        .expect("fetch cat_attack.avi");

    let parsed = header::parse(&dll_bytes).expect("parse PE32");
    assert_eq!(parsed.file.machine, header::IMAGE_FILE_MACHINE_I386);

    let (hits, total_exec_bytes, sec_descs) =
        scan_executable_sections(&dll_bytes).expect("scan exec sections");
    eprintln!(
        "round19: IR50_32.DLL executable sections ({} bytes total):",
        total_exec_bytes,
    );
    for d in &sec_descs {
        eprintln!("  {d}");
    }
    let mut mmx_hits: Vec<&OpcodeHit> = hits
        .iter()
        .filter(|(k, _)| *k == HitKind::MmxArith)
        .map(|(_, h)| h)
        .collect();
    let mut cpuid_hits: Vec<&OpcodeHit> = hits
        .iter()
        .filter(|(k, _)| *k == HitKind::Cpuid)
        .map(|(_, h)| h)
        .collect();
    mmx_hits.sort_by_key(|h| h.va);
    cpuid_hits.sort_by_key(|h| h.va);

    eprintln!(
        "round19: IR50_32.DLL exec-section byte-scan — \
         MMX-arith {} occurrences, CPUID {} occurrences",
        mmx_hits.len(),
        cpuid_hits.len(),
    );

    // ---- Drive IV50 decode -----
    let s0 =
        common::avi_extractor::extract_first_video_sample(&avi).expect("AVI walker on cat_attack");
    let width = s0.width;
    let height = s0.height;

    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(1_500_000_000);
    sb.cpu.enable_visited_eip_tracking();

    let img = sb.load("IR50_32.DLL", &dll_bytes).expect("load");
    sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .expect("DllMain");
    sb.install_codec(&img).expect("install_codec");

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"IV50");
    let hic = sb
        .ic_open(fcc_video, fcc_handler, ICMODE_DECOMPRESS)
        .expect("ic_open IV50");

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
        compression: [0; 4],
        size_image: width * height * 3,
        ..Default::default()
    };

    let _ = sb.ic_decompress_query(hic, &bih_in, Some(&bih_out));
    let _ = sb.ic_decompress_begin(hic, &bih_in, &bih_out);
    let out_capacity = width * height * 3;
    let mut frames_ok = 0usize;
    for n in 0..NUM_FRAMES {
        let sample = match common::avi_extractor::extract_video_sample(&avi, n) {
            Ok(s) => s,
            Err(_) => break,
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
        match sb.ic_decompress(hic, flags, &bih_in_n, &sample.bytes, &bih_out, out_capacity) {
            Ok((0, _)) => frames_ok += 1,
            _ => break,
        }
    }
    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);

    let total_instr = sb.cpu.instr_count;
    let visited = sb.cpu.take_visited_eips();

    let mmx_reached = mmx_hits.iter().filter(|h| visited.contains(&h.va)).count();
    let cpuid_reached_set: Vec<&OpcodeHit> = cpuid_hits
        .iter()
        .copied()
        .filter(|h| visited.contains(&h.va))
        .collect();

    eprintln!(
        "round19: IR50_32.DLL — {frames_ok}/8 frames OK, \
         {total_instr} instructions, {} unique EIPs visited",
        visited.len(),
    );
    eprintln!(
        "round19: IR50_32.DLL MMX-arith reachability — {}/{} reached",
        mmx_reached,
        mmx_hits.len(),
    );
    eprintln!(
        "round19: IR50_32.DLL CPUID reachability — {}/{} reached",
        cpuid_reached_set.len(),
        cpuid_hits.len(),
    );
    for h in &cpuid_hits {
        let reached = visited.contains(&h.va);
        let preceding = preceding_bytes_hex(&dll_bytes, h.file_off, 32);
        let following = following_bytes_hex(&dll_bytes, h.file_off, 64);
        eprintln!(
            "  IR50 CPUID va={:#010x} file_off={:#x} reached={reached}",
            h.va, h.file_off,
        );
        eprintln!("    preceding 32 B: {preceding}");
        eprintln!("    following 64 B: {following}");
    }

    eprintln!(
        "Round-19 reachability finding (IR50): MMX-arith {}/{} reached, \
         CPUID {}/{} reached during 8-frame cat_attack.avi IV50 decode ({} \
         unique EIPs across {} instructions).",
        mmx_reached,
        mmx_hits.len(),
        cpuid_reached_set.len(),
        cpuid_hits.len(),
        visited.len(),
        total_instr,
    );

    assert!(frames_ok >= 4, "round19 IV50 baseline must hold");
}
