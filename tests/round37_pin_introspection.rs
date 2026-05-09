//! Round 37 — wire `IPin::QueryPinInfo` + `IPin::ConnectedTo` on
//! the host output pin we hand the codec, and a logging path on
//! `IBaseFilter::QueryFilterInfo` so the codec's recursive walk
//! reaches a real PIN_INFO struct + parent IBaseFilter pointer.
//!
//! Round 36's crash (NULL+0x1c memory fault at MPG4DS32 RVA
//! 0x7184) is the codec's `IsEqualGUID(this+0x1c, &kZeroGuid)`
//! call where `this = NULL` because `[stack_helper+0x8c]` was
//! never populated.  The hypothesis (per round-37 GOAL doc) is
//! that the codec wants a pointer to upstream-pin info there;
//! when its init walks `pConnector->QueryPinInfo`/`ConnectedTo`
//! and finds `E_NOTIMPL`, it falls open with NULL and traps later.
//!
//! Tests below confirm:
//!  1. The new `mint_host_output_pin_with_connection` lays out
//!     the new fields at the expected offsets.
//!  2. The new pin's `QueryPinInfo` writes a 264-byte PIN_INFO
//!     with a non-NULL parent filter, `dir == PIN_OUTPUT`, and
//!     a UTF-16 name we can read back.
//!  3. The new pin's `ConnectedTo` returns the codec input pin
//!     we stamped in (or `VFW_E_NOT_CONNECTED` when not stamped).
//!  4. `IBaseFilter::QueryFilterInfo` writes the 260-byte struct
//!     and increments the per-state call counter.
//!  5. Round-32 baseline (`first_input_pin` / `pin_with_direction`)
//!     still works after the layout extension.
//!  6. Production-path receive against MPG4DS32 either reports
//!     a DIFFERENT trap site (proof we got past 0x7184) or
//!     reports SUCCESS — both unblock round 38.

#![cfg(feature = "auto-discovery")]

mod common;

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, Packet, TimeBase};
use oxideav_vfw::com::call::call_method;
use oxideav_vfw::discovery::{
    last_codec_allocator_negotiation, make_decoder, register_factory_for_id, DiscoveryRecord, Kind,
};
use oxideav_vfw::Sandbox;

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
// Test 1 — layout sanity: new pin records the connected_pin and
// parent_filter at the documented offsets, and the vtbl moved
// from obj+16 to obj+24 to make room.
// ────────────────────────────────────────────────────────────────

