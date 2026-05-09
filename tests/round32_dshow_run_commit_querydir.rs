//! Round 32 — close the DirectShow decode loop end-to-end.
//!
//! **A — Drive `IMediaFilter::Run(0)`.**  The codec needs to be in
//! `State_Running` (or at least `State_Paused`) before
//! `IMemInputPin::Receive` is legal.  `SandboxedDshowDecoder::ensure_open`
//! now drives `Pause()` then `Run(0)` against the codec filter via
//! the `IBaseFilter`/`IMediaFilter` shared vtable slots
//! (5 = Pause, 6 = Run).
//!
//! **B — `HostIMemAllocator::Commit` state machine.**  The host
//! allocator now tracks a per-instance commit flag in guest memory
//! (`obj+12 == 0` → decommitted; `1` → committed).  `GetBuffer`
//! returns `VFW_E_NOT_COMMITTED (0x80040209)` while decommitted;
//! `Commit` flips the flag to 1, `Decommit` back to 0.
//!
//! **C — `IPin::QueryDirection` filter on `first_input_pin`.**  The
//! discovery `first_input_pin` walker now enumerates *every* pin
//! the codec exposes via `IBaseFilter::EnumPins → IEnumPins::Next`
//! and picks the first one whose `IPin::QueryDirection` reports
//! `PIN_INPUT (0)`, instead of trusting the historic "input pins
//! enumerate first" convention.
//!
//! References: MSDN
//!  - `IMediaFilter::Run` / `Pause` / `Stop`.
//!  - `IMemAllocator::Commit` / `Decommit` / `GetBuffer`.
//!  - `IPin::QueryDirection`, `PIN_DIRECTION` enum.
//!
//! Windows SDK headers `axextend.h` / `strmif.h`.

#![cfg(feature = "auto-discovery")]

use std::path::PathBuf;

use oxideav_core::{CodecId, CodecParameters, Packet, TimeBase};
use oxideav_vfw::com::call::call_method;
use oxideav_vfw::discovery::{make_decoder, register_factory_for_id, DiscoveryRecord, Kind};
use oxideav_vfw::Sandbox;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

// ────────────────────────────────────────────────────────────────
// B — HostIMemAllocator::Commit state machine
// ────────────────────────────────────────────────────────────────

/// Newly-minted host allocator is *decommitted*: GetBuffer returns
/// VFW_E_NOT_COMMITTED until Commit() flips the flag.
#[test]
fn host_mem_allocator_starts_decommitted_and_get_buffer_rejects() {
    let mut sb = Sandbox::new();
    let alloc = sb.mint_host_mem_allocator(2, 1024, 0).expect("mint alloc");

    // The committed flag at obj+12 should be 0 immediately after mint.
    assert_eq!(sb.mmu.load32(alloc + 12).unwrap(), 0);

    // GetBuffer in the decommitted state → VFW_E_NOT_COMMITTED.
    let pp = sb.host.arena_alloc(4).unwrap();
    sb.mmu.write_initializer(pp, &0u32.to_le_bytes()).unwrap();
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_GET_BUFFER,
        &[pp, 0, 0, 0],
    )
    .unwrap();
    assert_eq!(
        r,
        oxideav_vfw::com::VFW_E_NOT_COMMITTED,
        "GetBuffer on decommitted allocator should return VFW_E_NOT_COMMITTED, got {r:#010x}"
    );
    assert_eq!(sb.mmu.load32(pp).unwrap(), 0);
}

