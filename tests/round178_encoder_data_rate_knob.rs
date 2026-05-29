//! Bridge layer — `oxideav_core::Encoder` `data_rate` knob smoke tests
//! (round 178).
//!
//! Round 112 wired the `quality` + `keyint` knobs; round 178 adds the
//! `data_rate` (per-frame byte ceiling) knob alongside them. The knob
//! is plumbed verbatim into `ICCompress`'s `dwFrameSizeLimit` slot —
//! `0` keeps the historical "codec chooses" behaviour, non-zero hints
//! a per-frame byte cap (useful for MTU-bounded transports).
//!
//! These tests exercise the factory + options-parsing path through the
//! public `make_encoder` surface. The DLL is never loaded (construction
//! is lazy), so a `/dev/null` path is fine. Real codec encode validation
//! against the manual `ud vfw encode` corpus lives in ud-emulator.

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

/// An encoder built with a `data_rate` knob still constructs cleanly —
/// the knob is read at construction time and applied lazily on the
/// first `ICCompress`. Output params are unaffected by the knob.
#[test]
fn make_encoder_with_data_rate_option_constructs() {
    let id = "vfw_mp43_round178_data_rate";
    register_vfw(id);
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(176);
    params.height = Some(144);
    // 1400 bytes/frame — roughly the IP/UDP/RTP payload room inside a
    // 1500-byte Ethernet MTU. This is the typical use-case for the
    // knob: hint the codec not to emit a frame bigger than this so the
    // muxer doesn't have to fragment.
    params.options.insert("data_rate", "1400");

    let enc = make_encoder(&params).expect("VfW make_encoder constructs with data_rate");
    assert_eq!(enc.codec_id().as_str(), id);

    let op = enc.output_params();
    assert_eq!(op.width, Some(176));
    assert_eq!(op.height, Some(144));
    assert_eq!(op.tag, Some(CodecTag::fourcc(b"MP43")));
}

/// A malformed `data_rate` value does NOT fail construction — the
/// bridge knob falls back to the codec-chooses default (best-effort
/// policy, same shape as the round-112 `quality` / `keyint` knobs).
#[test]
fn make_encoder_tolerates_malformed_data_rate_option() {
    let id = "vfw_mp43_round178_bad_data_rate";
    register_vfw(id);
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(32);
    params.height = Some(32);
    params.options.insert("data_rate", "not-a-number");

    let enc = make_encoder(&params).expect("malformed data_rate knob falls back, does not fail");
    assert_eq!(enc.codec_id().as_str(), id);
}

/// All three knobs can coexist on the same encoder without
/// interfering with each other — each is parsed independently from a
/// distinct options key.
#[test]
fn make_encoder_with_quality_keyint_and_data_rate_all_set() {
    let id = "vfw_mp43_round178_all_knobs";
    register_vfw(id);
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(320);
    params.height = Some(240);
    params.options.insert("quality", "7500");
    params.options.insert("keyint", "30");
    params.options.insert("data_rate", "2048");

    let enc = make_encoder(&params).expect("encoder accepts all three knobs at once");
    assert_eq!(enc.codec_id().as_str(), id);
    let op = enc.output_params();
    assert_eq!(op.width, Some(320));
    assert_eq!(op.height, Some(240));
}