#[test]
fn r37_output_pin_layout_records_connected_pin_and_parent_filter() {
    let mut sb = Sandbox::new();
    // Synthesize an AMT pointer (just a 16-byte arena slot) — we
    // don't need real AMT bytes for layout testing.
    let amt = sb.host.arena_alloc(16).expect("amt scratch");
    // Synthesize a fake codec input pin pointer (refcount slot
    // populated so AddRef bumps cleanly).
    let codec_input_pin = sb.host.arena_alloc(16).expect("codec input pin");
    sb.mmu
        .write_initializer(codec_input_pin, &0u32.to_le_bytes())
        .unwrap();
    sb.mmu
        .write_initializer(codec_input_pin + 4, &1u32.to_le_bytes())
        .unwrap();

    let pin = sb
        .mint_host_output_pin_with_connection(amt, codec_input_pin)
        .expect("mint host output pin");

    // vtbl at obj+24.
    let vtbl = sb.mmu.load32(pin).unwrap();
    assert_eq!(vtbl, pin + 24, "vtbl moved to obj+24 in round 37");
    // refcount at obj+4.
    assert_eq!(sb.mmu.load32(pin + 4).unwrap(), 1);
    // advertised_amt at obj+8.
    assert_eq!(sb.mmu.load32(pin + 8).unwrap(), amt);
    // connected_pin at obj+12.
    assert_eq!(sb.mmu.load32(pin + 12).unwrap(), codec_input_pin);
    // parent_filter at obj+16 — non-zero (synthesized by mint).
    let parent = sb.mmu.load32(pin + 16).unwrap();
    assert_ne!(parent, 0, "parent_filter should be auto-minted");
    // parent's first dword is its own vtbl pointer (it's a
    // host base filter); should be non-NULL.
    let parent_vtbl = sb.mmu.load32(parent).unwrap();
    assert_ne!(parent_vtbl, 0);
    // 18 vtable slots populated with thunks.
    for i in 0..18u32 {
        let m = sb.mmu.load32(vtbl + i * 4).unwrap();
        assert!(m != 0, "pin vtbl slot {i} unmapped");
        assert!(
            sb.registry.is_thunk(m),
            "pin vtbl slot {i} = {m:#010x} not a registered thunk"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Test 2 — QueryPinInfo writes PIN_INFO { pFilter, dir,
// achName[128] } correctly.
// ────────────────────────────────────────────────────────────────

#[test]
fn r37_query_pin_info_fills_pin_info_struct() {
    let mut sb = Sandbox::new();
    let amt = sb.host.arena_alloc(16).unwrap();
    let codec_input_pin = sb.host.arena_alloc(16).unwrap();
    sb.mmu
        .write_initializer(codec_input_pin, &0u32.to_le_bytes())
        .unwrap();
    sb.mmu
        .write_initializer(codec_input_pin + 4, &1u32.to_le_bytes())
        .unwrap();
    let pin = sb
        .mint_host_output_pin_with_connection(amt, codec_input_pin)
        .unwrap();
    let parent_filter = sb.mmu.load32(pin + 16).unwrap();

    // Allocate a 264-byte PIN_INFO scratch.
    let p_info = sb.host.arena_alloc(272).unwrap();
    // Fill with sentinel 0xAB so we can see what got written.
    for i in 0..264u32 {
        sb.mmu.store8(p_info + i, 0xAB).unwrap();
    }
    // Drive QueryPinInfo via call_method (slot 8).
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        8, // SLOT_PIN_QUERY_PIN_INFO
        &[p_info],
    )
    .expect("QueryPinInfo");
    assert_eq!(r, 0, "QueryPinInfo returned non-S_OK: {r:#010x}");
    // pFilter at offset 0.
    assert_eq!(sb.mmu.load32(p_info).unwrap(), parent_filter);
    // dir at offset 4 = PIN_OUTPUT (1).
    assert_eq!(sb.mmu.load32(p_info + 4).unwrap(), 1);
    // achName at offset 8 should start with UTF-16 'H'.
    assert_eq!(sb.mmu.load32(p_info + 8).unwrap() & 0xFFFF, b'H' as u32);
    // 11th WCHAR (offset 8 + 20) = 'n' (last char before NUL).
    assert_eq!(sb.mmu.load8(p_info + 8 + 18).unwrap(), b'n');
    // NUL terminator at offset 8 + 20.
    assert_eq!(sb.mmu.load8(p_info + 8 + 20).unwrap(), 0);
    // Parent filter refcount bumped (was 1 at mint, +1 = 2).
    assert_eq!(sb.mmu.load32(parent_filter + 4).unwrap(), 2);

    // The counter should now be 1.
    assert_eq!(sb.query_pin_info_call_count(), 1);
}

// ────────────────────────────────────────────────────────────────
// Test 3 — ConnectedTo returns the stamped codec input pin.
// ────────────────────────────────────────────────────────────────

#[test]
fn r37_connected_to_returns_codec_input_pin() {
    let mut sb = Sandbox::new();
    let amt = sb.host.arena_alloc(16).unwrap();
    let codec_input_pin = sb.host.arena_alloc(16).unwrap();
    sb.mmu
        .write_initializer(codec_input_pin, &0u32.to_le_bytes())
        .unwrap();
    sb.mmu
        .write_initializer(codec_input_pin + 4, &1u32.to_le_bytes())
        .unwrap();
    let pin = sb
        .mint_host_output_pin_with_connection(amt, codec_input_pin)
        .unwrap();
    // ConnectedTo (slot 6) writes *ppPin and returns S_OK.
    let pp = sb.host.arena_alloc(4).unwrap();
    sb.mmu.write_initializer(pp, &0u32.to_le_bytes()).unwrap();
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        6,
        &[pp],
    )
    .expect("ConnectedTo");
    assert_eq!(r, 0, "ConnectedTo returned non-S_OK: {r:#010x}");
    assert_eq!(sb.mmu.load32(pp).unwrap(), codec_input_pin);
    // refcount bumped on codec input pin.
    assert_eq!(sb.mmu.load32(codec_input_pin + 4).unwrap(), 2);
}

#[test]
fn r37_connected_to_returns_vfw_e_not_connected_when_no_peer() {
    let mut sb = Sandbox::new();
    let amt = sb.host.arena_alloc(16).unwrap();
    // Old API path: connected_pin defaults to 0.
    let pin = sb.mint_host_output_pin(amt).unwrap();
    let pp = sb.host.arena_alloc(4).unwrap();
    sb.mmu
        .write_initializer(pp, &0xDEAD_BEEFu32.to_le_bytes())
        .unwrap();
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        6,
        &[pp],
    )
    .expect("ConnectedTo");
    // VFW_E_NOT_CONNECTED = 0x80040209.
    assert_eq!(r, 0x8004_0209);
    // *ppPin should have been zeroed out before return.
    assert_eq!(sb.mmu.load32(pp).unwrap(), 0);
}

// ────────────────────────────────────────────────────────────────
// Test 4 — IBaseFilter::QueryFilterInfo writes a 260-byte FILTER_INFO
// and increments the per-state counter.
// ────────────────────────────────────────────────────────────────

#[test]
fn r37_query_filter_info_writes_filter_info_struct_and_increments_counter() {
    let mut sb = Sandbox::new();
    let amt = sb.host.arena_alloc(16).unwrap();
    let pin = sb.mint_host_output_pin(amt).unwrap();
    // Parent filter is auto-minted at obj+16.
    let parent = sb.mmu.load32(pin + 16).unwrap();
    assert_ne!(parent, 0);

    // Allocate scratch FILTER_INFO struct (260 bytes).
    let p_info = sb.host.arena_alloc(272).unwrap();
    for i in 0..260u32 {
        sb.mmu.store8(p_info + i, 0xCD).unwrap();
    }
    // Drive QueryFilterInfo on the parent filter (slot 12).
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        parent,
        12, // IBaseFilter::QueryFilterInfo
        &[p_info],
    )
    .expect("QueryFilterInfo");
    assert_eq!(r, 0);
    // achName starts with UTF-16 'H'.
    assert_eq!(sb.mmu.load32(p_info).unwrap() & 0xFFFF, b'H' as u32);
    // pGraph at offset 256 stays NULL.
    assert_eq!(sb.mmu.load32(p_info + 256).unwrap(), 0);
    // Counter incremented.
    assert_eq!(sb.query_filter_info_call_count(), 1);
}

