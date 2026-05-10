//! Round 38 — identify the C++ class base of the codec's filter,
//! verify `[filter_base + 0x8c]` (the `m_pInput` field that traps
//! at MPG4DS32 RVA `0x7184`) is non-NULL after EnumPins → Next,
//! and capture the exact disagreement between the pre-Receive
//! sanity-check view and the trap-time `[ebx+0x8c]==NULL` view.
//!
//! Round 36/37 background:
//!  * The codec's `IMemInputPin::Receive` trap site is at codec
//!    RVA `0x7184` = `repe cmpsd` inside the inlined helper at
//!    `0x7176` (`bool IsEqualGUID(this+0x1c, &kStaticGUID)`).
//!  * Round-37 hypothesis: `[filter+0x8c]` was NULL because the
//!    codec's lazy-init never ran for our scenario.
//!  * Round-38 disasm (RVA `0x33fd` = primary-vtable slot 7,
//!    `0x6334` = secondary-vtable slot N) reveals the codec has
//!    TWO GetPin helpers in two DIFFERENT vtables: one in the
//!    primary C++ class vtable at `0x269f4`, one in the
//!    secondary IBaseFilter sub-vtable at `0x269b8`.  The
//!    `IBaseFilter` pointer we hold via CoCreateInstance + QI
//!    points to the secondary vtable address (which equals
//!    `filter_base + 0xc`); the primary vtable lives at
//!    `[filter_base + 0]`.
//!
//! This test confirms our pre-Receive diagnostic identifies
//! `filter_base` correctly (= `self.filter - 0xc`), reads
//! `[filter_base + 0]` as `0x1c4269f4` (the primary C++ class
//! vtable), and reads `[filter_base + 0x8c]` as a NON-NULL pin
//! pointer — proving the lazy-init DID run before Receive.
//!
//! The trap therefore is NOT on `[self.filter + 0x8c]==NULL` as
//! r36/r37 hypothesized; it's on a DIFFERENT object reached
//! deeper in the Transform call chain.  See `round37_pin_
//! introspection::r37_production_path_traps_differently_or_
//! records_introspection`'s diagnostic dump for the new
//! `r38_pre=` / `mip_state=` / `call_chain=` evidence.

#![cfg(feature = "auto-discovery")]

mod common;

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, Packet, TimeBase};
use oxideav_vfw::discovery::{
    last_codec_allocator_negotiation, make_decoder, register_factory_for_id, DiscoveryRecord, Kind,
};

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn dshow_dll_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/MPG4DS32.AX");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn mp43_fixture_path(stem: &str) -> Option<PathBuf> {
    let p = workspace_root()?.join(format!("docs/video/msmpeg4-fixtures/{stem}/input.avi"));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn extract_mp43_keyframe(stem: &str) -> Option<(u32, u32, Vec<u8>)> {
    let path = mp43_fixture_path(stem)?;
    let bytes = std::fs::read(&path).ok()?;
    let s = common::avi_extractor::extract_video_sample(&bytes, 0).ok()?;
    Some((s.width, s.height, s.bytes))
}

// ────────────────────────────────────────────────────────────────
// Test 1 — production path emits an enriched `r38_pre=...` diag
// section in the trap message, proving the round-38 pre-Receive
// sanity dump fires before the call.
// ────────────────────────────────────────────────────────────────

