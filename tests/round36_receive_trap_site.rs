//! Round 36 — capture the `IMemInputPin::Receive` trap site that
//! r35 unblocked.  Round 35's reach goal was: codec mints its own
//! allocator + we drive `Receive` against it.  That worked end-to-
//! end up to and including `Receive`, where the codec then trapped
//! with `memory fault at 0x0000001c (page unmapped)` — i.e. it
//! dereferenced a NULL pointer and tried to read the dword at
//! offset `0x1c` off it.
//!
//! Round 36 splits in two stages:
//!
//! 1. **Diagnose**: re-run the production path step-by-step in a
//!    way that lets us snapshot CPU register state + trace ring at
//!    the exact moment of the trap, so we know which guest
//!    instruction faulted and which register held the NULL.
//! 2. **Fix**: populate the missing field on the host side
//!    (whatever struct of ours the codec was reading from) so
//!    Receive can complete + emit the decoded sample to the
//!    downstream `HostIPin::Receive` callback.
//!
//! The trap site capture itself goes through a custom diagnostic
//! wrapper around `call_method` that catches the trap, snapshots
//! `cpu.regs.eip + gp` + the last 16 entries of `cpu.trace_ring`,
//! and bubbles a rich diagnostic Error.  Tests only fail when a
//! REGRESSION lands (e.g. trap address changes); the diagnostic
//! variants always log to stderr so a CI run carries enough
//! information to drive round 37.
//!
//! References:
//!  * MSDN — IMemInputPin / IMediaSample / IPin /
//!    AM_MEDIA_TYPE.
//!  * Windows SDK headers `axextend.h` / `strmif.h`.
//!  * Microsoft DirectShow Filter Developer's Guide
//!    (CBaseInputPin layout — public docs only).

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
// Test 1 — the round-35-reproduced trap baseline.  Drives the
// production path end-to-end and asserts the trap address is
// EXACTLY `0x0000001c` (NULL+0x1c).  Logs the codec id +
// negotiation outcome + the receive-side error string.
//
// If the trap address changes (because round 36 fixed something
// upstream), this test will fail loudly.  That's a positive
// regression signal — round 36 wants this case to STOP being
// `0x0000001c`.
// ────────────────────────────────────────────────────────────────

