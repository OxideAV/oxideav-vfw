//! Round 258 — encoder knob-key vocabulary constants +
//! unrecognized-key advisory helper.
//!
//! Round 257 gave callers the *positive* query view of the
//! encoder's three `CodecParameters.options` bridge knobs
//! (`resolve_encoder_knobs` → "what will the encoder see?").
//! Round 258 adds the *negative* view: named constants for the
//! key spellings (`ENCODER_KNOB_QUALITY` / `ENCODER_KNOB_KEYINT` /
//! `ENCODER_KNOB_DATA_RATE`, collected in `ENCODER_KNOB_KEYS`) and
//! `unrecognized_encoder_knobs(&CodecParameters) -> Vec<&str>`,
//! which reports the option keys the bridge will silently ignore.
//! Under the best-effort policy a typo'd knob name produces no
//! error and no effect — the advisory helper is what lets a CLI /
//! pipeline pre-validator warn the user before encode time.
//!
//! These tests exercise the public surface end-to-end through the
//! `oxideav_vfw::discovery` re-exports, from a downstream-consumer
//! perspective.

#![cfg(feature = "auto-discovery")]

use oxideav_core::{CodecId, CodecParameters};
use oxideav_vfw::discovery::{
    resolve_encoder_knobs, unrecognized_encoder_knobs, ENCODER_KNOB_DATA_RATE, ENCODER_KNOB_KEYINT,
    ENCODER_KNOB_KEYS, ENCODER_KNOB_QUALITY,
};

/// The constants spell the exact key strings callers have stored
/// in their options bags since round 112 (`quality` / `keyint`)
/// and round 178 (`data_rate`). A rename would silently strand
/// every existing caller's knobs, so the spellings are pinned at
/// the public re-export level too.
#[test]
fn knob_key_constants_reachable_and_pinned_via_reexports() {
    assert_eq!(ENCODER_KNOB_QUALITY, "quality");
    assert_eq!(ENCODER_KNOB_KEYINT, "keyint");
    assert_eq!(ENCODER_KNOB_DATA_RATE, "data_rate");
    assert_eq!(
        ENCODER_KNOB_KEYS,
        [
            ENCODER_KNOB_QUALITY,
            ENCODER_KNOB_KEYINT,
            ENCODER_KNOB_DATA_RATE
        ]
    );
}

/// The canonical pre-validator flow the round-258 surface exists
/// for: a CLI maps user flags into the options bag via the named
/// constants, asks the resolver what the encoder will see, and
/// asks the advisory helper whether anything in the bag will be
/// ignored. A clean bag warns about nothing and resolves every
/// value.
#[test]
fn pre_validator_flow_clean_bag() {
    let mut params = CodecParameters::video(CodecId::new("vfw_round258_clean"));
    params.options.insert(ENCODER_KNOB_QUALITY, "8000");
    params.options.insert(ENCODER_KNOB_KEYINT, "30");
    params.options.insert(ENCODER_KNOB_DATA_RATE, "1400");

    assert!(unrecognized_encoder_knobs(&params).is_empty());
    let knobs = resolve_encoder_knobs(&params);
    assert_eq!(knobs.quality, 8000);
    assert_eq!(knobs.keyint, 30);
    assert_eq!(knobs.data_rate, 1400);
}

/// The motivating failure mode: a typo'd key vanishes under the
/// best-effort policy — the resolver falls back to the default
/// with no error — but the advisory helper surfaces it so the
/// caller can warn instead of shipping a silently-unconfigured
/// encode.
#[test]
fn typo_key_is_reported_and_resolver_ignores_it() {
    let mut params = CodecParameters::video(CodecId::new("vfw_round258_typo"));
    params.options.insert("qality", "9000"); // typo'd "quality"
    params.options.insert(ENCODER_KNOB_KEYINT, "12");

    assert_eq!(unrecognized_encoder_knobs(&params), vec!["qality"]);
    let knobs = resolve_encoder_knobs(&params);
    assert_eq!(knobs.quality, 0); // the typo'd value never landed
    assert_eq!(knobs.keyint, 12); // the well-formed knob still did
}

/// Matching is exact and case-sensitive, mirroring the resolver's
/// `options.get(key)` lookups — `"Quality"` will never be read, so
/// it must be reported.
#[test]
fn case_variant_keys_are_unrecognized() {
    let mut params = CodecParameters::video(CodecId::new("vfw_round258_case"));
    params.options.insert("Quality", "8000");
    params.options.insert("KEYINT", "30");
    assert_eq!(
        unrecognized_encoder_knobs(&params),
        vec!["Quality", "KEYINT"]
    );
}

/// The report preserves the bag's insertion order (the order
/// `CodecOptions::iter` walks), so a CLI can print it verbatim
/// with deterministic output.
#[test]
fn report_preserves_insertion_order() {
    let mut params = CodecParameters::video(CodecId::new("vfw_round258_order"));
    params.options.insert("zeta", "1");
    params.options.insert(ENCODER_KNOB_DATA_RATE, "1400");
    params.options.insert("alpha", "2");
    params.options.insert("mu", "3");
    assert_eq!(
        unrecognized_encoder_knobs(&params),
        vec!["zeta", "alpha", "mu"]
    );
}

/// Key-level verdict only: a recognized key carrying a malformed
/// value is read (and falls back) — it is NOT "unrecognized".
/// Value diagnostics are a separate concern from vocabulary
/// membership; conflating them would make the helper double-report
/// what the resolver's fallback already handles.
#[test]
fn malformed_value_on_known_key_not_reported() {
    let mut params = CodecParameters::video(CodecId::new("vfw_round258_badval"));
    params.options.insert(ENCODER_KNOB_QUALITY, "not-a-number");
    params.options.insert("bogus_key", "42");
    assert_eq!(unrecognized_encoder_knobs(&params), vec!["bogus_key"]);
    assert_eq!(resolve_encoder_knobs(&params).quality, 0);
}
