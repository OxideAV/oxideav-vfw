//! Round 56 — `msvcrt!_CIpow` real impl + `msadds32.ax` PE-load
//! surface advance.
//!
//! ## Background
//!
//! Round 55 wired `msvcrt!{rand, srand}` and pinned the next
//! `msadds32.ax` PE-load blocker as `msvcrt!_CIpow` — the MSVC
//! compiler-intrinsic `pow(double, double)` helper.
//!
//! The `_CI*` prefix is MSVC's "compiler intrinsic" convention:
//! args are passed on the **x87 stack** (not the cdecl integer
//! stack), and the result is returned on the x87 stack as the new
//! `ST(0)`.  The same calling-convention quirk applies as `_ftol`
//! (round 52): `arg_dwords = 0` because no dwords are on the
//! regular cdecl stack to be cleaned up.
//!
//! ## Stub contract
//!
//! `double __cdecl _CIpow(double base, double exp)`:
//!
//!  1. Pop `ST(0)` (exponent).
//!  2. Pop `ST(0)` (base; was `ST(1)` pre-pop).
//!  3. Compute `base.powf(exp)` per IEEE 754 — Rust's `f64::powf`
//!     is bit-correct by construction.
//!  4. Push result back onto x87 stack as new `ST(0)`.
//!  5. Return 0 in `eax`.
//!
//! IEEE 754 corner cases verified:
//!
//!  * `_CIpow(0.0, 0.0)              → 1.0`
//!  * `_CIpow(NaN, anything)         → NaN`  (except `.powf(0.0) = 1.0`)
//!  * `_CIpow(1.0, NaN)              → 1.0`
//!  * `_CIpow(∞, 0.0)                → 1.0`
//!  * `_CIpow(-2.0, 0.5)             → NaN`
//!
//! ## References (clean-room, on-disk)
//!
//! * MSDN `pow`:
//!   <https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/pow-powf-powl>
//! * Intel SDM Vol. 1 §8 + Vol. 2A "FLD" / "FSTP" — x87 stack
//!   semantics.
//! * IEEE 754-2008 — `pow` corner cases.

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

