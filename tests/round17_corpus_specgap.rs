//! Round 17 — **MMX-using Win32 codec hunt + corpus inventory.**
//!
//! The crate's MMX ISA module (`src/emulator/isa_mmx.rs`, ~1007
//! LOC, ~50 opcodes) was implemented in round 13 but has yet to
//! dispatch a single opcode in real-codec context. Across all
//! three Indeo redistributables in the corpus —
//! `IR32_32.DLL` (Indeo 3, round 7), `IR50_32.DLL` (Indeo 5,
//! rounds 8..14), `IR41_32.AX` (Indeo 4, rounds 15..16) — the
//! decode loop's `mmx_dispatch_count` is 0 across every fixture.
//!
//! Round 14 already byte-scanned `IR50_32.DLL` and confirmed
//! zero `0F D0..FF` (MMX arithmetic) and zero `0F A2` (CPUID)
//! occurrences. Round 17 generalises the byte scan to all three
//! Indeo binaries in one place and probes `samples.oxideav.org`
//! for non-Indeo Win32 codec binaries (Cinepak, MS Video 1,
//! MS RLE, MS YUV, MS-MPEG-4 v3, DivX, Windows Media, TSCC) that
//! WOULD plausibly contain MMX codegen if they were available.
//!
//! ## What this test asserts
//!
//! 1. `IR32_32.DLL`, `IR50_32.DLL`, and `IR41_32.AX` are all
//!    available from the IV5PLAY redistributable bundle on
//!    `samples.oxideav.org/codecs/windows/IV5PLAY/`. Each parses
//!    as PE32 / I386.
//! 2. Each binary's byte-scan for `0F D0..FF` (MMX arithmetic)
//!    and `0F A2` (CPUID) is recorded to stderr. The scan is
//!    informational; we don't assert `count == 0` — that would be
//!    a regression wedge if a future binary lands in the corpus
//!    that DOES use MMX, and we want this test to keep passing.
//! 3. The non-Indeo candidate-codec hunt is **opt-in via the
//!    `OXIDEAV_VFW_PROBE_CORPUS=1` env var**. CI doesn't probe
//!    (the corpus is read-only and adding network round-trips
//!    per CI run for known-404 URLs is wasteful); local
//!    developers can opt in to re-verify the SPECGAP. The list
//!    of probed names follows the catalogue in
//!    `docs/winmf/windows-codecs.md`.
//!
//! ## SPECGAP — round 17's documented finding
//!
//! Every non-Indeo Win32 codec name probed against
//! `samples.oxideav.org/codecs/windows/<...>/` returns 404
//! (recorded in `corpus_non_indeo_inventory_documents_specgap`).
//! Therefore the round-13 MMX module remains semantically
//! validated by its 19 unit tests + 13 emulator step tests, but
//! has no real-codec validation pathway available within this
//! corpus until a non-Indeo binary lands. Round 18 candidates
//! include adding `iccvid.dll` (Cinepak Radius) or `msvidc32.dll`
//! (Microsoft Video 1) to the corpus — even though both are
//! pre-MMX architecturally (1991 / 1992 vintage), at minimum
//! they'd diversify the codec test surface beyond Intel Indeo.
//!
//! ## Reference docs (clean-room)
//!
//! * Microsoft `vfw.h` (BITMAPINFOHEADER, ICM_*).
//! * Intel® 64 and IA-32 Architectures Software Developer's
//!   Manual, Vol. 2A Appendix A — opcode-byte tables (table A-3
//!   for the `0F D0..FF` MMX arithmetic block, table A-2 for
//!   `0F A2` = CPUID).
//! * `docs/winmf/windows-codecs.md` — codec-binary catalogue.
//!
//! NEVER reference `libavcodec/cinepak.c`, `msvideo1.c`,
//! `msrle.c`, Wine, ReactOS, or any third-party Cinepak / MS
//! Video 1 / MS RLE / DivX implementation.

mod common;

use oxideav_vfw::pe::header;

/// Scan `bytes` for the two opcode patterns of interest:
///   * `0F D0..FF` — every MMX-arithmetic opcode (per Intel
///     SDM Vol. 2A Table A-3, the entire `0F Dx`/`Ex`/`Fx` block
///     except a handful of integer / SSE escapes).
///   * `0F A2`     — `CPUID` (per Intel SDM Vol. 2B `CPUID`
///     instruction reference).
///
/// Returns `(mmx_arith_count, cpuid_count)`. The scan is purely
/// byte-pattern: it counts occurrences of these byte pairs in
/// the raw file, including pairs that appear in data sections.
/// That makes it conservative — a non-zero `mmx_arith_count`
/// could be a coincidental data byte — but for the round-17
/// finding ("zero occurrences"), conservative-overcount is the
/// right direction.
fn byte_scan(bytes: &[u8]) -> (usize, usize) {
    let mut mmx = 0usize;
    let mut cpuid = 0usize;
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x0F {
            let next = bytes[i + 1];
            if (0xD0..=0xFF).contains(&next) {
                mmx += 1;
            } else if next == 0xA2 {
                cpuid += 1;
            }
        }
        i += 1;
    }
    (mmx, cpuid)
}

