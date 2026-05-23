//! Bridge layer — `oxideav_core::Encoder` adapter smoke tests
//! (round 107).
//!
//! The encode-side mirror of `round29_decoder_trait_integration`.
//! Verifies the discovery → factory → `make_encoder` path constructs
//! a working `Box<dyn Encoder>` lazily for `Kind::Vfw` records and
//! rejects the kinds that have no `ICCompress*` lifecycle through
//! this bridge.
//!
//! Real codec encode (byte-equality between the trait path and the
//! manual `ud vfw encode` path) lives in ud-emulator's per-codec
//! corpus; this crate's responsibility ends at "the factory wires
//! up correctly + the lifecycle plumbing is sound".

#![cfg(feature = "auto-discovery")]

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, CodecTag, Frame, VideoFrame, VideoPlane};
use oxideav_vfw::discovery::{make_encoder, register_factory_for_id, DiscoveryRecord, Kind};

/// `Kind::Vfw` records construct a `SandboxedVfwEncoder` lazily:
/// `make_encoder` validates only the FourCC + the output-params
/// wiring; the DLL load + `ICCompress*` handshake run on first
/// `send_frame`.
#[test]
fn make_encoder_vfw_constructs_lazily_with_dims() {
    let id = "vfw_mp43_round107_encoder_lazy";
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path: PathBuf::from("/dev/null"),
            fourcc: "MP43".into(),
            kind: Kind::Vfw,
            clsid: None,
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(176);
    params.height = Some(144);
    let enc = make_encoder(&params).expect("VfW make_encoder constructs lazily");
    assert_eq!(enc.codec_id().as_str(), id);

    // Output stream params echo the dims and tag the stream with the
    // codec's FourCC so a downstream muxer re-emits MP43.
    let op = enc.output_params();
    assert_eq!(op.width, Some(176));
    assert_eq!(op.height, Some(144));
    assert_eq!(op.tag, Some(CodecTag::fourcc(b"MP43")));
}

/// DirectShow filters have no `ICCompress*` encode path through this
/// bridge — `make_encoder` rejects them cleanly.
#[test]
fn make_encoder_dshow_kind_is_rejected() {
    let id = "vfw_round107_dshow_encoder_unsupported";
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path: PathBuf::from("/dev/null"),
            fourcc: "WMV3".into(),
            kind: Kind::DirectShow,
            clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".into()),
        },
    );
    let params = CodecParameters::video(CodecId::new(id));
    assert!(make_encoder(&params).is_err());
}

/// `send_frame` rejects non-video frames before touching the codec —
/// the lazy `ensure_open` only fires for a video frame, so an audio
/// frame surfaces a clean `Err` without a DLL read. (A `/dev/null`
/// DLL path means any DLL read would itself fail, so this asserts the
/// frame-kind guard runs FIRST.)
#[test]
fn send_frame_rejects_audio_before_dll_load() {
    use oxideav_core::AudioFrame;
    let id = "vfw_mp43_round107_audio_guard";
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path: PathBuf::from("/this/dll/does/not/exist"),
            fourcc: "MP43".into(),
            kind: Kind::Vfw,
            clsid: None,
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(16);
    params.height = Some(16);
    let mut enc = make_encoder(&params).expect("constructs");
    let audio = Frame::Audio(AudioFrame {
        samples: 1,
        pts: Some(0),
        data: vec![vec![0u8; 4]],
    });
    let err = enc.send_frame(&audio).expect_err("audio rejected");
    // The error must mention the video-only guard, NOT a DLL read
    // failure — proving the kind check precedes `ensure_open`.
    let msg = format!("{err}");
    assert!(
        msg.contains("only video frames"),
        "expected video-only guard error, got: {msg}"
    );
}

/// Without dims on `CodecParameters`, the encode path cannot probe a
/// raw frame for its dimensions; `ensure_open` (driven from the first
/// `send_frame`) surfaces a clean invalid-argument error rather than
/// guessing. The frame supplied is a 1-plane BGR24 buffer so we reach
/// `ensure_open` past the frame-kind guard.
#[test]
fn send_frame_without_dims_errors_before_codec_open() {
    let id = "vfw_mp43_round107_missing_dims";
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path: PathBuf::from("/this/dll/does/not/exist"),
            fourcc: "MP43".into(),
            kind: Kind::Vfw,
            clsid: None,
        },
    );
    // No width/height on params.
    let params = CodecParameters::video(CodecId::new(id));
    let mut enc = make_encoder(&params).expect("constructs");
    let frame = Frame::Video(VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: 48,
            data: vec![0u8; 48 * 16],
        }],
    });
    let err = enc.send_frame(&frame).expect_err("missing dims rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("width/height must be supplied"),
        "expected missing-dims error, got: {msg}"
    );
}