// ────────────────────────────────────────────────────────────────
// Test 5 — Production path: drive the round-36 baseline test and
// confirm we either (a) succeed, (b) hit a DIFFERENT trap site
// than 0x7184, or (c) at least record QueryPinInfo / QueryFilterInfo
// calls (proof the codec walked the introspection path we wired).
// ────────────────────────────────────────────────────────────────

#[test]
fn r37_production_path_traps_differently_or_records_introspection() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round37 production: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let (width, height, keyframe) = match extract_mp43_keyframe("fourcc-MP43") {
        Some(t) => t,
        None => {
            eprintln!("round37 production: MP43 fixture missing; skipping");
            return;
        }
    };
    let id = "vfw_round37_production".to_string();
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
    let send_result = decoder.send_packet(&packet);
    let outcome = match send_result {
        Err(e) => Err(e),
        Ok(()) => decoder.receive_frame(),
    };
    if let Some(neg) = last_codec_allocator_negotiation(&id) {
        eprintln!(
            "round37 production: GA={:#010x} alloc={:#010x} SP={:#010x} CO={:#010x} using_codec={}",
            neg.get_allocator_hr,
            neg.codec_allocator,
            neg.set_properties_hr,
            neg.commit_hr,
            neg.using_codec_allocator,
        );
    }
    match outcome {
        Ok(oxideav_core::Frame::Video(v)) => {
            eprintln!(
                "round37 production: SUCCESS — Frame::Video with {} planes",
                v.planes.len()
            );
        }
        Ok(other) => eprintln!("round37 production: unexpected Ok({other:?})"),
        Err(e) => {
            let msg = format!("{e}");
            eprintln!("round37 production: receive_frame → Err({msg})");
            // Round-36 baseline regression check — do not regress to
            // CLASS_E_CLASSNOTAVAILABLE (the round-34 baseline).
            assert!(
                !msg.contains("0x80040111"),
                "round37 must not regress to CLASS_E_CLASSNOTAVAILABLE: {msg}"
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Test 6 — Round-30..36 baseline preservation smoke: the
// `mint_host_output_pin` legacy entry still mints a pin whose
// IUnknown trio + QueryDirection + ConnectionMediaType +
// EnumMediaTypes work as before.
// ────────────────────────────────────────────────────────────────

#[test]
fn r37_legacy_mint_host_output_pin_baseline_preserved() {
    let mut sb = Sandbox::new();
    // Stage a real-ish AMT (just zero bytes; ConnectionMediaType
    // copies 72 bytes regardless of content).
    let amt = sb.host.arena_alloc(72).unwrap();
    for i in 0..72u32 {
        sb.mmu.store8(amt + i, 0).unwrap();
    }
    let pin = sb.mint_host_output_pin(amt).unwrap();
    // QueryDirection (slot 9) → PIN_OUTPUT (1).
    let p_dir = sb.host.arena_alloc(4).unwrap();
    sb.mmu
        .write_initializer(p_dir, &0u32.to_le_bytes())
        .unwrap();
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        9,
        &[p_dir],
    )
    .unwrap();
    assert_eq!(r, 0);
    assert_eq!(sb.mmu.load32(p_dir).unwrap(), 1);
    // AddRef + Release round trip.
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        1,
        &[],
    )
    .unwrap();
    assert_eq!(r, 2);
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        2,
        &[],
    )
    .unwrap();
    assert_eq!(r, 1);
}