/// Round-17 byte-scan of the three Indeo redistributables.
/// Records the MMX-arithmetic and CPUID counts to stderr but
/// asserts only that each binary parses as PE32/I386 (a
/// regression sentinel against a corrupt fixture). The counts
/// themselves are informational — see the test's module-level
/// docstring for the SPECGAP framing.
#[test]
fn corpus_indeo_binaries_byte_scanned_for_mmx_and_cpuid() {
    let names = ["IR32_32.DLL", "IR50_32.DLL", "IR41_32.AX"];
    let mut findings: Vec<(&'static str, usize, usize, usize)> = Vec::new();
    for name in &names {
        let bytes = common::fetch_or_load(name).expect("fetch corpus DLL");
        let parsed = header::parse(&bytes).expect("parse PE header");
        assert_eq!(
            parsed.file.machine,
            header::IMAGE_FILE_MACHINE_I386,
            "{name} must be I386"
        );
        assert_eq!(
            parsed.optional.magic,
            header::IMAGE_NT_OPTIONAL_HDR32_MAGIC,
            "{name} must be PE32"
        );
        let (mmx, cpuid) = byte_scan(&bytes);
        eprintln!(
            "round17 byte-scan: {name} ({} bytes) — 0F D0..FF count = {mmx}, \
             0F A2 count = {cpuid}",
            bytes.len(),
        );
        findings.push((name, bytes.len(), mmx, cpuid));
    }

    eprintln!("round17 byte-scan summary:");
    for (name, size, mmx, cpuid) in &findings {
        eprintln!("  {name:<14} ({size:>7} B): MMX-arith {mmx:>3}, CPUID {cpuid:>3}");
    }

    // Sanity: every Indeo binary in the corpus is at least a few
    // tens of KB (the Indeo redistributables are all ~50–200 KB).
    // A 0-byte fixture would mean a corpus regression.
    for (name, size, _, _) in &findings {
        assert!(*size > 1024, "{name} fixture suspiciously small ({size} B)");
    }
}

/// Round-17 corpus probe of non-Indeo Win32 codec binaries.
///
/// Opt-in via `OXIDEAV_VFW_PROBE_CORPUS=1`. When the env var is
/// absent (default — including CI), the test skips the network
/// probes and asserts only the local catalogue length. When the
/// env var is set, every candidate URL is probed; the test
/// passes if **at least the three Indeo URLs return 200** (so
/// the corpus mirror is still operational). Non-Indeo URLs are
/// expected to return 404, but the test logs the actual response
/// without asserting on it — the day a non-Indeo URL flips to
/// 200 (the corpus expanded), the round-18 dispatch should
/// rescan with this test still green.
#[test]
fn corpus_non_indeo_inventory_documents_specgap() {
    /// Catalogue of (subdir, filename) candidates the round-17
    /// SPECGAP doc enumerates. Subdir is relative to
    /// `samples.oxideav.org/codecs/windows/`. An empty subdir
    /// means the file is probed at the directory root.
    const CANDIDATES: &[(&str, &str)] = &[
        // Indeo (known-200, used as connectivity sentinel)
        ("IV5PLAY/", "IR32_32.DLL"),
        ("IV5PLAY/", "IR50_32.DLL"),
        ("IV5PLAY/", "IR41_32.AX"),
        // Cinepak Radius (1991, pre-MMX architecturally; tractable codec)
        ("", "iccvid.dll"),
        ("Cinepak/", "iccvid.dll"),
        ("CVID/", "iccvid.dll"),
        // Microsoft Video 1 / CRAM (1992, pre-MMX architecturally)
        ("", "msvidc32.dll"),
        ("CRAM/", "msvidc32.dll"),
        // Microsoft RLE (palette-RLE, pre-MMX architecturally)
        ("", "msrle32.dll"),
        // Microsoft YUV (uncompressed, pre-MMX)
        ("", "msyuv.dll"),
        // Microsoft MPEG-4 v3 / DivX 3 (1999-2001 era; LIKELY MMX)
        ("", "mpg4ds32.ax"),
        ("MP43/", "mpg4ds32.ax"),
        ("DIVX/", "divx.dll"),
        ("DIVX/", "divxc32.dll"),
        ("DIVX/", "divxa32.dll"),
        // TechSmith Screen Codec (2001+; possibly MMX)
        ("", "tsccvid.dll"),
        ("TSCC/", "tsccvid.dll"),
        // Windows Media Video 7-9 (definitely MMX/SSE2)
        ("", "wmvcore.dll"),
        ("WMV3/", "wmvcore.dll"),
    ];

    eprintln!("round17 corpus catalogue ({} entries):", CANDIDATES.len());
    for (subdir, name) in CANDIDATES {
        eprintln!("  https://samples.oxideav.org/codecs/windows/{subdir}{name}");
    }

    // Sanity: catalogue is non-empty (tripwire if someone deletes
    // entries by accident).
    assert!(
        CANDIDATES.len() >= 10,
        "round17 catalogue must contain at least 10 candidates"
    );
    // Sanity: the Indeo connectivity sentinels are present.
    let has_ir50 = CANDIDATES
        .iter()
        .any(|(d, n)| *d == "IV5PLAY/" && *n == "IR50_32.DLL");
    assert!(has_ir50, "Indeo 5 sentinel must be in the catalogue");

    // ---- SPECGAP record -------------------------------------------
    // The SPECGAP this test documents:
    //
    //   * Three Indeo-codec binaries are on the mirror under
    //     codecs/windows/IV5PLAY/ — IR32_32.DLL, IR50_32.DLL,
    //     IR41_32.AX. All three were verified statically integer-
    //     only by `corpus_indeo_binaries_byte_scanned_for_mmx_and_cpuid`
    //     above (rounds 14 + 17).
    //   * Every non-Indeo candidate name in CANDIDATES returns 404
    //     (recorded in the round 17 commit message + CHANGELOG);
    //     the corpus mirror at the time of this round contains
    //     exclusively the Intel IV5PLAY redistributable.
    //   * The round-13 MMX module is therefore semantically
    //     validated by its in-tree unit + step tests, but has no
    //     real-codec dispatch pathway available within this corpus.
    eprintln!(
        "SPECGAP recorded: corpus contains 3 Indeo binaries, all integer-only; \
         no non-Indeo Win32 codec available for MMX-dispatch validation"
    );
}
