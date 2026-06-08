//! Round 257 — typed encoder-knobs query API integration tests.
//!
//! The encoder honours three optional `CodecParameters.options`
//! bridge knobs: `"quality"` (clamped to `0..=10_000`), `"keyint"`
//! (force every Nth frame keyframe), and `"data_rate"` (per-frame
//! byte ceiling for `ICCompress`'s `dwFrameSizeLimit` slot). Before
//! round 257 these were parsed inline inside
//! `SandboxedVfwEncoder::new`; a downstream caller (CLI tool /
//! integration test / pipeline pre-validator) had no public way to
//! ask "what would the encoder resolve my options to?" without
//! constructing an encoder and reaching into private fields.
//!
//! Round 257 lifts the parser into a typed
//! `oxideav_vfw::discovery::resolve_encoder_knobs(&CodecParameters)
//! -> EncoderKnobs` query API. The encoder's `new` routes through
//! the same helper so the construction path and the query API can't
//! drift apart. These tests exercise the public surface end-to-end
//! through the crate-root re-exports.

#![cfg(feature = "auto-discovery")]

use oxideav_core::{CodecId, CodecParameters};
use oxideav_vfw::discovery::{resolve_encoder_knobs, EncoderKnobs, ENCODER_QUALITY_MAX};

/// The empty-options case produces an `EncoderKnobs::default()` —
/// all three knobs land on their `0` sentinel. Locks the contract
/// that a caller can diff against `EncoderKnobs::default()` to
/// decide whether the user opted into any knob at all.
#[test]
fn empty_options_produces_default_knobs() {
    let params = CodecParameters::video(CodecId::new("vfw_round257_empty"));
    let knobs = resolve_encoder_knobs(&params);
    assert_eq!(knobs, EncoderKnobs::default());
    assert_eq!(knobs.quality, 0);
    assert_eq!(knobs.keyint, 0);
    assert_eq!(knobs.data_rate, 0);
}

/// A fully-populated options map round-trips through the typed
/// view with each value preserved. This is the canonical
/// pre-encoder-construction validation flow: a CLI tool reads
/// `--quality 8000 --keyint 30 --data-rate 1400`, stuffs them into
/// `params.options`, and asks the resolver to surface the values
/// the encoder will see — without having to construct an encoder.
#[test]
fn fully_populated_options_round_trip() {
    let mut params = CodecParameters::video(CodecId::new("vfw_round257_full"));
    params.options.insert("quality", "8000");
    params.options.insert("keyint", "30");
    params.options.insert("data_rate", "1400");
    let knobs = resolve_encoder_knobs(&params);
    assert_eq!(knobs.quality, 8000);
    assert_eq!(knobs.keyint, 30);
    assert_eq!(knobs.data_rate, 1400);
}

/// The `quality` clamp ceiling (`ENCODER_QUALITY_MAX = 10_000`) is
/// inclusive — a value at exactly the ceiling passes through, and
/// a value past it saturates at the ceiling. Mirrors the round-112
/// inline clamp, lifted to a named constant in round 257.
#[test]
fn quality_clamp_is_inclusive_at_ceiling() {
    let mut params = CodecParameters::video(CodecId::new("vfw_round257_clamp"));

    // Exactly the maximum — preserved.
    params.options.insert("quality", "10000");
    assert_eq!(resolve_encoder_knobs(&params).quality, ENCODER_QUALITY_MAX);
    assert_eq!(ENCODER_QUALITY_MAX, 10_000);

    // Over the maximum — saturated.
    params.options.insert("quality", "999999");
    assert_eq!(resolve_encoder_knobs(&params).quality, ENCODER_QUALITY_MAX);
}

/// `data_rate` and `keyint` are NOT clamped — they pass through
/// verbatim. The codec is the arbiter of plausibility for the byte
/// ceiling; an over-large value just degrades to a no-op hint
/// rather than rejecting the encoder construction.
#[test]
fn keyint_and_data_rate_pass_through_verbatim() {
    let mut params = CodecParameters::video(CodecId::new("vfw_round257_nonclamp"));
    params.options.insert("keyint", "99999");
    params.options.insert("data_rate", "1000000000"); // 1 GB/frame
    let knobs = resolve_encoder_knobs(&params);
    assert_eq!(knobs.keyint, 99999);
    assert_eq!(knobs.data_rate, 1_000_000_000);
}

/// A malformed value on any single knob falls back to the per-knob
/// default. The remaining well-formed knobs still parse cleanly —
/// a single bad key never poisons the others. This is the
/// best-effort contract that lets a caller layer the resolver into
/// a UI form without having to pre-validate each field separately.
#[test]
fn malformed_values_fall_back_independently() {
    let mut params = CodecParameters::video(CodecId::new("vfw_round257_bad"));
    params.options.insert("quality", "not-a-number");
    params.options.insert("keyint", "10");
    params.options.insert("data_rate", "also-not-a-number");
    let knobs = resolve_encoder_knobs(&params);
    assert_eq!(knobs.quality, 0);
    assert_eq!(knobs.keyint, 10);
    assert_eq!(knobs.data_rate, 0);
}

/// Whitespace around the value parses cleanly — the underlying
/// parser already calls `.trim()` on the value, which lets the
/// resolver tolerate `.env` files / systemd `Environment=` lines /
/// YAML quoting where stray whitespace frequently leaves a
/// leading or trailing space around the value. Mirrors the
/// round-211 whitespace strip on `OXIDEAV_VFW_CODEC_PATH`
/// components — the same forgiving-input policy across the crate's
/// configuration surface.
#[test]
fn values_tolerate_surrounding_whitespace() {
    let mut params = CodecParameters::video(CodecId::new("vfw_round257_ws"));
    params.options.insert("quality", "  7500  ");
    params.options.insert("keyint", "\t15\t");
    params.options.insert("data_rate", " 2048\n");
    let knobs = resolve_encoder_knobs(&params);
    assert_eq!(knobs.quality, 7500);
    assert_eq!(knobs.keyint, 15);
    assert_eq!(knobs.data_rate, 2048);
}

/// `EncoderKnobs` is `Copy` — the typed view is intended to be
/// passed by value through pipeline stages and stored cheaply in
/// configuration structs without forcing every caller to clone.
/// A regression to `Clone`-only would silently break callers that
/// pattern-match on the struct + try to use the bindings later.
#[test]
fn encoder_knobs_is_copy() {
    fn require_copy<T: Copy>(_: T) {}
    let mut params = CodecParameters::video(CodecId::new("vfw_round257_copy"));
    params.options.insert("quality", "5000");
    let knobs = resolve_encoder_knobs(&params);
    let copy_of = knobs; // implicit copy
    require_copy(knobs);
    assert_eq!(copy_of.quality, 5000);
    assert_eq!(knobs.quality, 5000); // original still usable
}
