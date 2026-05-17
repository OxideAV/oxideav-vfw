//! Bridge layer — `oxideav_core::Decoder` adapter smoke tests.
//!
//! Verifies the discovery → factory → make_decoder path constructs
//! a working `Box<dyn Decoder>` lazily for both `Kind::Vfw` and
//! `Kind::DirectShow` records. Real codec decode (byte-equality
//! between trait path and manual path) lives in ud-emulator's
//! per-codec corpus; this crate's responsibility ends at
//! "the factory wires up correctly".

#![cfg(feature = "auto-discovery")]

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, PixelFormat};
use oxideav_vfw::discovery::{
    codec_id_for, make_decoder, output_pixel_format, register_factory_for_id, DiscoveryRecord, Kind,
};

#[test]
fn output_pixel_format_is_bgr24() {
    assert_eq!(output_pixel_format(), PixelFormat::Bgr24);
}

#[test]
fn make_decoder_without_width_constructs_with_get_format_probe_path() {
    // Width is optional on `CodecParameters`. When missing, the
    // decoder probes the codec via `ICM_DECOMPRESS_GET_FORMAT` on
    // first `send_packet` and populates dims from the codec's
    // reply. `make_decoder` itself succeeds; the failure (if any)
    // surfaces from the probe.
    let id = "vfw_mp43_round29_test_no_width";
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path: PathBuf::from("/dev/null"),
            fourcc: "MP43".into(),
            kind: Kind::Vfw,
            clsid: None,
        },
    );
    let params = CodecParameters::video(CodecId::new(id));
    let decoder = make_decoder(&params).expect("make_decoder constructs lazily");
    assert_eq!(decoder.codec_id().as_str(), id);
}

#[test]
fn make_decoder_dshow_kind_now_constructs_lazily() {
    // DShow path constructs a `SandboxedDshowDecoder` at
    // make_decoder time. The real DLL load + IPin handshake run
    // on first `send_packet`; failure surfaces from
    // `receive_frame`.
    let id = "vfw_round29_dshow_lazy_construct";
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path: PathBuf::from("/dev/null"),
            fourcc: "WMV3".into(),
            kind: Kind::DirectShow,
            clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".into()),
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(176);
    params.height = Some(144);
    let decoder = make_decoder(&params).expect("DShow make_decoder constructs lazily");
    assert_eq!(decoder.codec_id().as_str(), id);
}

/// `codec_id_for` is exposed through the `discovery` module so
/// downstream tooling can synthesise the same id the `register()`
/// cascade builds. Smoke-test it stays stable.
#[test]
fn codec_id_for_matches_round28_format() {
    let path = PathBuf::from("/some/path/MPG4C32.DLL");
    assert_eq!(codec_id_for(&path, "MP43"), "vfw_mp43_mpg4c32");
}
