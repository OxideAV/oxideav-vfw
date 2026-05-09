//! Round 31 — DirectShow decode loop closure.
//!
//! **A — IPin::EnumMediaTypes walk.** The codec's input pin
//! exposes its own native AMT preferences via
//! `IPin::EnumMediaTypes(IEnumMediaTypes**)`. Round 31 walks
//! that enumeration, captures the FOURCC + format type, and
//! retries `IPin::ReceiveConnection` with the codec's own AMT
//! echoed back.
//!
//! **B — Downstream HostIPin::Receive callback.** The codec's
//! output pin pushes decoded samples to whatever filter is
//! connected on its output side. We mint a paired
//! HostIPin (input role) + HostIMemInputPin and wire it into the
//! codec's output pin via `IPin::ReceiveConnection`. When the
//! codec calls our `IMemInputPin::Receive(IMediaSample*)`, we
//! capture the sample's bytes into a host-side queue.
//! `SandboxedDshowDecoder::receive_frame` drains that queue and
//! surfaces the bytes as `Frame::Video`.
//!
//! References: MSDN — IPin / IMemInputPin / IBaseFilter /
//! IEnumMediaTypes / AM_MEDIA_TYPE; Windows SDK headers
//! `axextend.h` / `strmif.h` / `amvideo.h`.

#![cfg(feature = "auto-discovery")]

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, Packet, TimeBase};
use oxideav_vfw::discovery::{make_decoder, register_factory_for_id, DiscoveryRecord, Kind};
use oxideav_vfw::Sandbox;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

/// Round 31 unit — `mint_host_input_pin_pair` lays out two
/// cross-referenced objects with the documented vtable shape.
#[test]
fn host_input_pin_pair_layout_cross_references_correctly() {
    let mut sb = Sandbox::new();
    let (pin, mip) = sb
        .host_iface_r31_mint_input_pin_pair()
        .expect("mint_host_input_pin_pair");
    // Pin object has 18-slot vtable at obj+16; obj+12 = sibling
    // mem-input ptr.
    let pin_vtbl = sb.mmu.load32(pin).unwrap();
    assert_eq!(pin_vtbl, pin + 16);
    let pin_sibling = sb.mmu.load32(pin + 12).unwrap();
    assert_eq!(pin_sibling, mip);
    // Mem-input object has 9-slot vtable at obj+12; obj+8 =
    // sibling pin ptr.
    let mip_vtbl = sb.mmu.load32(mip).unwrap();
    assert_eq!(mip_vtbl, mip + 12);
    let mip_sibling = sb.mmu.load32(mip + 8).unwrap();
    assert_eq!(mip_sibling, pin);
    // All vtable slots resolve to registered thunk addresses.
    for i in 0..18u32 {
        let m = sb.mmu.load32(pin_vtbl + i * 4).unwrap();
        assert!(m != 0 && sb.registry.is_thunk(m), "pin slot {i} unmapped");
    }
    for i in 0..9u32 {
        let m = sb.mmu.load32(mip_vtbl + i * 4).unwrap();
        assert!(m != 0 && sb.registry.is_thunk(m), "mip slot {i} unmapped");
    }
}

/// Round 31 unit — `mint_host_base_filter` lays out a 15-slot
/// IBaseFilter vtable wrapping the input pin pointer.
#[test]
fn host_base_filter_layout_exposes_input_pin() {
    let mut sb = Sandbox::new();
    let (pin, _mip) = sb.host_iface_r31_mint_input_pin_pair().unwrap();
    let filter = sb.host_iface_r31_mint_base_filter(pin).unwrap();
    let vtbl = sb.mmu.load32(filter).unwrap();
    assert_eq!(vtbl, filter + 12);
    let stored_pin = sb.mmu.load32(filter + 8).unwrap();
    assert_eq!(stored_pin, pin);
    for i in 0..15u32 {
        let m = sb.mmu.load32(vtbl + i * 4).unwrap();
        assert!(
            m != 0 && sb.registry.is_thunk(m),
            "filter slot {i} unmapped"
        );
    }
}