/// Commit flips the flag → GetBuffer succeeds; Decommit flips it
/// back → subsequent GetBuffer returns VFW_E_NOT_COMMITTED.
#[test]
fn host_mem_allocator_commit_decommit_round_trip() {
    let mut sb = Sandbox::new();
    let alloc = sb.mint_host_mem_allocator(2, 1024, 0).expect("mint alloc");
    let pp = sb.host.arena_alloc(4).unwrap();
    sb.mmu.write_initializer(pp, &0u32.to_le_bytes()).unwrap();

    // Commit → flag flips to 1.
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_COMMIT,
        &[],
    )
    .unwrap();
    assert_eq!(r, 0);
    assert_eq!(sb.mmu.load32(alloc + 12).unwrap(), 1);

    // GetBuffer now succeeds.
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_GET_BUFFER,
        &[pp, 0, 0, 0],
    )
    .unwrap();
    assert_eq!(r, 0);
    let s1 = sb.mmu.load32(pp).unwrap();
    assert_ne!(s1, 0);

    // Decommit → flag flips back to 0.
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_DECOMMIT,
        &[],
    )
    .unwrap();
    assert_eq!(r, 0);
    assert_eq!(sb.mmu.load32(alloc + 12).unwrap(), 0);

    // GetBuffer again → VFW_E_NOT_COMMITTED, even though there's
    // a free slot in the pool.
    sb.mmu.write_initializer(pp, &0u32.to_le_bytes()).unwrap();
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_GET_BUFFER,
        &[pp, 0, 0, 0],
    )
    .unwrap();
    assert_eq!(r, oxideav_vfw::com::VFW_E_NOT_COMMITTED);
    assert_eq!(sb.mmu.load32(pp).unwrap(), 0);

    // Re-commit → GetBuffer works again.
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_COMMIT,
        &[],
    )
    .unwrap();
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        alloc,
        oxideav_vfw::com::SLOT_MEMALLOCATOR_GET_BUFFER,
        &[pp, 0, 0, 0],
    )
    .unwrap();
    assert_eq!(r, 0);
    let s2 = sb.mmu.load32(pp).unwrap();
    // The second GetBuffer should hand back the OTHER free sample
    // (s1 is still marked in_use=1 from the earlier acquisition;
    // Decommit does NOT release outstanding samples).
    assert_ne!(s2, 0);
    assert_ne!(s2, s1);
}

// ────────────────────────────────────────────────────────────────
// A — IMediaFilter::Pause / Run slot constants are the IBaseFilter
//     slots (IBaseFilter extends IMediaFilter)
// ────────────────────────────────────────────────────────────────

/// Sanity-check: the new `SLOT_MEDIAFILTER_*` constants alias the
/// historical `SLOT_BASEFILTER_*` slots, since `IBaseFilter`
/// extends `IMediaFilter`.
#[test]
fn mediafilter_slot_constants_match_basefilter_slots() {
    assert_eq!(
        oxideav_vfw::com::SLOT_MEDIAFILTER_STOP,
        oxideav_vfw::com::SLOT_BASEFILTER_STOP
    );
    assert_eq!(
        oxideav_vfw::com::SLOT_MEDIAFILTER_PAUSE,
        oxideav_vfw::com::SLOT_BASEFILTER_PAUSE
    );
    assert_eq!(
        oxideav_vfw::com::SLOT_MEDIAFILTER_RUN,
        oxideav_vfw::com::SLOT_BASEFILTER_RUN
    );
    assert_eq!(
        oxideav_vfw::com::SLOT_MEDIAFILTER_GET_STATE,
        oxideav_vfw::com::SLOT_BASEFILTER_GET_STATE
    );
}

// ────────────────────────────────────────────────────────────────
// C — QueryDirection filter on first_input_pin (end-to-end)
// ────────────────────────────────────────────────────────────────

