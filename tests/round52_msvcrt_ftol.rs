//! Round 52 — `msvcrt!_ftol` real impl + `msadds32.ax` PE-load
//! surface advance.
//!
//! ## Background
//!
//! Round 50 added `msvcrt!_beginthreadex` as a fail-soft no-op stub
//! (returning 0 = MSDN "thread creation failed" sentinel) and
//! pushed `Sandbox::load("msadds32.ax")` past the splitter's CRT
//! thread-creation edge.  The next unresolved import the splitter
//! pulls is `msvcrt!_ftol` — the MSVC x87-to-i32 truncate helper
//! used by code compiled without `/QIfist`.
//!
//! Unlike the r48/r50 fail-soft pair (`_endthreadex` /
//! `_beginthreadex`, both never actually invoked on the decode path
//! we drive), `_ftol` IS called from filter-coefficient init paths.
//! Returning a constant 0 (or a wrong-sign truncation) would
//! scramble every conversion of a precomputed float coefficient
//! back to the i32 the splitter's FIR loops expect.  Round 52 wires
//! the real impl.
//!
//! ## Stub semantics
//!
//! `long __cdecl _ftol(double)` — MSDN MSVC CRT.
//!
//! Per the MSVC ABI the `double` argument is passed on the x87
//! stack: the caller emits `FLD qword ptr [arg]` immediately before
//! the CALL, leaving the value as `ST(0)`.  `_ftol` then:
//!
//!  1. Reads `ST(0)`.
//!  2. Truncates toward zero (i.e. `f64 as i32` semantics in Rust,
//!     NOT `floor`).
//!  3. Pops `ST(0)` off the x87 stack.
//!  4. Returns the i32 in `eax`.
//!
//! Saturation contract (pinned by these tests):
//!
//!  * `f.is_nan()`            → `i32::MIN` (`0x8000_0000`).
//!  * `f >= 2_147_483_648.0`  → `i32::MAX` (`0x7FFF_FFFF`).
//!  * `f <= -2_147_483_649.0` → `i32::MIN` (`0x8000_0000`).
//!  * Otherwise               → `f as i32` (truncation toward zero).
//!
//! cdecl from the C source's perspective; caller-cleanup on the
//! regular cdecl stack, but the *argument* is on the x87 stack and
//! not on the regular stack at all → `arg_dwords = 0`.
//!
//! ## References (clean-room, on-disk)
//!
//! * `docs/winmf/winmf-emulator.md` — splitter import-walk
//!   inventory; `msvcrt!_ftol` is the post-r50 edge symbol.
//! * MSDN `_ftol`:
//!   <https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/ftol>
//! * Intel SDM Vol. 2A — `FLD` / `FSTP` (x87 stack semantics).
//!
//! ## What we deliberately do NOT do
//!
//! Drive `msadds32.ax` through `DLL_PROCESS_ATTACH` /
//! `DriverProc`.  Per the round-24 / r45..r51 follow-up scope we
//! just "wire the stub, don't drive msadds32".  Future rounds that
//! decide to exercise the splitter's window-pump path will need to
//! extend more stubs as the next blocker surfaces.

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

/// Push `v` onto the sandbox's x87 stack as `ST(0)`, then dispatch
/// `_ftol`, then return the dword left in `eax` along with the
/// post-call x87 stack depth so the tests can pin both halves of
/// the contract.
fn call_ftol(v: f64) -> (u32, u8) {
    let mut sb = Sandbox::new();
    // Emulate the caller's `FLD qword ptr [arg]` by pushing the
    // value directly onto the x87 stack.
    sb.cpu.fpu.push(v);
    let depth_before = depth(&sb);
    let thunk = sb
        .registry
        .resolve("msvcrt.dll", "_ftol")
        .expect("_ftol registered");
    // cdecl: no args on the regular stack.  Just the synthetic
    // return-address sentinel.
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    let eax = sb.cpu.regs.get32(Reg32::Eax);
    let depth_after = depth(&sb);
    assert_eq!(
        depth_after + 1,
        depth_before,
        "_ftol must pop exactly one x87 slot"
    );
    (eax, depth_after)
}

/// Count occupied (valid-tag) entries in the FPU stack.
fn depth(sb: &Sandbox) -> u8 {
    let mut n = 0u8;
    for v in &sb.cpu.fpu.tag_valid {
        if *v {
            n += 1;
        }
    }
    n
}