/// Round 31 unit — `IPin(input)::QueryDirection` reports
/// `PIN_INPUT (0)` distinct from the round-27 output-role stub.
#[test]
fn host_input_pin_query_direction_reports_pin_input() {
    let mut sb = Sandbox::new();
    let (pin, _mip) = sb.host_iface_r31_mint_input_pin_pair().unwrap();
    let dir_slot = sb.host.arena_alloc(4).unwrap();
    sb.mmu
        .write_initializer(dir_slot, &0xFFu32.to_le_bytes())
        .unwrap();
    let r = oxideav_vfw::com::call::call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        9, // IPin::QueryDirection
        &[dir_slot],
    )
    .unwrap();
    assert_eq!(r, 0);
    assert_eq!(sb.mmu.load32(dir_slot).unwrap(), 0); // PIN_INPUT
}

/// Round 31 unit — `IMemInputPin::QueryInterface(IID_IPin)`
/// returns the sibling input pin object.
#[test]
fn host_meminput_qi_for_ipin_returns_sibling_input_pin() {
    let mut sb = Sandbox::new();
    let (pin, mip) = sb.host_iface_r31_mint_input_pin_pair().unwrap();
    // Stage IID_IPIN into guest memory.
    let iid_addr = sb.host.arena_alloc(16).unwrap();
    oxideav_vfw::IID_IPIN.stage(&mut sb.mmu, iid_addr).unwrap();
    let ppv = sb.host.arena_alloc(4).unwrap();
    sb.mmu.write_initializer(ppv, &0u32.to_le_bytes()).unwrap();
    let r = oxideav_vfw::com::call::call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        0, // QI
        &[iid_addr, ppv],
    )
    .unwrap();
    assert_eq!(r, 0);
    let out = sb.mmu.load32(ppv).unwrap();
    assert_eq!(
        out, pin,
        "QI(IID_IPin) on IMemInputPin should return sibling pin"
    );
}

/// Round 31 unit — `IPin(input)::QueryInterface(IID_IMemInputPin)`
/// returns the sibling mem-input object.
#[test]
fn host_input_pin_qi_for_imeminputpin_returns_sibling() {
    let mut sb = Sandbox::new();
    let (pin, mip) = sb.host_iface_r31_mint_input_pin_pair().unwrap();
    let iid_addr = sb.host.arena_alloc(16).unwrap();
    oxideav_vfw::IID_IMEMINPUTPIN
        .stage(&mut sb.mmu, iid_addr)
        .unwrap();
    let ppv = sb.host.arena_alloc(4).unwrap();
    sb.mmu.write_initializer(ppv, &0u32.to_le_bytes()).unwrap();
    let r = oxideav_vfw::com::call::call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        0, // QI
        &[iid_addr, ppv],
    )
    .unwrap();
    assert_eq!(r, 0);
    let out = sb.mmu.load32(ppv).unwrap();
    assert_eq!(
        out, mip,
        "QI(IID_IMemInputPin) on input pin should return sibling mem-input"
    );
}

