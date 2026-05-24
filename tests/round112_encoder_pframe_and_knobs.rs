//! Bridge layer — `oxideav_core::Encoder` P-frame reference + quality /
//! keyframe-interval knob smoke tests (round 112).
//!
//! Round 107 landed the encode-side factory; every frame was encoded as
//! an independent unit (`prev_bih_opt = None`). Round 112 threads the
//! previous raw input frame through `ICCompress`'s `lpPrev` slot on
//! non-keyframe encodes and honours two optional
//! `CodecParameters.options` knobs:
//!
//! * `"quality"` (u32 0..10000) → `ICCompress`'s `quality` slot.
//! * `"keyint"` (u32 frames) → force every Nth frame to a keyframe.
//!
//! These tests verify the factory wiring + the options-parsing path
//! through the public `make_encoder` surface. The DLL is never loaded
//! (construction is lazy), so a `/dev/null` path is fine — they stop
//! at "the encoder constructs with the right knobs". Real codec encode
//! (byte-equality between the trait path and the manual `ud vfw encode`
//! path, including P-frame deltas) lives in ud-emulator's per-codec
//! corpus.

#![cfg(feature = "auto-discovery")]

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, CodecTag};
use oxideav_vfw::discovery::{make_encoder, register_factory_for_id, DiscoveryRecord, Kind};

fn register_vfw(id: &str) {
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path: PathBuf::from("/dev/null"),
            fourcc: "MP43".into(),
            kind: Kind::Vfw,
            clsid: None,
        },
    );
}

/// An encoder built with `quality` / `keyint` options still constructs
/// cleanly (the knobs are read at construction time, applied lazily on
/// the first `ICCompress`). Output params are unaffected by the knobs.
#[test]
fn make_encoder_with_quality_and_keyint_options_constructs() {
    let id = "vfw_mp43_round112_knobs";
    register_vfw(id);
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(176);
    params.height = Some(144);
    params.options.insert("quality", "8000");
    params.options.insert("keyint", "15");

    let enc = make_encoder(&params).expect("VfW make_encoder constructs with knobs");
    assert_eq!(enc.codec_id().as_str(), id);

    let op = enc.output_params();
    assert_eq!(op.width, Some(176));
    assert_eq!(op.height, Some(144));
    assert_eq!(op.tag, Some(CodecTag::fourcc(b"MP43")));
}

/// A malformed `quality` value does NOT fail construction — the bridge
/// knob falls back to the codec-chooses default (best-effort policy).
#[test]
fn make_encoder_tolerates_malformed_quality_option() {
    let id = "vfw_mp43_round112_bad_quality";
    register_vfw(id);
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(32);
    params.height = Some(32);
    params.options.insert("quality", "not-a-number");

    let enc = make_encoder(&params).expect("malformed knob falls back, does not fail");
    assert_eq!(enc.codec_id().as_str(), id);
}

/// No options at all → encoder still constructs (the round-107 contract
/// is preserved; the new knobs are strictly additive).
#[test]
fn make_encoder_without_options_still_constructs() {
    let id = "vfw_mp43_round112_no_options";
    register_vfw(id);
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(64);
    params.height = Some(48);

    let enc = make_encoder(&params).expect("no-options encoder constructs");
    let op = enc.output_params();
    assert_eq!(op.width, Some(64));
    assert_eq!(op.height, Some(48));
}