#[test]
fn r38_trap_message_carries_pre_receive_sanity_dump() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round38 sanity: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let (width, height, keyframe) = match extract_mp43_keyframe("fourcc-MP43") {
        Some(t) => t,
        None => {
            eprintln!("round38 sanity: MP43 fixture missing; skipping");
            return;
        }
    };
    let id = "vfw_round38_sanity".to_string();
    register_factory_for_id(
        &id,
        DiscoveryRecord {
            dll_path,
            fourcc: "MP43".to_string(),
            kind: Kind::DirectShow,
            clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".to_string()),
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id.clone()));
    params.width = Some(width);
    params.height = Some(height);
    let mut decoder = make_decoder(&params).expect("make_decoder");
    let packet = Packet::new(0, TimeBase::new(1, 25), keyframe).with_keyframe(true);
    let _ = decoder.send_packet(&packet);
    let outcome = decoder.receive_frame();
    if let Some(neg) = last_codec_allocator_negotiation(&id) {
        // Round-38 must preserve r36's negotiation baseline.
        assert_eq!(
            neg.get_allocator_hr, 0,
            "r38 must preserve r36 GA=S_OK baseline"
        );
        assert_ne!(
            neg.codec_allocator, 0,
            "r38 must preserve r36 non-NULL codec allocator"
        );
        assert_eq!(neg.set_properties_hr, 0, "r38 SetProperties=S_OK preserved");
        assert_eq!(neg.commit_hr, 0, "r38 Commit=S_OK preserved");
        assert!(neg.using_codec_allocator, "r38 codec allocator preserved");
    }
    match outcome {
        Ok(other) => {
            eprintln!("round38 sanity: unexpected Ok({other:?})");
        }
        Err(e) => {
            let msg = format!("{e}");
            eprintln!("round38 sanity: receive_frame → Err({msg})");
            // The new r38 diagnostic prefix must appear in the trap
            // message body (proving the pre-Receive dump fired).
            assert!(
                msg.contains("r38_pre="),
                "r38 must carry pre-Receive sanity dump: {msg}"
            );
            assert!(
                msg.contains("filter_base="),
                "r38 must report filter_base computation: {msg}"
            );
            assert!(
                msg.contains("[filter_base+0]="),
                "r38 must dump primary-vtable address: {msg}"
            );
            assert!(
                msg.contains("[filter_base+0x8c]="),
                "r38 must dump m_pInput field: {msg}"
            );
            // r38 finding: `[filter_base+0]` must be the codec's
            // primary C++ class vtable (`0x1c4269f4`).
            assert!(
                msg.contains("[filter_base+0]=0x1c4269f4"),
                "r38 expected primary vtable 0x1c4269f4: {msg}"
            );
            // r38 finding: `[filter_base+0x8c]` is NON-NULL after
            // EnumPins/Next runs in `ensure_open` — meaning the
            // codec's input pin IS allocated, contrary to r36/r37
            // hypothesis.
            assert!(
                !msg.contains("[filter_base+0x8c]=0x00000000"),
                "r38 expected NON-NULL m_pInput (lazy-init ran): {msg}"
            );
            // The trap is still at RVA 0x7184 (NULL+0x1c read in
            // `IsEqualGUID`), which means the trap object is
            // DIFFERENT from `filter_base` — r39 needs to identify
            // which intermediate object's `+0x8c` is being read.
            assert!(
                msg.contains("rva=0x00007184"),
                "r38 trap site unchanged from r36 baseline (proves the r37 + r38 surface \
                 still doesn't reach the failing object): {msg}"
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Test 2 — sample slot 13 resolves to the host `sample_get_media_
// _type` thunk address (in `0xFFFE_xxxx` thunk space), not to a
// codec address.  This proves the input sample we pass IS our
// host sample, ruling out a sample-substitution path inside the
// codec's allocator handshake.
// ────────────────────────────────────────────────────────────────

#[test]
fn r38_input_sample_vtable_slot_13_is_host_thunk() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round38 sample-slot13: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let (width, height, keyframe) = match extract_mp43_keyframe("fourcc-MP43") {
        Some(t) => t,
        None => {
            eprintln!("round38 sample-slot13: MP43 fixture missing; skipping");
            return;
        }
    };
    let id = "vfw_round38_sample_slot13".to_string();
    register_factory_for_id(
        &id,
        DiscoveryRecord {
            dll_path,
            fourcc: "MP43".to_string(),
            kind: Kind::DirectShow,
            clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".to_string()),
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id.clone()));
    params.width = Some(width);
    params.height = Some(height);
    let mut decoder = make_decoder(&params).expect("make_decoder");
    let packet = Packet::new(0, TimeBase::new(1, 25), keyframe).with_keyframe(true);
    let _ = decoder.send_packet(&packet);
    let outcome = decoder.receive_frame();
    let msg = match outcome {
        Ok(other) => {
            eprintln!("round38 sample-slot13: unexpected Ok({other:?})");
            return;
        }
        Err(e) => format!("{e}"),
    };
    // The diagnostic carries `sample_vtbl[+0x34]=0xfffe...` —
    // a host thunk address.  If this changes to a codec RVA
    // (`0x1c4...`), it means our allocator handshake is somehow
    // returning a codec-internal sample, and r39 should chase
    // that.
    assert!(
        msg.contains("sample_vtbl[+0x34]=0xfffe"),
        "r38 input sample's slot 13 must be a host thunk (was: {msg})"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 3 — round-37 baseline preservation: GA/SP/CO=S_OK,
// using_codec_allocator=true.  r38 must not regress.
// ────────────────────────────────────────────────────────────────

#[test]
fn r38_round_37_negotiation_baseline_preserved() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round38 baseline: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let (width, height, keyframe) = match extract_mp43_keyframe("fourcc-MP43") {
        Some(t) => t,
        None => {
            eprintln!("round38 baseline: MP43 fixture missing; skipping");
            return;
        }
    };
    let id = "vfw_round38_baseline".to_string();
    register_factory_for_id(
        &id,
        DiscoveryRecord {
            dll_path,
            fourcc: "MP43".to_string(),
            kind: Kind::DirectShow,
            clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".to_string()),
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id.clone()));
    params.width = Some(width);
    params.height = Some(height);
    let mut decoder = make_decoder(&params).expect("make_decoder");
    let packet = Packet::new(0, TimeBase::new(1, 25), keyframe).with_keyframe(true);
    let _ = decoder.send_packet(&packet);
    let _ = decoder.receive_frame();
    let neg = last_codec_allocator_negotiation(&id).expect("negotiation captured");
    assert_eq!(
        neg.get_allocator_hr, 0,
        "r38 must preserve r37 GA=S_OK baseline; got {:#010x}",
        neg.get_allocator_hr,
    );
    assert_ne!(
        neg.codec_allocator, 0,
        "r38 must preserve r37 non-NULL codec allocator"
    );
    assert_eq!(
        neg.set_properties_hr, 0,
        "r38 must preserve r37 SetProperties=S_OK baseline"
    );
    assert_eq!(
        neg.commit_hr, 0,
        "r38 must preserve r37 Commit=S_OK baseline"
    );
    assert!(
        neg.using_codec_allocator,
        "r38 must preserve r37 using_codec_allocator=true baseline"
    );
}