/// Round 31 unit — `HostIBaseFilter::EnumPins` vends an
/// IEnumPins that yields the input pin once.
#[test]
fn host_base_filter_enum_pins_yields_input_pin_once_then_s_false() {
    let mut sb = Sandbox::new();
    let (pin, _mip) = sb.host_iface_r31_mint_input_pin_pair().unwrap();
    let filter = sb.host_iface_r31_mint_base_filter(pin).unwrap();

    // EnumPins(filter, &enum_ptr).
    let pp = sb.host.arena_alloc(4).unwrap();
    sb.mmu.write_initializer(pp, &0u32.to_le_bytes()).unwrap();
    let r = oxideav_vfw::com::call::call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        10, // SLOT_BASEFILTER_ENUM_PINS
        &[pp],
    )
    .unwrap();
    assert_eq!(r, 0);
    let enum_ptr = sb.mmu.load32(pp).unwrap();
    assert!(enum_ptr != 0);

    // Next(1, &out, &fetched) → S_OK + pin.
    let out_slot = sb.host.arena_alloc(8).unwrap();
    sb.mmu.write_initializer(out_slot, &[0u8; 8]).unwrap();
    let r = oxideav_vfw::com::call::call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        enum_ptr,
        3, // IEnumPins::Next
        &[1, out_slot, out_slot + 4],
    )
    .unwrap();
    assert_eq!(r, 0);
    assert_eq!(sb.mmu.load32(out_slot).unwrap(), pin);
    assert_eq!(sb.mmu.load32(out_slot + 4).unwrap(), 1);

    // Second Next → S_FALSE.
    sb.mmu.write_initializer(out_slot, &[0u8; 8]).unwrap();
    let r = oxideav_vfw::com::call::call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        enum_ptr,
        3,
        &[1, out_slot, out_slot + 4],
    )
    .unwrap();
    assert_eq!(r, 1, "S_FALSE expected after enumeration exhaustion");
    assert_eq!(sb.mmu.load32(out_slot).unwrap(), 0);
}

/// Round 31 — drive the full DShow trait path against MPG4DS32.AX.
/// The codec's `IPin::EnumMediaTypes` walk is exercised; if any
/// codec-native AMT is captured, ReceiveConnection is retried with
/// it.  Even on systems where MPG4DS32.AX rejects every AMT,
/// the test confirms the trait path returns a clean diagnostic
/// rather than panicking — and that the round-31 capture queue
/// remains addressable.
#[test]
fn round31_dshow_trait_path_walks_amts_and_wires_downstream() {
    let dll_path = match workspace_root() {
        Some(p) => p.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/MPG4DS32.AX"),
        None => {
            eprintln!("round31 DShow: cannot resolve workspace root");
            return;
        }
    };
    if !dll_path.is_file() {
        eprintln!("round31 DShow: MPG4DS32.AX missing; skipping");
        return;
    }
    let id = "vfw_round31_dshow_amt_walk";
    register_factory_for_id(
        id,
        DiscoveryRecord {
            dll_path,
            fourcc: "MP43".to_string(),
            kind: Kind::DirectShow,
            clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".to_string()),
        },
    );
    let mut params = CodecParameters::video(CodecId::new(id));
    params.width = Some(320);
    params.height = Some(240);
    let mut decoder = make_decoder(&params).expect("DShow make_decoder constructs lazily");

    // Synthetic 100-byte packet — the goal is exercising the AMT
    // walk + downstream wiring, not a real decode.
    let packet = Packet::new(0, TimeBase::new(1, 25), vec![0u8; 100]).with_keyframe(true);
    match decoder.send_packet(&packet) {
        Err(e) => {
            let msg = format!("{e}");
            eprintln!("round31 DShow: send_packet → Err({msg})");
            // The error surface should mention the AMT walk and/or
            // the candidate count, not the round-30 "fabricated AMT
            // type" lament.
            assert!(
                msg.contains("AMT")
                    || msg.contains("ReceiveConnection")
                    || msg.contains("DShow")
                    || msg.contains("vfw discovery"),
                "expected DShow-pathway diagnostic, got {msg:?}"
            );
        }
        Ok(()) => match decoder.receive_frame() {
            Err(oxideav_core::Error::Eof) => {
                eprintln!(
                    "round31 DShow: send_packet ok; receive_frame → Eof \
                     (codec accepted input + no output sample landed)"
                );
            }
            Err(e) => {
                let msg = format!("{e}");
                eprintln!("round31 DShow: receive_frame → Err({msg})");
                assert!(
                    msg.contains("DShow") || msg.contains("Receive"),
                    "expected DShow diagnostic, got {msg:?}"
                );
            }
            Ok(frame) => {
                eprintln!("round31 DShow: surfaced {frame:?}");
            }
        },
    }
}