// ────────────────────────────────────────────────────────────────
// Test 1 — the stub is wired into the msvcrt registry.
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_is_registered_in_msvcrt() {
    let mut r = Registry::new();
    oxideav_vfw::win32::msvcrt::register(&mut r);
    assert!(
        r.resolve("msvcrt.dll", "_ftol").is_some(),
        "msvcrt!_ftol stub missing — round-52 follow-up"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — positive fractional: 3.7 → 3 (truncate toward zero,
// NOT floor).
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_positive_fraction_truncates_toward_zero() {
    let (eax, _) = call_ftol(3.7);
    assert_eq!(eax as i32, 3, "_ftol(3.7) must truncate to 3, not floor");
}

// ────────────────────────────────────────────────────────────────
// Test 3 — negative fractional: -3.7 → -3 (truncate toward zero,
// NOT floor; ceil-style for negatives).
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_negative_fraction_truncates_toward_zero() {
    let (eax, _) = call_ftol(-3.7);
    assert_eq!(
        eax as i32, -3,
        "_ftol(-3.7) must truncate to -3 (toward zero), not floor to -4"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 4 — exactly zero.
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_zero_returns_zero() {
    let (eax, _) = call_ftol(0.0);
    assert_eq!(eax, 0);
}

// ────────────────────────────────────────────────────────────────
// Test 5 — +∞ saturates to i32::MAX.
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_positive_infinity_saturates_to_i32_max() {
    let (eax, _) = call_ftol(f64::INFINITY);
    assert_eq!(eax as i32, i32::MAX);
}

// ────────────────────────────────────────────────────────────────
// Test 6 — -∞ saturates to i32::MIN.
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_negative_infinity_saturates_to_i32_min() {
    let (eax, _) = call_ftol(f64::NEG_INFINITY);
    assert_eq!(eax as i32, i32::MIN);
}

// ────────────────────────────────────────────────────────────────
// Test 7 — NaN returns the i32::MIN sentinel (MSVC "indefinite
// integer").
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_nan_returns_i32_min_sentinel() {
    let (eax, _) = call_ftol(f64::NAN);
    assert_eq!(
        eax as i32,
        i32::MIN,
        "_ftol(NaN) must return the i32::MIN sentinel (0x80000000)"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 8 — large positive saturates to i32::MAX.
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_large_positive_saturates_to_i32_max() {
    let (eax, _) = call_ftol(1.0e20);
    assert_eq!(eax as i32, i32::MAX);
}

// ────────────────────────────────────────────────────────────────
// Test 9 — large negative saturates to i32::MIN.
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_large_negative_saturates_to_i32_min() {
    let (eax, _) = call_ftol(-1.0e20);
    assert_eq!(eax as i32, i32::MIN);
}

// ────────────────────────────────────────────────────────────────
// Test 10 — boundary: exactly i32::MAX as f64 maps to i32::MAX
// (note: f64 representation of 2_147_483_648.0 is *one above* MAX,
// which triggers the saturation envelope).
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_boundary_at_i32_max_is_handled() {
    // 2_147_483_647.0 is exactly i32::MAX as f64.
    let (eax, _) = call_ftol(i32::MAX as f64);
    assert_eq!(eax as i32, i32::MAX);
}

// ────────────────────────────────────────────────────────────────
// Test 11 — exact integer truncates cleanly.
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_exact_integer_passes_through() {
    let (eax, _) = call_ftol(42.0);
    assert_eq!(eax as i32, 42);
    let (eax2, _) = call_ftol(-42.0);
    assert_eq!(eax2 as i32, -42);
}

// ────────────────────────────────────────────────────────────────
// Test 12 — sub-1 fractions: 0.5 → 0 (toward zero), -0.5 → 0.
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_sub_unit_fractions_round_toward_zero() {
    let (a, _) = call_ftol(0.5);
    assert_eq!(a as i32, 0);
    let (b, _) = call_ftol(-0.5);
    assert_eq!(b as i32, 0);
    let (c, _) = call_ftol(0.999);
    assert_eq!(c as i32, 0);
    let (d, _) = call_ftol(-0.999);
    assert_eq!(d as i32, 0);
}

// ────────────────────────────────────────────────────────────────
// Test 13 — x87 stack depth decreases by 1 after the call.
// ────────────────────────────────────────────────────────────────

#[test]
fn ftol_pops_st0_off_the_x87_stack() {
    let (_eax, depth_after) = call_ftol(123.456);
    assert_eq!(
        depth_after, 0,
        "_ftol must pop the single FLD'd slot leaving depth 0"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 14 — the round-52 headline: `Sandbox::load("msadds32.ax")`
// advances past `_ftol`.  Either the load completes (all imports
// resolved by r52) or it stops at the next unresolved import; we
// report both outcomes informationally and pin the failure case so
// any silent forward progress in a sibling round shows up here.
//
// Skipped gracefully if the DLL is not present in the docs tree.
// ────────────────────────────────────────────────────────────────

#[test]
fn msadds32_ax_pe_load_advances_past_ftol() {
    let Some(p) = msadds32_path() else {
        eprintln!("round52: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("msadds32.ax", &bytes) {
        Ok(img) => {
            eprintln!(
                "round52: msadds32.ax FULLY PE-loaded — image_base={:#010x}, \
                 entry_point={:#010x}, DllMain={:?}, DllGetClassObject={:?}",
                img.image_base,
                img.entry_point,
                img.export("DllMain"),
                img.export("DllGetClassObject"),
            );
        }
        Err(e) => {
            // Pin: the unresolved-import name in the error must
            // not contain `_ftol` any more.
            let msg = format!("{e}");
            assert!(
                !msg.contains("\"_ftol\"") && !msg.contains("!_ftol"),
                "round 52 expected msadds32.ax PE-load to advance PAST _ftol; \
                 got: {msg}"
            );
            eprintln!(
                "round52: msadds32.ax PE-load advanced past _ftol; \
                 next blocker (if any) is reported in the error: {msg}"
            );
        }
    }
}