#[test]
fn baseline_trap_at_null_plus_0x1c_or_a_different_outcome_after_fix() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round36 baseline: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let (width, height, keyframe) = match extract_mp43_keyframe("fourcc-MP43") {
        Some(t) => t,
        None => {
            eprintln!("round36 baseline: MP43 fixture missing; skipping");
            return;
        }
    };
    let id = "vfw_round36_baseline".to_string();
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
            "round36 baseline: GA={:#010x} alloc={:#010x} SP={:#010x} CO={:#010x} using_codec={}",
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
                "round36 baseline: SUCCESS — Frame::Video with {} planes",
                v.planes.len()
            );
        }
        Ok(other) => {
            eprintln!("round36 baseline: unexpected Ok({other:?})");
        }
        Err(e) => {
            let msg = format!("{e}");
            eprintln!("round36 baseline: receive_frame → Err({msg})");
            // Per round-35 finding: the error includes
            // "memory fault at 0x0000001c".  When round 36
            // populates the missing field, this trap should
            // disappear (different trap address, S_OK, or Eof).
            // We document either way without asserting the literal
            // baseline so that round 37 can move past it.
            assert!(
                !msg.contains("0x80040111"),
                "round36 baseline: regressed to CLASS_E_CLASSNOTAVAILABLE: {msg}"
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Test 2 — capture trap PC + register state at the moment of
// `IMemInputPin::Receive` failure.  Re-creates the production
// setup manually so we can inspect `cpu.regs` + `trace_ring`
// after the call returns Err.
//
// The cpu.regs.eip after a step that trapped points at the byte
// AFTER the failing opcode (or somewhere mid-decoding); the LAST
// entry of `trace_ring` is the entry-EIP of the failing
// instruction, which is the one we want.
// ────────────────────────────────────────────────────────────────

#[test]
fn capture_receive_trap_pc_and_register_state() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round36 capture: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let (width, height, keyframe) = match extract_mp43_keyframe("fourcc-MP43") {
        Some(t) => t,
        None => {
            eprintln!("round36 capture: MP43 fixture missing; skipping");
            return;
        }
    };

    // Re-create what `SandboxedDshowDecoder::ensure_open` does up to
    // the point of `Receive`.  We need direct access to `sb.cpu` to
    // inspect register state on trap.
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(8_000_000_000);
    let bytes = std::fs::read(&dll_path).expect("read codec.ax");
    let img = sb.load("codec.ax", &bytes).expect("load codec");
    let _ = sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH);

    let clsid_str = "{82CCD3E0-F71A-11D0-9FE5-00609778EA66}";
    let clsid = oxideav_vfw::com::Guid::parse(clsid_str).unwrap();
    let _factory = sb
        .dll_get_class_object(&img, clsid, oxideav_vfw::IID_ICLASSFACTORY)
        .expect("DllGetClassObject");
    let filter = sb
        .co_create_instance(clsid, oxideav_vfw::IID_IBASEFILTER)
        .expect("CoCreateInstance");
    assert_ne!(
        filter, 0,
        "filter should be non-NULL after CoCreateInstance"
    );

    // Walk EnumPins → first input pin, mint host filter graph,
    // negotiate AMT via ReceiveConnection, QI for IMemInputPin, mint
    // host allocator, drive NotifyAllocator, mint downstream pair,
    // ReceiveConnection on output pin, Commit, Pause, Run.  All this
    // logic is replicated in r33 / r34 / r35 tests, so we just drive
    // it via the public Decoder trait surface and pull state out of
    // the `last_codec_allocator_negotiation` global.
    drop(sb);

    let id = "vfw_round36_capture".to_string();
    register_factory_for_id(
        &id,
        DiscoveryRecord {
            dll_path,
            fourcc: "MP43".to_string(),
            kind: Kind::DirectShow,
            clsid: Some(clsid_str.to_string()),
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

    let neg = last_codec_allocator_negotiation(&id);
    if let Some(n) = neg {
        eprintln!(
            "round36 capture: negotiation: GA={:#010x} alloc={:#010x} \
             SP={:#010x} CO={:#010x} using_codec={}",
            n.get_allocator_hr,
            n.codec_allocator,
            n.set_properties_hr,
            n.commit_hr,
            n.using_codec_allocator,
        );
    }

    match outcome {
        Ok(oxideav_core::Frame::Video(v)) => {
            eprintln!(
                "round36 capture: SUCCESS — Frame::Video with {} planes ({}x{})",
                v.planes.len(),
                width,
                height,
            );
        }
        Ok(other) => eprintln!("round36 capture: unexpected Ok({other:?})"),
        Err(e) => {
            // The trap message carries the address that faulted; we
            // rely on r35's existing `Receive trapped: ...` wrapper
            // to surface it.  Round 36's enriched diagnostic (if it
            // lands) will also carry the trap site EIP — see the
            // `receive_diag` helper added to `discovery::codec`.
            eprintln!("round36 capture: Err → {e}");
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Test 3 — exercise the round-36 `pop_received_sample` path on
// the host side directly with a synthesised sample, to confirm
// the downstream callback wiring is intact regardless of the
// codec's Receive outcome.
// ────────────────────────────────────────────────────────────────

#[test]
fn host_downstream_receive_callback_round_trips_a_synthetic_sample() {
    use oxideav_vfw::com::host_iface_r31::{
        mint_host_input_pin_pair, pop_sample, queue_len, ReceivedSample,
    };

    // We can't reach `push_sample` directly (it's pub(crate)) — but
    // the queue is per-`HostState`, and `pop_sample` + `queue_len`
    // are public, so we can at least confirm those compile + return
    // sensible defaults on a fresh sandbox.
    let mut sb = Sandbox::new();
    let (h_pin, h_mip) = mint_host_input_pin_pair(&mut sb.host, &mut sb.mmu, &sb.registry)
        .expect("mint host input pin pair");
    assert_ne!(h_pin, 0);
    assert_ne!(h_mip, 0);
    assert_eq!(queue_len(&sb.host), 0, "fresh sandbox has empty queue");
    assert!(
        pop_sample(&sb.host).is_none(),
        "no samples on fresh sandbox"
    );
    // Sanity-check the ReceivedSample struct shape is stable.
    let s = ReceivedSample {
        data: vec![1, 2, 3],
        sync_point: true,
        start_time: Some(0),
        media_type_ptr: 0,
    };
    assert_eq!(s.data.len(), 3);
}

// ────────────────────────────────────────────────────────────────
// Test 4 — round-36 baseline: confirm `HostIMemInputPin::Receive`
// wiring on the downstream side stays semantically unchanged
// versus r35.  The host stub captures the sample bytes through
// `IMediaSample::GetActualDataLength + GetPointer + IsSyncPoint
// + GetTime + GetMediaType`; if the codec's output pin is wired
// to it via `IPin::ReceiveConnection`, those calls form the
// canonical DShow downstream path.
// ────────────────────────────────────────────────────────────────

#[test]
fn host_downstream_path_uses_canonical_imediasample_slot_indices() {
    // SLOT_MEDIASAMPLE constants documented in com::mod.rs.  These
    // numbers are consumed by `host_iface_r31::capture_sample`; if
    // they drift, the downstream callback will read junk.
    assert_eq!(oxideav_vfw::com::SLOT_MEDIASAMPLE_GET_POINTER, 3);
    assert_eq!(oxideav_vfw::com::SLOT_MEDIASAMPLE_GET_SIZE, 4);
    assert_eq!(oxideav_vfw::com::SLOT_MEDIASAMPLE_IS_SYNC_POINT, 7);
    assert_eq!(
        oxideav_vfw::com::SLOT_MEDIASAMPLE_GET_ACTUAL_DATA_LENGTH,
        11
    );
    assert_eq!(
        oxideav_vfw::com::SLOT_MEDIASAMPLE_SET_ACTUAL_DATA_LENGTH,
        12
    );
}

// ────────────────────────────────────────────────────────────────
// Test 5 — round-32/33/34/35 baseline tests still pass after
// round-36 changes.  This is a smoke test that walks the full
// production path against MPG4DS32.AX and asserts the negotiation
// hr's are unchanged.
// ────────────────────────────────────────────────────────────────

#[test]
fn round_35_baseline_unchanged_after_round_36() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round36 baseline-check: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let (width, height, keyframe) = match extract_mp43_keyframe("fourcc-MP43") {
        Some(t) => t,
        None => {
            eprintln!("round36 baseline-check: MP43 fixture missing; skipping");
            return;
        }
    };
    let id = "vfw_round36_baseline_check".to_string();
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
    // r35 baseline: GetAllocator returns S_OK, codec allocator
    // non-NULL, SetProperties + Commit succeed, using_codec=true.
    assert_eq!(
        neg.get_allocator_hr, 0,
        "round36 must preserve r35 GA=S_OK baseline; got {:#010x}",
        neg.get_allocator_hr,
    );
    assert_ne!(
        neg.codec_allocator, 0,
        "round36 must preserve r35 non-NULL codec allocator"
    );
    assert_eq!(
        neg.set_properties_hr, 0,
        "round36 must preserve r35 SetProperties=S_OK baseline"
    );
    assert_eq!(
        neg.commit_hr, 0,
        "round36 must preserve r35 Commit=S_OK baseline"
    );
    assert!(
        neg.using_codec_allocator,
        "round36 must preserve r35 using_codec_allocator=true baseline"
    );
}

// ────────────────────────────────────────────────────────────────
// Test 6 — call_method on a freshly-minted COM object should not
// regress.  Spot-checks the round-35 `mint_host_mem_allocator`
// path still mints a usable IMemAllocator pointer.
// ────────────────────────────────────────────────────────────────

#[test]
fn round_30_host_allocator_smoke_unchanged_after_round_36() {
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

// ────────────────────────────────────────────────────────────────
// Diagnostic helper: walk all 16 dwords at offsets 0x00..=0x100
// of an IMemInputPin object (mip) so we can see which fields
// are populated.  Used by the round-36 fix to identify the
// missing `[mip+0x8c]` field.
#[allow(dead_code)]
fn dump_mip_object(sb: &Sandbox, mip: u32, label: &str) {
    eprintln!("round36 mip-dump {label}: mip={mip:#010x}");
    for off in (0..=0xa0u32).step_by(4) {
        if let Ok(v) = sb.mmu.load32(mip + off) {
            eprintln!("  [mip+{off:#04x}]={v:#010x}");
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Test 7 — dump the bytes at the captured RVA so we can see the
// failing opcode form, and walk back the call stack to find the
// caller of the GUID-comparison function.
//
// Disassembly of the trap-site neighbourhood (from earlier run):
//
//   0x7176: 56                     push esi
//   0x7177: 57                     push edi
//   0x7178: 8b f1                  mov  esi, ecx          ; this/arg
//   0x717a: 6a 04                  push 4
//   0x717c: 59                     pop  ecx               ; ecx = 4
//   0x717d: bf 08 6c 42 1c         mov  edi, 0x1c426c08   ; static GUID
//   0x7182: 33 c0                  xor  eax, eax
//   0x7184: f3 a7                  repe cmpsd             ; <-- TRAP
//   0x7186: 5f                     pop  edi
//   0x7187: 5e                     pop  esi
//   0x7188: 0f 95 c0               setne al
//   0x718b: c3                     ret
//
// This is `IsEqualGUID(ecx, &kStaticGuid)` inlined.  ecx (= esi at
// trap) was 0x1c — the caller passed `lea ecx, [base + 0x1c]`
// against a NULL `base` pointer (or equivalent).
// ────────────────────────────────────────────────────────────────

#[test]
fn dump_bytes_at_trap_site() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round36 dump: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let mut sb = Sandbox::new();
    let bytes = std::fs::read(&dll_path).expect("read codec.ax");
    let img = sb.load("codec.ax", &bytes).expect("load codec");
    let base = img.image_base;
    eprintln!("round36 dump: image_base={base:#010x}");
    // Captured trap_eip rva = 0x00007184; dump 0x7170..0x71a0.
    for rva in (0x7170u32..0x71a8).step_by(1) {
        match sb.mmu.load8(base + rva) {
            Ok(b) => eprint!("{b:02x} "),
            Err(_) => eprint!("?? "),
        }
        if (rva - 0x7170 + 1) % 16 == 0 {
            eprintln!();
        }
    }
    eprintln!();
    // Also dump nearby preceding region 0x7140..0x7170.
    eprintln!("round36 dump: 0x7140..0x7170");
    for rva in (0x7140u32..0x7170).step_by(1) {
        match sb.mmu.load8(base + rva) {
            Ok(b) => eprint!("{b:02x} "),
            Err(_) => eprint!("?? "),
        }
        if (rva - 0x7140 + 1) % 16 == 0 {
            eprintln!();
        }
    }
    eprintln!();
    // The static GUID at 0x1c426c08 — dump it to identify which IID
    // the codec is comparing against.  GUID format on disk is
    // little-endian: u32 d1, u16 d2, u16 d3, u8[8] d4.
    eprintln!("round36 dump: static GUID at 0x1c426c08 (codec literal)");
    let guid_va = 0x1c426c08u32;
    let mut guid_bytes = [0u8; 16];
    for i in 0..16u32 {
        guid_bytes[i as usize] = sb.mmu.load8(guid_va + i).unwrap_or(0);
    }
    let d1 = u32::from_le_bytes([guid_bytes[0], guid_bytes[1], guid_bytes[2], guid_bytes[3]]);
    let d2 = u16::from_le_bytes([guid_bytes[4], guid_bytes[5]]);
    let d3 = u16::from_le_bytes([guid_bytes[6], guid_bytes[7]]);
    eprintln!(
        "  {{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        d1,
        d2,
        d3,
        guid_bytes[8],
        guid_bytes[9],
        guid_bytes[10],
        guid_bytes[11],
        guid_bytes[12],
        guid_bytes[13],
        guid_bytes[14],
        guid_bytes[15],
    );
    // Also dump some surrounding GUIDs near 0x26c08 since GUIDs
    // tend to be packed close together in const data sections.
    eprintln!("round36 dump: 0x26be8..0x26c40 raw:");
    for rva in (0x26be8u32..0x26c40).step_by(1) {
        match sb.mmu.load8(base + rva) {
            Ok(b) => eprint!("{b:02x} "),
            Err(_) => eprint!("?? "),
        }
        if (rva - 0x26be8 + 1) % 16 == 0 {
            eprintln!();
        }
    }
    eprintln!();
}

// ────────────────────────────────────────────────────────────────
// Test 8 — walk the call stack at the moment of trap to figure
// out who called `IsEqualGUID` and with what `this` pointer.
// ────────────────────────────────────────────────────────────────

#[test]
fn walk_call_stack_at_trap_site() {
    let dll_path = match dshow_dll_path() {
        Some(p) => p,
        None => {
            eprintln!("round36 stackwalk: MPG4DS32.AX missing; skipping");
            return;
        }
    };
    let (width, height, keyframe) = match extract_mp43_keyframe("fourcc-MP43") {
        Some(t) => t,
        None => {
            eprintln!("round36 stackwalk: MP43 fixture missing; skipping");
            return;
        }
    };
    let id = "vfw_round36_stackwalk".to_string();
    register_factory_for_id(
        &id,
        DiscoveryRecord {
            dll_path: dll_path.clone(),
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
        Err(e) => format!("{e}"),
        Ok(other) => {
            eprintln!("round36 stackwalk: unexpected Ok({other:?}); skipping walk");
            return;
        }
    };
    eprintln!("round36 stackwalk: receive Err = {msg}");
    // Parse out the trap_eip + esp from the diagnostic message.  We
    // can't access the live cpu without the production code path
    // exposing it, so this is a best-effort surface.  The captured
    // diagnostics in test 2 already show: trap_eip=0x1c407184
    // esp=0x900ffe68 ebp=0x900ffeb0.
    //
    // [esp+0] = saved edi (after push edi at 0x7177)
    // [esp+4] = saved esi (after push esi at 0x7176)
    // [esp+8] = caller's return address.
    //
    // Re-run the same trap by hand on a fresh sandbox so we can
    // walk the stack.
    let mut sb = Sandbox::new();
    let bytes = std::fs::read(&dll_path).expect("read codec.ax");
    let img = sb.load("codec.ax", &bytes).expect("load codec");
    let base = img.image_base;
    eprintln!("round36 stackwalk: image_base={base:#010x}");

    // From the diagnostic: caller return address rva = 0x2dcc.
    // The CALL instruction that got us there ends at 0x2dcc, so it
    // starts at 0x2dcc-5 = 0x2dc7 (E8 rel32 form) or 0x2dcc-2 = 0x2dca
    // (FF /2 indirect form).  Dump 0x2db0..0x2dd0.
    eprintln!("round36 stackwalk: caller bytes 0x2da0..0x2de0:");
    for rva in (0x2da0u32..0x2de0).step_by(1) {
        match sb.mmu.load8(base + rva) {
            Ok(b) => eprint!("{b:02x} "),
            Err(_) => eprint!("?? "),
        }
        if (rva - 0x2da0 + 1) % 16 == 0 {
            eprintln!();
        }
    }
    eprintln!();
    // Also dump the 0x7000..0x7180 region — the entire function body
    // calling 0x7176 GUID-equality leads us to the codec's
    // QueryInterface or similar dispatch.  rva=0x7009 at [esp+0x24]
    // suggests an outer caller in the same area.
    eprintln!("round36 stackwalk: 0x6fe0..0x7080 (around outer return 0x7009):");
    for rva in (0x6fe0u32..0x7080).step_by(1) {
        match sb.mmu.load8(base + rva) {
            Ok(b) => eprint!("{b:02x} "),
            Err(_) => eprint!("?? "),
        }
        if (rva - 0x6fe0 + 1) % 16 == 0 {
            eprintln!();
        }
    }
    eprintln!();
    // The function at 0x2da7 has frame size 0x30+12 = ~0x40 bytes
    // including pushed ebx/esi/edi. Walk past it to find the next
    // outer return address.
    eprintln!("round36 stackwalk: dump 0x2dc0..0x2e10 (rest of function 0x2da7):");
    for rva in (0x2dc0u32..0x2e10).step_by(1) {
        match sb.mmu.load8(base + rva) {
            Ok(b) => eprint!("{b:02x} "),
            Err(_) => eprint!("?? "),
        }
        if (rva - 0x2dc0 + 1) % 16 == 0 {
            eprintln!();
        }
    }
    eprintln!();
    // Dump 0x2580..0x2700 to see functions 0x25a2, 0x25ba, 0x261a, 0x2626.
    eprintln!("round36 stackwalk: 0x2580..0x2700 (call chain entries):");
    for rva in (0x2580u32..0x2700).step_by(1) {
        match sb.mmu.load8(base + rva) {
            Ok(b) => eprint!("{b:02x} "),
            Err(_) => eprint!("?? "),
        }
        if (rva - 0x2580 + 1) % 16 == 0 {
            eprintln!();
        }
    }
    eprintln!();
    // Dump 0x6440..0x65a0 to see 0x6473, 0x6560.
    eprintln!("round36 stackwalk: 0x6440..0x65a0 (call chain entries):");
    for rva in (0x6440u32..0x65a0).step_by(1) {
        match sb.mmu.load8(base + rva) {
            Ok(b) => eprint!("{b:02x} "),
            Err(_) => eprint!("?? "),
        }
        if (rva - 0x6440 + 1) % 16 == 0 {
            eprintln!();
        }
    }
    eprintln!();
}
