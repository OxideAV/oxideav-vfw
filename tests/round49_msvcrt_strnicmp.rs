//! Round 49 — `msvcrt!_strnicmp` stub + `msadds32.ax` PE-load
//! surface advance.
//!
//! ## Background
//!
//! Round 48 added `msvcrt!_endthreadex` as a fail-soft no-op stub
//! and pushed `Sandbox::load("msadds32.ax")` past the splitter's
//! thread-teardown edge.  The next unresolved import the splitter
//! pulls is `msvcrt!_strnicmp` — the case-insensitive bounded
//! ASCII string compare.  Unlike `_endthreadex`, this one is
//! actually called during init for FOURCC / header-magic
//! matching, so a stub returning a constant 0 ("every string
//! compares equal") would let the codec take a wrong branch and
//! silently misbehave on a real decode.  Round 49 wires the real
//! ASCII-tolower bounded compare.
//!
//! ## Stub semantics
//!
//! `int __cdecl _strnicmp(const char *string1, const char
//! *string2, size_t count)` — cdecl (caller-cleanup), 3 dwords on
//! the stack.  Returns `< 0` if `string1` is less than `string2`,
//! `0` if equal up to `count`, `> 0` if greater (just like
//! `strcmp`).
//!
//! Implementation choices (all documented in
//! `src/win32/msvcrt.rs::stub_strnicmp`):
//!
//! * Each byte is folded to lowercase by the ASCII rule
//!   `b'A'..=b'Z' → +0x20`; bytes ≥ `0x80` are compared
//!   byte-for-byte (no Unicode tolower).
//! * Comparison terminates early at the first NUL on EITHER
//!   side within `count` bytes.
//! * `count == 0` returns 0.
//! * `count > 1 MiB` or any out-of-bounds pointer returns 0
//!   (fail-soft envelope).
//!
//! ## References (clean-room, on-disk)
//!
//! * `docs/winmf/winmf-emulator.md` — splitter import-walk
//!   inventory; `msvcrt!_strnicmp` is the post-r48 edge symbol.
//! * MSDN `_strnicmp, _wcsnicmp, _mbsnicmp,
//!   _strnicmp_l, _wcsnicmp_l, _mbsnicmp_l`:
//!   <https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/strnicmp-wcsnicmp-mbsnicmp-strnicmp-l-wcsnicmp-l-mbsnicmp-l>
//!
//! ## What we deliberately do NOT do
//!
//! Drive `msadds32.ax` through `DLL_PROCESS_ATTACH` /
//! `DriverProc`.  Per the round-24 / r45 / r46 / r47 / r48
//! follow-up scope we just "wire the stub, don't drive
//! msadds32".  Future rounds that decide to exercise the
//! splitter's window-pump path will need to extend more stubs as
//! the next blocker surfaces.

use oxideav_vfw::emulator::isa_int::RET_SENTINEL;
use oxideav_vfw::emulator::regs::Reg32;
use oxideav_vfw::win32::Registry;
use oxideav_vfw::Sandbox;
use std::path::PathBuf;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn msadds32_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/msadds32.ax");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// Stage two NUL-terminated ASCII byte slices into the sandbox's
/// const-arena and call `_strnicmp(s1, s2, count)` end-to-end via
/// the dispatcher.  Returns the dword left in `eax` (the codec
/// re-interprets it as a signed `int`).
fn call_strnicmp(s1: &[u8], s2: &[u8], count: u32) -> u32 {
    let mut sb = Sandbox::new();
    let p1 = sb.host.arena_const_alloc(s1.len() as u32 + 1).unwrap();
    sb.mmu.write_initializer(p1, s1).unwrap();
    sb.mmu
        .write_initializer(p1 + s1.len() as u32, &[0u8])
        .unwrap();
    let p2 = sb.host.arena_const_alloc(s2.len() as u32 + 1).unwrap();
    sb.mmu.write_initializer(p2, s2).unwrap();
    sb.mmu
        .write_initializer(p2 + s2.len() as u32, &[0u8])
        .unwrap();
    let thunk = sb
        .registry
        .resolve("msvcrt.dll", "_strnicmp")
        .expect("_strnicmp registered");
    // cdecl: 3 args.  Push `count`, `s2`, `s1` (right-to-left),
    // then the synthetic return-address sentinel.
    sb.cpu.push32(&mut sb.mmu, count).unwrap();
    sb.cpu.push32(&mut sb.mmu, p2).unwrap();
    sb.cpu.push32(&mut sb.mmu, p1).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    sb.cpu.regs.get32(Reg32::Eax)
}