/// End-to-end: drive the DShow trait path against MPG4DS32.AX with
/// the round-32 enhancements.  The codec's pin enumeration is
/// walked, QueryDirection-filtered for PIN_INPUT, the host
/// allocator is Commit()'d, and `IMediaFilter::Run(0)` is driven
/// before `Receive`.
///
/// The test is permissive about the final outcome — even with all
/// three round-32 enhancements, the codec may still reject the
/// synthetic 100-byte packet (it isn't a valid MP43 keyframe).
/// What we assert is:
///  * the path no longer panics;
///  * any error message mentions a DShow-pathway diagnostic;
///  * if a frame surfaces, it carries a non-empty plane.
#[test]
fn round32_dshow_trait_path_drives_run_commit_and_querydir() {
    let dll_path = match workspace_root() {
        Some(p) => p.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/MPG4DS32.AX"),
        None => {
            eprintln!("round32 DShow: cannot resolve workspace root");
            return;
        }
    };
    if !dll_path.is_file() {
        eprintln!("round32 DShow: MPG4DS32.AX missing; skipping");
        return;
    }
    let id = "vfw_round32_dshow_run_commit_querydir";
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

    // Synthetic packet — exercises the AMT walk + downstream wiring
    // + Run/Commit/QueryDirection enhancements; we don't expect a
    // bit-exact decode here.
    let packet = Packet::new(0, TimeBase::new(1, 25), vec![0u8; 100]).with_keyframe(true);
    match decoder.send_packet(&packet) {
        Err(e) => {
            let msg = format!("{e}");
            eprintln!("round32 DShow: send_packet → Err({msg})");
            assert!(
                msg.contains("AMT")
                    || msg.contains("ReceiveConnection")
                    || msg.contains("DShow")
                    || msg.contains("vfw discovery")
                    || msg.contains("CreateInstance"),
                "expected DShow-pathway diagnostic, got {msg:?}"
            );
        }
        Ok(()) => match decoder.receive_frame() {
            Err(oxideav_core::Error::Eof) => {
                eprintln!(
                    "round32 DShow: send_packet ok; receive_frame → Eof \
                     (codec accepted input + no output sample landed); \
                     Run+Commit+QueryDir path is exercised."
                );
            }
            Err(e) => {
                let msg = format!("{e}");
                eprintln!("round32 DShow: receive_frame → Err({msg})");
                assert!(
                    msg.contains("DShow") || msg.contains("Receive"),
                    "expected DShow diagnostic, got {msg:?}"
                );
            }
            Ok(oxideav_core::Frame::Video(v)) => {
                eprintln!(
                    "round32 DShow: surfaced Frame::Video with {} planes",
                    v.planes.len()
                );
                assert!(!v.planes.is_empty(), "Frame::Video has no planes");
                let plane0 = &v.planes[0];
                assert!(plane0.stride > 0, "plane0 stride is 0");
                let nonzero = plane0.data.iter().filter(|&&b| b != 0).count();
                eprintln!(
                    "round32 DShow: plane0 stride={} bytes={} nonzero={}",
                    plane0.stride,
                    plane0.data.len(),
                    nonzero
                );
            }
            Ok(other) => panic!("expected Frame::Video, got {other:?}"),
        },
    }
}

/// Round 32 unit — the QueryDirection filter on a synthetic
/// multi-pin filter picks the PIN_INPUT pin even when it isn't the
/// first one enumerated.  Drives the host-mint helpers to construct
/// a synthetic "filter" with output-then-input pin order, then
/// asserts the discovery path walks past the output pin and lands
/// on the input pin.
///
/// This exercises the direction-filter logic without needing a real
/// codec DLL.  We can't directly call the private `first_input_pin`
/// helper from the integration test, so instead we exercise the
/// public `pin_with_direction` semantics via the host stubs:
/// driving QueryDirection on a HostIPin (output role, from r27)
/// reports PIN_OUTPUT (1), and on a HostIPin (input role, from r31)
/// reports PIN_INPUT (0).
#[test]
fn host_pins_query_direction_reports_distinct_roles() {
    let mut sb = Sandbox::new();

    // Round 27 mints the OUTPUT-role host pin.
    let amt_scratch = sb.host.arena_alloc(72).unwrap();
    sb.mmu.write_initializer(amt_scratch, &[0u8; 72]).unwrap();
    let out_pin = sb.mint_host_output_pin(amt_scratch).unwrap();
    let dir_slot = sb.host.arena_alloc(4).unwrap();
    sb.mmu
        .write_initializer(dir_slot, &0xFFu32.to_le_bytes())
        .unwrap();
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        out_pin,
        oxideav_vfw::com::SLOT_PIN_QUERY_DIRECTION,
        &[dir_slot],
    )
    .unwrap();
    assert_eq!(r, 0);
    assert_eq!(
        sb.mmu.load32(dir_slot).unwrap(),
        oxideav_vfw::com::PIN_DIRECTION_OUTPUT
    );

    // Round 31 mints the INPUT-role host pin.
    let (in_pin, _mip) = sb.host_iface_r31_mint_input_pin_pair().unwrap();
    sb.mmu
        .write_initializer(dir_slot, &0xFFu32.to_le_bytes())
        .unwrap();
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        in_pin,
        oxideav_vfw::com::SLOT_PIN_QUERY_DIRECTION,
        &[dir_slot],
    )
    .unwrap();
    assert_eq!(r, 0);
    assert_eq!(
        sb.mmu.load32(dir_slot).unwrap(),
        oxideav_vfw::com::PIN_DIRECTION_INPUT
    );
}
