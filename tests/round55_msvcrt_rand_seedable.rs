//! Round 55 — `msvcrt!rand` + `msvcrt!srand` with seedable
//! `Sandbox`-level PRNG state.
//!
//! ## Background
//!
//! Round 52 wired the real `msvcrt!_ftol` impl and pinned the next
//! `msadds32.ax` PE-load blocker as `msvcrt!rand`.  Round 55 wires
//! `rand` plus the seed companion `srand`, and adds a host-side
//! seedable `Sandbox` API so callers can drive the codec with a
//! deterministic PRNG sequence.
//!
//! ## Stub contract — `int __cdecl rand(void)`
//!
//! Per MSDN `rand`: returns a pseudorandom integer in `[0,
//! RAND_MAX]` where `RAND_MAX = 0x7FFF` (32767).  MSVC implements
//! `rand` as a linear-congruential generator (LCG) with the
//! standard Knuth-style parameters; the multiplier (`214013`),
//! increment (`2531011`), modulus (`2^32`), and output-bit mask
//! (`(state >> 16) & 0x7FFF`) are public number-theory constants
//! found in many textbook LCG tables — no Microsoft CRT source
//! was consulted.
//!
//! ```text
//! state = state * 214013 + 2531011   (mod 2^32)
//! rand  = (state >> 16) & 0x7FFF
//! ```
//!
//! ## Stub contract — `void __cdecl srand(unsigned int seed)`
//!
//! Per MSDN `srand`: sets the LCG state used by subsequent `rand`
//! calls.  MSVC stores `seed` directly into the state field (no
//! XOR / no scrambling) — a public, observable convention.
//!
//! ## Sandbox-level seed API
//!
//! `Sandbox::with_rand_seed(seed) -> Self` (builder),
//! `Sandbox::set_rand_seed(&mut self, seed)` (setter),
//! `Sandbox::rand_seed(&self) -> u32` (reader).  All three read /
//! write the same `HostState::rand_state` field that
//! `msvcrt!rand` / `srand` use, so:
//!
//!  * Host-seeded sandboxes produce reproducible `rand` sequences.
//!  * A guest `srand(s)` call overrides the host-staged seed and
//!    becomes observable via `rand_seed()`.
//!
//! ## References (clean-room, on-disk)
//!
//! * MSDN `rand`:
//!   <https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/rand>
//! * MSDN `srand`:
//!   <https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/srand>
//! * Knuth, *The Art of Computer Programming*, Vol. 2 — public
//!   LCG parameter tables (no Microsoft CRT source consulted).

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

/// Dispatch `msvcrt!rand` once on the supplied sandbox and return
/// the dword left in `eax` (low 15 bits == the rand value;
/// upper bits == 0).
fn call_rand(sb: &mut Sandbox) -> u32 {
    let thunk = sb
        .registry
        .resolve("msvcrt.dll", "rand")
        .expect("rand registered");
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    sb.cpu.regs.get32(Reg32::Eax)
}

/// Dispatch `msvcrt!srand(seed)` on the supplied sandbox.  cdecl
/// caller-cleanup, one dword on the stack — push the arg then the
/// synthetic ret-sentinel.
fn call_srand(sb: &mut Sandbox, seed: u32) {
    let thunk = sb
        .registry
        .resolve("msvcrt.dll", "srand")
        .expect("srand registered");
    sb.cpu.push32(&mut sb.mmu, seed).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
}

// ────────────────────────────────────────────────────────────────
// Test 1 — both stubs are wired into the msvcrt registry.
// ────────────────────────────────────────────────────────────────