/// Same as [`call_strnicmp`] but writes the staged bytes EXACTLY
/// (no implicit NUL appended) — used by the test that asserts
/// "NUL within count" terminates the compare.
fn call_strnicmp_raw(s1: &[u8], s2: &[u8], count: u32) -> u32 {
    let mut sb = Sandbox::new();
    let p1 = sb.host.arena_const_alloc(s1.len() as u32).unwrap();
    sb.mmu.write_initializer(p1, s1).unwrap();
    let p2 = sb.host.arena_const_alloc(s2.len() as u32).unwrap();
    sb.mmu.write_initializer(p2, s2).unwrap();
    let thunk = sb
        .registry
        .resolve("msvcrt.dll", "_strnicmp")
        .expect("_strnicmp registered");
    sb.cpu.push32(&mut sb.mmu, count).unwrap();
    sb.cpu.push32(&mut sb.mmu, p2).unwrap();
    sb.cpu.push32(&mut sb.mmu, p1).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    sb.cpu.regs.get32(Reg32::Eax)
}

// ────────────────────────────────────────────────────────────────
// Test 1 — the stub is wired into the msvcrt registry.
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_is_registered_in_msvcrt() {
    let mut r = Registry::new();
    oxideav_vfw::win32::msvcrt::register(&mut r);
    assert!(
        r.resolve("msvcrt.dll", "_strnicmp").is_some(),
        "msvcrt!_strnicmp stub missing — round-49 follow-up"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — equal-prefix case-insensitive compare:
// "AVI " vs "avi " with count=4 returns 0.
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_case_insensitive_equal_prefix_returns_zero() {
    let r = call_strnicmp(b"AVI ", b"avi ", 4);
    assert_eq!(
        r, 0,
        "expected 0 for case-insensitive equal compare; got {r:#x}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 3 — different prefix: "MP43" vs "MP42" with count=4
// returns positive (the differing byte is `'3' (0x33)` vs
// `'2' (0x32)`, so the difference is +1).
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_differing_prefix_returns_positive() {
    let r = call_strnicmp(b"MP43", b"MP42", 4);
    let signed = r as i32;
    assert!(signed > 0, "expected positive for MP43>MP42; got {signed}");
    assert_eq!(signed, 1, "exact difference should be +1 ('3' - '2')");
}

// ────────────────────────────────────────────────────────────────
// Test 4 — shorter compare: "MP43" vs "MP42" with count=3 returns
// 0 (only the first 3 bytes are compared, which all match).
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_shorter_count_returns_zero_on_matching_prefix() {
    let r = call_strnicmp(b"MP43", b"MP42", 3);
    assert_eq!(r, 0, "expected 0 for matching 3-byte prefix; got {r:#x}");
}

// ────────────────────────────────────────────────────────────────
// Test 5 — NUL terminator within count: "AVI\0" vs "AVI\0XYZ"
// with count=7 returns 0 (both sides hit NUL at the same index,
// so the compare ends equal — the trailing "XYZ" bytes after
// `s2`'s NUL are never inspected).
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_nul_terminator_within_count_returns_zero() {
    // Stage RAW bytes — call_strnicmp_raw appends nothing.  Both
    // sides start with `"AVI\0"` so the compare terminates at
    // index 3 with both bytes == 0.
    let r = call_strnicmp_raw(b"AVI\0", b"AVI\0XYZ", 7);
    assert_eq!(
        r, 0,
        "expected 0 when both sides NUL at index 3; got {r:#x}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 6 — non-ASCII byte ≥ 0x80 is compared byte-for-byte (no
// Unicode tolower).  Bytes 0xC0 (Latin capital A grave) vs 0xE0
// (Latin small a grave) are NOT folded to equal; they should
// compare unequal as raw byte values.
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_high_byte_is_byte_for_byte_no_unicode_fold() {
    let r = call_strnicmp(&[0xC0u8], &[0xE0u8], 1);
    let signed = r as i32;
    assert_ne!(signed, 0, "high-byte compare should NOT fold via Unicode");
    // 0xC0 - 0xE0 = -32
    assert_eq!(signed, -32, "exact difference should be 0xC0 - 0xE0 = -32");
}

// ────────────────────────────────────────────────────────────────
// Test 7 — fail-soft on out-of-bounds pointer.  Stage one valid
// string and pass an unmapped guest address as the other; the
// stub should return 0 rather than propagating a Trap.
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_fail_soft_on_oob_pointer() {
    let mut sb = Sandbox::new();
    let p1 = sb.host.arena_const_alloc(8).unwrap();
    sb.mmu.write_initializer(p1, b"AVI \0\0\0\0").unwrap();
    // 0x0000_0010 is unmapped (heap arena starts much higher;
    // const arena is at a separate region).  Confirm by querying
    // the MMU directly.
    let oob = 0x0000_0010u32;
    assert!(
        !sb.mmu.is_mapped(oob),
        "test precondition: {oob:#010x} must be unmapped"
    );
    let thunk = sb
        .registry
        .resolve("msvcrt.dll", "_strnicmp")
        .expect("_strnicmp registered");
    sb.cpu.push32(&mut sb.mmu, 4).unwrap();
    sb.cpu.push32(&mut sb.mmu, oob).unwrap();
    sb.cpu.push32(&mut sb.mmu, p1).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    // The stub should NOT propagate a Trap on the fail-soft
    // boundary case — the dispatcher must run cleanly to the
    // sentinel.
    sb.run_until_sentinel()
        .expect("_strnicmp on OOB pointer must fail-soft, not trap");
    let r = sb.cpu.regs.get32(Reg32::Eax);
    assert_eq!(
        r, 0,
        "fail-soft envelope should return 0 (treat as equal); got {r:#x}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 8 — fail-soft on absurdly large `count`.  Pass two valid
// 4-byte strings and `count = u32::MAX`; the stub should refuse
// to walk past 1 MiB and return 0 rather than trapping when the
// compare runs off the end of the const arena.
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_fail_soft_on_absurd_count() {
    let r = call_strnicmp(b"AVI ", b"AVI ", u32::MAX);
    assert_eq!(
        r, 0,
        "fail-soft envelope should return 0 on count > 1 MiB; got {r:#x}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 9 — `count == 0` returns 0 (vacuously equal per MSDN).
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_count_zero_returns_zero() {
    // Even with totally different strings, count=0 must return 0.
    let r = call_strnicmp(b"FOO", b"BAR", 0);
    assert_eq!(r, 0);
}

// ────────────────────────────────────────────────────────────────
// Test 10 — empty-string sentinel: comparing two zero-length
// strings (NUL at index 0 on both sides) with count=1 returns 0.
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_both_empty_returns_zero() {
    let r = call_strnicmp_raw(b"\0", b"\0", 1);
    assert_eq!(r, 0);
}

// ────────────────────────────────────────────────────────────────
// Test 11 — early-NUL-on-one-side: "AVI\0" vs "AVIX" with
// count=5 — at index 3, s1 reads NUL (0x00) and s2 reads 'X'
// (0x58); the documented difference is `(0x00 - 0x58) = -88`
// (negative — s1 is "less than" s2).  Defensively, just assert
// the sign.
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_one_side_nul_picks_up_sign() {
    let r = call_strnicmp_raw(b"AVI\0", b"AVIX", 5);
    let signed = r as i32;
    assert!(
        signed < 0,
        "expected negative for `AVI\\0` < `AVIX`; got {signed}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 12 — round-trip the two FOURCC casings the splitter
// actually compares (per the docs/winmf/ inventory): `"riff"` vs
// `"RIFF"` with count=4 returns 0 (this is the canonical use site
// driving the round-49 implementation shape).
// ────────────────────────────────────────────────────────────────

#[test]
fn strnicmp_riff_fourcc_compare_returns_zero() {
    let r = call_strnicmp(b"riff", b"RIFF", 4);
    assert_eq!(r, 0, "RIFF FOURCC casing compare must be equal; got {r:#x}");
}

// ────────────────────────────────────────────────────────────────
// Test 13 — the round-49 headline: `Sandbox::load("msadds32.ax")`
// advances past `_strnicmp`.  Either the load completes (all
// imports resolved) or it stops at the next unresolved import; we
// report both outcomes informationally and pin the failure case so
// any silent forward progress in a sibling round shows up here.
//
// Skipped gracefully if the DLL is not present in the docs tree.
// ────────────────────────────────────────────────────────────────

#[test]
fn msadds32_ax_pe_load_advances_past_strnicmp() {
    let Some(p) = msadds32_path() else {
        eprintln!("round49: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("msadds32.ax", &bytes) {
        Ok(img) => {
            // Desired terminal state: full splitter PE-load.
            eprintln!(
                "round49: msadds32.ax FULLY PE-loaded — image_base={:#010x}, \
                 entry_point={:#010x}, DllMain={:?}, DllGetClassObject={:?}",
                img.image_base,
                img.entry_point,
                img.export("DllMain"),
                img.export("DllGetClassObject"),
            );
        }
        Err(e) => {
            // Pin: the unresolved-import name in the error must
            // not contain `_strnicmp` any more.
            let msg = format!("{e}");
            assert!(
                !msg.contains("\"_strnicmp\"") && !msg.contains("!_strnicmp"),
                "round 49 expected msadds32.ax PE-load to advance PAST _strnicmp; \
                 got: {msg}"
            );
            eprintln!(
                "round49: msadds32.ax PE-load advanced past _strnicmp; \
                 next blocker (if any) is reported in the error: {msg}"
            );
        }
    }
}