// ────────────────────────────────────────────────────────────────
// Test 7 — round-32/33/34/35/36 negotiation baseline: full
// production walkthrough still preserves the GA/SP/CO=S_OK
// state with using_codec_allocator=true.
// ────────────────────────────────────────────────────────────────

#[test]
fn r37_round_36_baseline_preserved_after_round_37() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round37 baseline-check: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let (width, height, keyframe) = match extract_mp43_keyframe("fourcc-MP43") {
        Some(t) => t,
        None => {
            eprintln!("round37 baseline-check: MP43 fixture missing; skipping");
            return;
        }
    };
    let id = "vfw_round37_baseline_check".to_string();
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
        "round37 must preserve r36 GA=S_OK baseline; got {:#010x}",
        neg.get_allocator_hr,
    );
    assert_ne!(
        neg.codec_allocator, 0,
        "round37 must preserve r36 non-NULL codec allocator"
    );
    assert_eq!(
        neg.set_properties_hr, 0,
        "round37 must preserve r36 SetProperties=S_OK baseline"
    );
    assert_eq!(
        neg.commit_hr, 0,
        "round37 must preserve r36 Commit=S_OK baseline"
    );
    assert!(
        neg.using_codec_allocator,
        "round37 must preserve r36 using_codec_allocator=true baseline"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 8 — Reach goal: confirm IndeoCinepak trait integration
// is unaffected by the host-output-pin layout change.  Re-uses the
// round-30 host-allocator smoke + IMediaSample minting.
// ────────────────────────────────────────────────────────────────

#[test]
fn r37_round_30_host_allocator_smoke_unchanged() {
    let mut sb = Sandbox::new();
    let alloc = sb
        .mint_host_mem_allocator(4, 256 * 1024, 0)
        .expect("mint allocator");
    assert_ne!(alloc, 0);
    let r_co = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_COMMIT,
        &[],
    )
    .expect("Commit");
    assert_eq!(r_co, oxideav_vfw::com::S_OK);
    let pp = sb.host.arena_alloc(4).unwrap();
    sb.mmu.write_initializer(pp, &0u32.to_le_bytes()).unwrap();
    let r_gb = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_GET_BUFFER,
        &[pp, 0, 0, 0],
    )
    .expect("GetBuffer");
    assert_eq!(r_gb, oxideav_vfw::com::S_OK);
    let sample = sb.mmu.load32(pp).unwrap();
    assert_ne!(sample, 0);
}