/// Push `base` then `exp` onto the sandbox's x87 stack (matching
/// the caller's `FLD base; FLD exp` emission so ST(0)=exp,
/// ST(1)=base), dispatch `_CIpow`, then return (result-as-ST(0),
/// post-call x87 stack depth, eax).
fn call_ci_pow(base: f64, exp: f64) -> (f64, u8, u32) {
    let mut sb = Sandbox::new();
    sb.cpu.fpu.push(base);
    sb.cpu.fpu.push(exp);
    let depth_before = depth(&sb);
    let thunk = sb
        .registry
        .resolve("msvcrt.dll", "_CIpow")
        .expect("_CIpow registered");
    // cdecl: no args on the regular stack — just the synthetic
    // ret-sentinel.
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    let eax = sb.cpu.regs.get32(Reg32::Eax);
    let result = sb.cpu.fpu.st(0);
    let depth_after = depth(&sb);
    // _CIpow consumes 2 x87 slots and pushes 1 → net -1.
    assert_eq!(
        depth_after + 1,
        depth_before,
        "_CIpow must consume 2 x87 slots and push 1 (net -1 depth)"
    );
    (result, depth_after, eax)
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
fn cipow_is_registered_in_msvcrt() {
    let mut r = Registry::new();
    oxideav_vfw::win32::msvcrt::register(&mut r);
    assert!(
        r.resolve("msvcrt.dll", "_CIpow").is_some(),
        "msvcrt!_CIpow stub missing — round-56 follow-up"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — canonical: 2.0 ** 10.0 == 1024.0.
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_canonical_2_to_10_is_1024() {
    let (r, _depth, _eax) = call_ci_pow(2.0, 10.0);
    assert_eq!(r, 1024.0, "_CIpow(2.0, 10.0) must equal 1024.0");
}

// ────────────────────────────────────────────────────────────────
// Test 3 — fractional exponent: 2.0 ** 0.5 ≈ sqrt(2).
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_fractional_exponent_is_sqrt() {
    let (r, _, _) = call_ci_pow(2.0, 0.5);
    let expected = std::f64::consts::SQRT_2;
    assert!(
        (r - expected).abs() < 1e-10,
        "_CIpow(2.0, 0.5) = {r}, want ≈ {expected}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 4 — negative base, integer exponent: (-3.0) ** 2.0 == 9.0.
// IEEE 754: real result for integer exponents.
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_negative_base_integer_exp_is_real() {
    let (r, _, _) = call_ci_pow(-3.0, 2.0);
    assert_eq!(r, 9.0, "_CIpow(-3.0, 2.0) must equal 9.0");
}

// ────────────────────────────────────────────────────────────────
// Test 5 — negative base, non-integer exponent: (-2.0) ** 0.5
// produces NaN (real result is imaginary).
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_negative_base_non_integer_exp_is_nan() {
    let (r, _, _) = call_ci_pow(-2.0, 0.5);
    assert!(
        r.is_nan(),
        "_CIpow(-2.0, 0.5) must be NaN (imaginary real result), got {r}"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 6 — zero base, positive exponent: 0.0 ** 3.0 == 0.0.
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_zero_base_positive_exp_is_zero() {
    let (r, _, _) = call_ci_pow(0.0, 3.0);
    assert_eq!(r, 0.0, "_CIpow(0.0, 3.0) must equal 0.0");
}

// ────────────────────────────────────────────────────────────────
// Test 7 — IEEE 754 default: 0.0 ** 0.0 == 1.0.
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_zero_to_zero_is_one_per_ieee754() {
    let (r, _, _) = call_ci_pow(0.0, 0.0);
    assert_eq!(
        r, 1.0,
        "_CIpow(0.0, 0.0) must equal 1.0 per IEEE 754 default"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 8 — NaN propagation: NaN ** 2.0 = NaN.
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_nan_base_propagates() {
    let (r, _, _) = call_ci_pow(f64::NAN, 2.0);
    assert!(r.is_nan(), "_CIpow(NaN, 2.0) must be NaN, got {r}");
}

// ────────────────────────────────────────────────────────────────
// Test 9 — NaN ** 0.0 == 1.0 (IEEE 754 exception to NaN propagation).
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_nan_to_zero_is_one_per_ieee754() {
    let (r, _, _) = call_ci_pow(f64::NAN, 0.0);
    assert_eq!(
        r, 1.0,
        "_CIpow(NaN, 0.0) must equal 1.0 per IEEE 754 (powf-of-NaN exception)"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 10 — +∞ ** 0.0 == 1.0.
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_infinity_to_zero_is_one() {
    let (r, _, _) = call_ci_pow(f64::INFINITY, 0.0);
    assert_eq!(r, 1.0, "_CIpow(∞, 0.0) must equal 1.0 per IEEE 754 default");
}

// ────────────────────────────────────────────────────────────────
// Test 11 — 1.0 ** NaN == 1.0 (the other powf-of-NaN exception).
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_one_to_nan_is_one_per_ieee754() {
    let (r, _, _) = call_ci_pow(1.0, f64::NAN);
    assert_eq!(
        r, 1.0,
        "_CIpow(1.0, NaN) must equal 1.0 per IEEE 754 (powf-of-NaN exception)"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 12 — x87 stack invariant: 2 in, 1 out (net -1 depth).
// ────────────────────────────────────────────────────────────────

#[test]
fn cipow_pops_two_pushes_one_on_x87_stack() {
    let (_r, depth_after, _eax) = call_ci_pow(2.0, 3.0);
    assert_eq!(
        depth_after, 1,
        "post-call x87 depth must be 1 (pushed result, no other slots used)"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 13 — round-56 headline: `Sandbox::load("msadds32.ax")`
// advances past `_CIpow`.  Either the load completes (all imports
// resolved by r56) or it stops at the next unresolved import; we
// report both outcomes informationally and pin the failure case so
// any silent forward progress in a sibling round shows up here.
//
// Skipped gracefully if the DLL is not present in the docs tree.
// ────────────────────────────────────────────────────────────────

#[test]
fn msadds32_ax_pe_load_advances_past_cipow() {
    let Some(p) = msadds32_path() else {
        eprintln!("round56: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("msadds32.ax", &bytes) {
        Ok(img) => {
            eprintln!(
                "round56: msadds32.ax FULLY PE-loaded — image_base={:#010x}, \
                 entry_point={:#010x}, DllMain={:?}, DllGetClassObject={:?}",
                img.image_base,
                img.entry_point,
                img.export("DllMain"),
                img.export("DllGetClassObject"),
            );
        }
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                !msg.contains("\"_CIpow\"") && !msg.contains("!_CIpow"),
                "round 56 expected msadds32.ax PE-load to advance PAST _CIpow; \
                 got: {msg}"
            );
            eprintln!(
                "round56: msadds32.ax PE-load advanced past _CIpow; \
                 next blocker (if any) is reported in the error: {msg}"
            );
        }
    }
}