#[test]
fn rand_and_srand_are_registered_in_msvcrt() {
    let mut r = Registry::new();
    oxideav_vfw::win32::msvcrt::register(&mut r);
    assert!(
        r.resolve("msvcrt.dll", "rand").is_some(),
        "msvcrt!rand stub missing — round 55 follow-up"
    );
    assert!(
        r.resolve("msvcrt.dll", "srand").is_some(),
        "msvcrt!srand stub missing — round 55 follow-up"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 2 — default-seed sequence is reproducible across freshly-
// constructed sandboxes (MSVC's documented "no `srand` called yet"
// initial state == 1).
// ────────────────────────────────────────────────────────────────

#[test]
fn default_seed_sequence_is_reproducible() {
    let mut a = Sandbox::new();
    let mut b = Sandbox::new();
    let seq_a: Vec<u32> = (0..10).map(|_| call_rand(&mut a)).collect();
    let seq_b: Vec<u32> = (0..10).map(|_| call_rand(&mut b)).collect();
    assert_eq!(
        seq_a, seq_b,
        "default-seed (1) sandboxes must produce identical rand sequences"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 3 — same-seed sequences match.
// ────────────────────────────────────────────────────────────────

#[test]
fn same_seed_sequences_match() {
    let mut a = Sandbox::new().with_rand_seed(42);
    let mut b = Sandbox::new().with_rand_seed(42);
    let seq_a: Vec<u32> = (0..10).map(|_| call_rand(&mut a)).collect();
    let seq_b: Vec<u32> = (0..10).map(|_| call_rand(&mut b)).collect();
    assert_eq!(
        seq_a, seq_b,
        "with_rand_seed(42) sandboxes must produce identical sequences"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 4 — different-seed sequences differ.
// ────────────────────────────────────────────────────────────────

#[test]
fn different_seed_sequences_differ() {
    let mut a = Sandbox::new().with_rand_seed(42);
    let mut b = Sandbox::new().with_rand_seed(43);
    let seq_a: Vec<u32> = (0..10).map(|_| call_rand(&mut a)).collect();
    let seq_b: Vec<u32> = (0..10).map(|_| call_rand(&mut b)).collect();
    assert_ne!(
        seq_a, seq_b,
        "seeds 42 and 43 must produce different rand sequences"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 5 — output bound: all values fit in [0, RAND_MAX = 0x7FFF].
// ────────────────────────────────────────────────────────────────

#[test]
fn rand_output_is_bounded_by_rand_max() {
    let mut sb = Sandbox::new().with_rand_seed(0xDEAD_BEEF);
    for _ in 0..256 {
        let v = call_rand(&mut sb);
        assert!(
            v <= 0x7FFF,
            "rand() returned {v:#x}, exceeds RAND_MAX = 0x7FFF"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Test 6 — entropy: 1000 consecutive values from a single seed
// occupy a reasonable spread of the 15-bit output space.  We bin
// the values into 16 buckets and assert every bucket received some
// hits — an 8-cycle LCG (or one with massive period collapse)
// would land all hits in a small handful of buckets.
// ────────────────────────────────────────────────────────────────

#[test]
fn rand_output_covers_15_bit_space_under_uniform_sampling() {
    let mut sb = Sandbox::new().with_rand_seed(1);
    let mut buckets = [0u32; 16];
    for _ in 0..1000 {
        let v = call_rand(&mut sb);
        // Buckets are 0x1000 wide across the 0..=0x7FFF output
        // range, then truncated to 16 buckets.
        let b = ((v >> 11) & 0xF) as usize;
        buckets[b] += 1;
    }
    // Every bucket must have received ≥ 10 hits (uniform expects
    // 1000/16 = 62.5; 10 is a 6× safety floor that still catches a
    // genuinely degenerate LCG).
    for (i, &n) in buckets.iter().enumerate() {
        assert!(
            n >= 10,
            "bucket {i} got only {n} hits across 1000 samples; \
             LCG output may be degenerate"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Test 7 — `srand` from guest code overrides the host-staged seed.
// ────────────────────────────────────────────────────────────────

#[test]
fn guest_srand_overrides_host_set_rand_seed() {
    let mut sb = Sandbox::new();
    sb.set_rand_seed(1);
    assert_eq!(sb.rand_seed(), 1);
    call_srand(&mut sb, 99);
    assert_eq!(
        sb.rand_seed(),
        99,
        "guest srand(99) must overwrite the host-staged seed"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 8 — sequence is `srand`-equivalent: host seed-state X is
// observationally indistinguishable from a fresh sandbox that
// guest-called `srand(X)`.
// ────────────────────────────────────────────────────────────────

#[test]
fn host_set_rand_seed_matches_guest_srand_sequence() {
    let mut host_seeded = Sandbox::new().with_rand_seed(0x1234_5678);
    let mut guest_seeded = Sandbox::new();
    call_srand(&mut guest_seeded, 0x1234_5678);
    let a: Vec<u32> = (0..16).map(|_| call_rand(&mut host_seeded)).collect();
    let b: Vec<u32> = (0..16).map(|_| call_rand(&mut guest_seeded)).collect();
    assert_eq!(
        a, b,
        "host-seeded and guest-srand-seeded sandboxes must produce identical sequences"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 9 — known-vector check.  Compute the expected first three
// outputs from the documented LCG by hand and assert the stub
// emits them.  `state = 1` is the default; per the LCG step
//   s1 = 1 * 214013 + 2531011 = 2_745_024.
//   r1 = (2_745_024 >> 16) & 0x7FFF = 41 & 0x7FFF = 41.
//   s2 = 2_745_024 * 214013 + 2531011 = 587_604_336_643 mod 2^32
//      = 587_604_336_643 - 136 * 2^32 = 587_604_336_643 - 584_115_552_256
//      = 3_488_784_387.
//   r2 = (3_488_784_387 >> 16) & 0x7FFF = 53231 & 0x7FFF = 20463.
// We compute these the same way in-test rather than baking literals
// the comment may misalign.
// ────────────────────────────────────────────────────────────────

#[test]
fn rand_known_vectors_from_default_seed() {
    fn expected_step(state: u32) -> (u32, u32) {
        let s = state.wrapping_mul(214013).wrapping_add(2531011);
        let r = (s >> 16) & 0x7FFF;
        (s, r)
    }

    let mut sb = Sandbox::new(); // default seed = 1
    let mut model_state: u32 = 1;
    for i in 0..16 {
        let (next_state, expected_r) = expected_step(model_state);
        model_state = next_state;
        let actual_r = call_rand(&mut sb);
        assert_eq!(
            actual_r, expected_r,
            "step {i}: stub-emitted {actual_r:#x} differs from LCG model {expected_r:#x}"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Test 10 — `rand_seed()` reflects the post-call LCG state, so the
// host can read back what the sandbox's PRNG ended up at.
// ────────────────────────────────────────────────────────────────

#[test]
fn rand_seed_reads_back_post_call_state() {
    let mut sb = Sandbox::new().with_rand_seed(7);
    assert_eq!(sb.rand_seed(), 7);
    let _ = call_rand(&mut sb);
    let after_one = sb.rand_seed();
    assert_ne!(
        after_one, 7,
        "rand_seed must advance after a guest rand() call"
    );
    // Expected: s = 7 * 214013 + 2531011 = 4_029_102 (mod 2^32).
    let expected: u32 = 7u32.wrapping_mul(214013).wrapping_add(2531011);
    assert_eq!(after_one, expected);
}

// ────────────────────────────────────────────────────────────────
// Test 11 — `set_rand_seed` mid-flight resets the sequence.
// ────────────────────────────────────────────────────────────────

#[test]
fn set_rand_seed_resets_sequence_mid_flight() {
    let mut sb = Sandbox::new().with_rand_seed(100);
    let first_three: Vec<u32> = (0..3).map(|_| call_rand(&mut sb)).collect();
    sb.set_rand_seed(100);
    let again_three: Vec<u32> = (0..3).map(|_| call_rand(&mut sb)).collect();
    assert_eq!(
        first_three, again_three,
        "set_rand_seed(100) mid-flight must reset to the same sequence"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 12 — round-55 headline: `Sandbox::load("msadds32.ax")`
// advances past `rand` (and past `srand` if it surfaces) and pins
// the next blocker. Either the load completes (all imports
// resolved by r55) or it stops at the next unresolved import; we
// report both outcomes informationally and pin the failure case
// so any silent forward progress in a sibling round shows up here.
//
// Skipped gracefully if the DLL is not present in the docs tree.
// ────────────────────────────────────────────────────────────────

#[test]
fn msadds32_ax_pe_load_advances_past_rand() {
    let Some(p) = msadds32_path() else {
        eprintln!("round55: msadds32.ax missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    match sb.load("msadds32.ax", &bytes) {
        Ok(img) => {
            eprintln!(
                "round55: msadds32.ax FULLY PE-loaded — image_base={:#010x}, \
                 entry_point={:#010x}, DllMain={:?}, DllGetClassObject={:?}",
                img.image_base,
                img.entry_point,
                img.export("DllMain"),
                img.export("DllGetClassObject"),
            );
        }
        Err(e) => {
            // Pin: the unresolved-import name in the error must
            // not contain `rand` or `srand` any more (and must not
            // be quoted as `"rand"` / `"srand"` — that's the form
            // the loader emits).
            let msg = format!("{e}");
            assert!(
                !msg.contains("\"rand\"") && !msg.contains("!rand"),
                "round 55 expected msadds32.ax PE-load to advance PAST rand; \
                 got: {msg}"
            );
            assert!(
                !msg.contains("\"srand\"") && !msg.contains("!srand"),
                "round 55 expected msadds32.ax PE-load to advance PAST srand; \
                 got: {msg}"
            );
            eprintln!(
                "round55: msadds32.ax PE-load advanced past rand/srand; \
                 next blocker (if any) is reported in the error: {msg}"
            );
        }
    }
}
