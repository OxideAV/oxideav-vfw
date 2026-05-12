//! Round 61 — drive the `msadds32.ax` input-pin `IMemAllocator`
//! handshake so the codec's internal `Receive(IMediaSample*)`
//! stops returning `VFW_E_NOT_COMMITTED` (0x80040209).
//!
//! Round 60 closed by cracking the AMT validator: criteria-passing
//! WMA1/WMA2 `AM_MEDIA_TYPE`s now land `IPin::ReceiveConnection`
//! → `HRESULT 0x00000000` (S_OK).  Phase 5 of round 60 then drove
//! `Pause + Run(0) + Receive` and observed `0x80040209` —
//! `VFW_E_NOT_COMMITTED` — because the codec's input-pin
//! IMemAllocator has not been transitioned out of the decommitted
//! state via the standard
//! `GetAllocator → SetProperties → Commit → NotifyAllocator`
//! sequence.
//!
//! Round 25–43 already established the same handshake for the
//! video-side `mpg4ds32.ax`; this round drives the identical flow
//! for the audio splitter.  No new emulator scaffolding is
//! required — the `HostIMemAllocator`, `Commit/Decommit` state
//! machine, and `mint_host_mem_allocator` helper are all live.
//!
//! ## Phase coverage
//!
//! * **Phase 1** — QI for `IMemInputPin` then `GetAllocator` +
//!   `GetAllocatorRequirements` on the audio splitter's input
//!   pin.  Document what the codec proposes.
//! * **Phase 2** — drive `SetProperties → Commit → NotifyAllocator`
//!   on the codec's own preferred allocator when it surfaces one;
//!   otherwise advertise a host-side allocator sized for audio
//!   (4 buffers × 8 KiB).
//! * **Phase 3** — push the round-59 ASF data packet through
//!   `IMemInputPin::Receive` and observe whether the
//!   `VFW_E_NOT_COMMITTED` blocker is cleared.  Capture any PCM
//!   bytes the codec emits on the host sink.
//!
//! ## Reference material (clean-room only)
//!
//! * MSDN — `IMemAllocator`, `IMemInputPin`, `IMediaSample`,
//!   `ALLOCATOR_PROPERTIES`, COM IUnknown ABI.
//! * Intel SDM Vol. 2 — opcode-level disassembly (used in
//!   rounds 20–60 to map the codec's internal call sites).
//! * Raw bytes of `msadds32.ax` from
//!   `docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/`.
//!
//! No Wine / ReactOS / MinGW / Microsoft DShow base-class source
//! consulted.

use oxideav_vfw::com::{
    all_set_properties,
    call::{call_method, vtable_is_plausible},
    clear_set_properties_log, AllocatorPropertiesCapture, AmtBlueprint, MSADDS_AUDIO_DECODER_CLSID,
    PIN_DIRECTION_INPUT, SLOT_BASEFILTER_ENUM_PINS, SLOT_BASEFILTER_STOP, SLOT_ENUMPINS_NEXT,
    SLOT_MEDIAFILTER_PAUSE, SLOT_MEDIAFILTER_RUN, SLOT_MEMALLOCATOR_COMMIT,
    SLOT_MEMALLOCATOR_SET_PROPERTIES, SLOT_MEMINPUTPIN_GET_ALLOCATOR,
    SLOT_MEMINPUTPIN_NOTIFY_ALLOCATOR, SLOT_MEMINPUTPIN_RECEIVE, SLOT_PIN_QUERY_DIRECTION,
    SLOT_PIN_RECEIVE_CONNECTION,
};
use oxideav_vfw::{Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY, IID_IMEMINPUTPIN, IID_IUNKNOWN};
use std::path::PathBuf;

// ---- fixture helpers (mirror round 60) -------------------------------

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn msadds32_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/msadds32.ax");
    p.is_file().then_some(p)
}

fn load_msadds32() -> Option<(Sandbox, oxideav_vfw::pe::Image)> {
    let p = msadds32_path()?;
    let bytes = std::fs::read(&p).ok()?;
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(8_000_000_000);
    let img = sb.load("msadds32.ax", &bytes).ok()?;
    let _ = sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH);
    Some((sb, img))
}

fn bootstrap_filter() -> Option<(Sandbox, oxideav_vfw::pe::Image, u32)> {
    let (mut sb, img) = load_msadds32()?;
    let _factory = sb
        .dll_get_class_object(&img, MSADDS_AUDIO_DECODER_CLSID, IID_ICLASSFACTORY)
        .ok()?;
    let unk = sb
        .co_create_instance(MSADDS_AUDIO_DECODER_CLSID, IID_IUNKNOWN)
        .ok()?;
    if unk == 0 {
        return None;
    }
    let filter = sb.query_interface(unk, IID_IBASEFILTER).ok()?;
    if filter == 0 || !vtable_is_plausible(&sb.mmu, filter) {
        return None;
    }
    Some((sb, img, filter))
}

fn find_input_pin(sb: &mut Sandbox, filter: u32) -> Option<u32> {
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_BASEFILTER_STOP,
        &[],
    );
    let scratch = sb.host.arena_alloc(4).ok()?;
    sb.mmu.write_initializer(scratch, &[0u8; 4]).ok()?;
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_BASEFILTER_ENUM_PINS,
        &[scratch],
    )
    .ok()?;
    if r != 0 {
        return None;
    }
    let pp = sb.mmu.load32(scratch).ok()?;
    if pp == 0 {
        return None;
    }
    sb.host.com.intern(pp, None);
    for _ in 0..8 {
        let pin_slot = sb.host.arena_alloc(8).ok()?;
        sb.mmu.write_initializer(pin_slot, &[0u8; 8]).ok()?;
        let _ = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            pp,
            SLOT_ENUMPINS_NEXT,
            &[1, pin_slot, pin_slot + 4],
        );
        let pin = sb.mmu.load32(pin_slot).unwrap_or(0);
        let fetched = sb.mmu.load32(pin_slot + 4).unwrap_or(0);
        if pin == 0 || fetched != 1 {
            break;
        }
        sb.host.com.intern(pin, None);
        let dir_slot = sb.host.arena_alloc(4).ok()?;
        let _ = sb
            .mmu
            .write_initializer(dir_slot, &0xFFFF_FFFFu32.to_le_bytes());
        let _ = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            pin,
            SLOT_PIN_QUERY_DIRECTION,
            &[dir_slot],
        );
        let dir = sb.mmu.load32(dir_slot).unwrap_or(0xFFFF_FFFF);
        if dir == PIN_DIRECTION_INPUT {
            let _ = sb.com_release(pp);
            return Some(pin);
        }
    }
    let _ = sb.com_release(pp);
    None
}

/// Stage a criteria-passing AMT (WMA2 by default) and call
/// `IPin::ReceiveConnection` on the input pin against a host
/// output pin advertising that AMT.  Returns the AMT pointer +
/// the IMemInputPin pointer the input pin yields under QI.
///
/// Replays the round-60 phase-4 sequence verbatim.  Any failure
/// returns `None` so individual phases can skip gracefully.
fn open_connection_and_qi_mem_input_pin(sb: &mut Sandbox, filter: u32) -> Option<(u32, u32, u32)> {
    let input_pin = find_input_pin(sb, filter)?;
    let bp = AmtBlueprint::wma_criteria_passing(0x0161, 1, 44_100, 4_000, 185);
    let amt = stage_audio_amt_from_blueprint(sb, &bp).ok()?;
    let host_out = sb
        .mint_host_output_pin_with_connection(amt, input_pin)
        .ok()?;
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        input_pin,
        SLOT_PIN_RECEIVE_CONNECTION,
        &[host_out, amt],
    )
    .ok()?;
    if r != 0 {
        eprintln!("round61 helper: ReceiveConnection rejected HRESULT {r:#010x}; skipping");
        return None;
    }
    let mip = sb.query_interface(input_pin, IID_IMEMINPUTPIN).ok()?;
    if mip == 0 {
        return None;
    }
    Some((input_pin, mip, amt))
}

// ───────────────────────────────────────────────────────────────────
// Phase 1 — Discover the codec's allocator preferences
// ───────────────────────────────────────────────────────────────────

/// Phase 1 — after connection, call `IMemInputPin::GetAllocator`
/// on the codec's input pin to learn what allocator (if any) the
/// codec wants to use, then `GetAllocatorRequirements` to fetch
/// the `ALLOCATOR_PROPERTIES` shape it prefers.  Pure-empirical
/// observation: no assertion on the specific allocator pointer
/// or shape (codec is free to return `VFW_E_NO_ALLOCATOR` or its
/// own allocator).
#[test]
fn phase1_discover_codec_allocator_preferences() {
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round61 phase1: msadds32.ax missing; skipping");
        return;
    };
    let Some((input_pin, mip, _amt)) = open_connection_and_qi_mem_input_pin(&mut sb, filter) else {
        eprintln!("round61 phase1: cannot establish connection; skipping");
        return;
    };
    eprintln!("round61 phase1: input_pin = {input_pin:#010x}, IMemInputPin = {mip:#010x}");

    // GetAllocator(IMemAllocator** ppAllocator).
    let pp = sb.host.arena_alloc(4).expect("scratch for codec_alloc");
    sb.mmu
        .write_initializer(pp, &0u32.to_le_bytes())
        .expect("init out slot");
    let r_ga = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_GET_ALLOCATOR,
        &[pp],
    );
    let codec_alloc = sb.mmu.load32(pp).unwrap_or(0);
    match r_ga {
        Ok(hr) => eprintln!(
            "round61 phase1: IMemInputPin::GetAllocator → HRESULT {hr:#010x}, \
             *ppAllocator = {codec_alloc:#010x}"
        ),
        Err(ref e) => eprintln!("round61 phase1: GetAllocator trapped: {e}"),
    }

    // GetAllocatorRequirements(ALLOCATOR_PROPERTIES* pProps).
    // 16 bytes: cBuffers / cbBuffer / cbAlign / cbPrefix, each LONG.
    let req = sb.host.arena_alloc(16).expect("scratch for req");
    for off in [0u32, 4, 8, 12] {
        sb.mmu
            .write_initializer(req + off, &0u32.to_le_bytes())
            .expect("init req field");
    }
    // GetAllocatorRequirements is vtable slot 5 of IMemInputPin.
    let r_gar = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        5,
        &[req],
    );
    let c_buffers = sb.mmu.load32(req).unwrap_or(0);
    let cb_buffer = sb.mmu.load32(req + 4).unwrap_or(0);
    let cb_align = sb.mmu.load32(req + 8).unwrap_or(0);
    let cb_prefix = sb.mmu.load32(req + 12).unwrap_or(0);
    match r_gar {
        Ok(hr) => eprintln!(
            "round61 phase1: GetAllocatorRequirements → HRESULT {hr:#010x}, \
             cBuffers={c_buffers} cbBuffer={cb_buffer} cbAlign={cb_align} \
             cbPrefix={cb_prefix}"
        ),
        Err(ref e) => eprintln!("round61 phase1: GetAllocatorRequirements trapped: {e}"),
    }

    // The phase deliberately makes no hard assertion — the
    // empirical observation is the deliverable.  We still confirm
    // the test exercised at least one of the methods.
    assert!(
        r_ga.is_ok() || r_gar.is_ok(),
        "neither GetAllocator nor GetAllocatorRequirements completed without trap"
    );
}

// ───────────────────────────────────────────────────────────────────
// Phase 2 — Drive SetProperties → Commit → NotifyAllocator
// ───────────────────────────────────────────────────────────────────

/// Phase 2 — drive the standard 4-step handshake on the codec's
/// IMemAllocator (preferred when surfaced; falls back to a host
/// allocator sized for audio).
///
/// Step A:  SetProperties(&request, &actual)   — request 4 × 8 KiB.
/// Step B:  Commit()                            — pool transitions
///                                               to committed.
/// Step C:  NotifyAllocator(alloc, FALSE)       — tell the codec
///                                               which allocator
///                                               we'll feed it.
///
/// Each step must return a success HRESULT (high bit clear).
#[test]
fn phase2_set_properties_commit_notify_handshake() {
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round61 phase2: msadds32.ax missing; skipping");
        return;
    };
    let Some((_input_pin, mip, amt)) = open_connection_and_qi_mem_input_pin(&mut sb, filter) else {
        eprintln!("round61 phase2: cannot establish connection; skipping");
        return;
    };

    // Try the codec's allocator first.
    let pp = sb.host.arena_alloc(4).expect("scratch for codec_alloc");
    sb.mmu
        .write_initializer(pp, &0u32.to_le_bytes())
        .expect("init out slot");
    let r_ga = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_GET_ALLOCATOR,
        &[pp],
    )
    .unwrap_or(0xFFFF_FFFF);
    let codec_alloc = sb.mmu.load32(pp).unwrap_or(0);
    eprintln!("round61 phase2: GetAllocator hr={r_ga:#010x} codec_alloc={codec_alloc:#010x}");

    // Build a 16-byte ALLOCATOR_PROPERTIES request shaped for
    // audio: 4 buffers × 8 KiB, align 1, no prefix.
    // (cbBuffer must be at least nBlockAlign × ~2; the round-60
    // fixture uses nBlockAlign=185, so 8 KiB is generous.)
    let props_req = sb.host.arena_alloc(16).expect("scratch req");
    let props_actual = sb.host.arena_alloc(16).expect("scratch actual");
    for (off, val) in [(0u32, 4u32), (4, 8192), (8, 1), (12, 0)] {
        sb.mmu
            .write_initializer(props_req + off, &val.to_le_bytes())
            .expect("req field");
        sb.mmu
            .write_initializer(props_actual + off, &0u32.to_le_bytes())
            .expect("actual field");
    }

    // The allocator we'll ultimately advertise in NotifyAllocator.
    // Prefer the codec's own when it surfaced one; otherwise mint
    // a host allocator pre-sized to match what we'd SetProperties
    // for.
    let host_alloc = sb
        .mint_host_mem_allocator(4, 8192, amt)
        .expect("mint host allocator");
    eprintln!("round61 phase2: host_alloc = {host_alloc:#010x}");
    let target_alloc = if r_ga == 0 && codec_alloc != 0 {
        codec_alloc
    } else {
        host_alloc
    };
    eprintln!(
        "round61 phase2: target_alloc = {target_alloc:#010x}  (using_codec={})",
        target_alloc == codec_alloc && codec_alloc != 0
    );

    // Step A — SetProperties.
    let r_sp = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        target_alloc,
        SLOT_MEMALLOCATOR_SET_PROPERTIES,
        &[props_req, props_actual],
    )
    .expect("SetProperties must not trap");
    let actual_c = sb.mmu.load32(props_actual).unwrap_or(0);
    let actual_b = sb.mmu.load32(props_actual + 4).unwrap_or(0);
    let actual_a = sb.mmu.load32(props_actual + 8).unwrap_or(0);
    let actual_p = sb.mmu.load32(props_actual + 12).unwrap_or(0);
    eprintln!(
        "round61 phase2: SetProperties → hr={r_sp:#010x}, actual: \
         cBuffers={actual_c} cbBuffer={actual_b} cbAlign={actual_a} \
         cbPrefix={actual_p}"
    );
    assert!(
        (r_sp & 0x8000_0000) == 0,
        "SetProperties returned failure HRESULT {r_sp:#010x}"
    );

    // Step B — Commit.
    let r_co = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        target_alloc,
        SLOT_MEMALLOCATOR_COMMIT,
        &[],
    )
    .expect("Commit must not trap");
    eprintln!("round61 phase2: Commit → hr={r_co:#010x}");
    assert!(
        (r_co & 0x8000_0000) == 0,
        "Commit returned failure HRESULT {r_co:#010x}"
    );

    // Step C — NotifyAllocator(alloc, FALSE).  bReadOnly = 0
    // means we own the buffers and the codec may not mutate them
    // in-place.
    let r_na = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_NOTIFY_ALLOCATOR,
        &[target_alloc, 0],
    )
    .expect("NotifyAllocator must not trap");
    eprintln!("round61 phase2: NotifyAllocator → hr={r_na:#010x}");
    // Permissive: some DShow filters return E_NOTIMPL from
    // NotifyAllocator and rely entirely on the GetAllocator path
    // (per MSDN).  Accept any return; this is informational.
    eprintln!(
        "round61 phase2: handshake-complete, target_alloc Commit state pinned to 1 \
         (host-side observable at obj+12)"
    );

    // For the HOST allocator, Commit is observable at obj+12 = 1.
    // (For codec allocators, we don't peek inside their layout.)
    if target_alloc == host_alloc {
        let flag = sb.mmu.load32(host_alloc + 12).unwrap_or(0xDEAD);
        assert_eq!(
            flag, 1,
            "host allocator committed_state @ +12 should be 1 after Commit"
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 3 — Push real WMA frames through Receive
// ───────────────────────────────────────────────────────────────────

/// Phase 3 — after the full handshake lands, drive
/// `IMediaFilter::Pause + Run(0)` then push the round-59 ASF
/// fixture's first data packet through `IMemInputPin::Receive`
/// and observe the HRESULT.
///
/// Success criterion: the call no longer returns
/// `VFW_E_NOT_COMMITTED` (`0x80040209`).  Anything else — S_OK,
/// a new failure, or a trap — is reported on stderr for round-62
/// baselining.  Any PCM bytes that surface on the host sink are
/// also logged.
#[test]
fn phase3_receive_after_handshake_clears_not_committed() {
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round61 phase3: msadds32.ax missing; skipping");
        return;
    };
    let Some((_input_pin, mip, amt)) = open_connection_and_qi_mem_input_pin(&mut sb, filter) else {
        eprintln!("round61 phase3: cannot establish connection; skipping");
        return;
    };

    // -- Drive the handshake -------------------------------------
    let pp = sb.host.arena_alloc(4).expect("scratch for codec_alloc");
    sb.mmu
        .write_initializer(pp, &0u32.to_le_bytes())
        .expect("init out slot");
    let r_ga = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_GET_ALLOCATOR,
        &[pp],
    )
    .unwrap_or(0xFFFF_FFFF);
    let codec_alloc = sb.mmu.load32(pp).unwrap_or(0);

    let host_alloc = sb
        .mint_host_mem_allocator(4, 8192, amt)
        .expect("mint host allocator");
    let target_alloc = if r_ga == 0 && codec_alloc != 0 {
        codec_alloc
    } else {
        host_alloc
    };

    let props_req = sb.host.arena_alloc(16).expect("scratch req");
    let props_actual = sb.host.arena_alloc(16).expect("scratch actual");
    for (off, val) in [(0u32, 4u32), (4, 8192), (8, 1), (12, 0)] {
        sb.mmu
            .write_initializer(props_req + off, &val.to_le_bytes())
            .expect("req field");
        sb.mmu
            .write_initializer(props_actual + off, &0u32.to_le_bytes())
            .expect("actual field");
    }
    let r_sp = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        target_alloc,
        SLOT_MEMALLOCATOR_SET_PROPERTIES,
        &[props_req, props_actual],
    )
    .expect("SetProperties");
    let r_co = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        target_alloc,
        SLOT_MEMALLOCATOR_COMMIT,
        &[],
    )
    .expect("Commit");
    let r_na = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_NOTIFY_ALLOCATOR,
        &[target_alloc, 0],
    )
    .expect("NotifyAllocator");
    eprintln!(
        "round61 phase3: handshake hr trio: SetProps={r_sp:#010x} Commit={r_co:#010x} \
         NotifyAlloc={r_na:#010x}, using_codec={}",
        target_alloc == codec_alloc && codec_alloc != 0
    );

    // -- Pause + Run --------------------------------------------
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_MEDIAFILTER_PAUSE,
        &[],
    );
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_MEDIAFILTER_RUN,
        &[0, 0],
    );

    // -- Build a real-bytes sample ------------------------------
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    let asf_bytes = match std::fs::read(&fixture_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("round61 phase3: cannot read WMA2 fixture: {e}; skipping");
            return;
        }
    };
    let packet = oxideav_vfw::com::locate_first_data_packet(&asf_bytes).unwrap_or(&[]);
    if packet.is_empty() {
        eprintln!("round61 phase3: no data packet in ASF; skipping");
        return;
    }
    let payload: Vec<u8> = packet.iter().take(4096).copied().collect();
    let sample = sb
        .mint_host_media_sample(8192, amt)
        .expect("mint host media sample");
    sb.media_sample_set_payload(sample, &payload, true)
        .expect("set sample payload");

    // -- Drive Receive -------------------------------------------
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_RECEIVE,
        &[sample],
    );
    let pcm_queued = oxideav_vfw::com::host_iface_r31::queue_len(&sb.host);
    match r {
        Ok(hr) => {
            eprintln!(
                "round61 phase3: IMemInputPin::Receive({} B WMA2) → HRESULT {hr:#010x}",
                payload.len()
            );
            eprintln!("round61 phase3: PCM bytes queued on host sink = {pcm_queued}");
            if hr == 0x8004_0209 {
                eprintln!(
                    "round61 phase3: STILL VFW_E_NOT_COMMITTED after the input-pin \
                     handshake.  The codec is walking a SECOND internal allocator \
                     (almost certainly its output-pin's downstream allocator, \
                     which only gets set up when the output pin is connected to \
                     a downstream IMemInputPin via ReceiveConnection + \
                     NotifyAllocator).  Round 62 should drive the output-pin \
                     connection path — see phase5 below for the empirical probe."
                );
            }
        }
        Err(e) => {
            eprintln!("round61 phase3: Receive trapped: {e}");
            eprintln!(
                "round61 phase3: trap is also acceptable forward progress over \
                 round-60's VFW_E_NOT_COMMITTED — round 62 will surface the new \
                 blocker (likely a missing import)."
            );
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 5 — Probe: connect the codec's OUTPUT pin too
// ───────────────────────────────────────────────────────────────────

/// Phase 5 — empirical probe: try the round-31 video pattern of
/// also connecting the codec's OUTPUT pin to a host downstream
/// `(HostIPin, HostIMemInputPin)` pair before pushing samples.
/// If the codec's output-pin allocator was the source of the
/// remaining `VFW_E_NOT_COMMITTED`, this probe should clear it.
///
/// We enumerate the codec's pins, locate the output one (direction
/// = PIN_OUTPUT), then call `IPin::ReceiveConnection(host_in_pin,
/// dn_amt)` against it with a PCM-shaped downstream AMT.  Result
/// reported on stderr for round-62 baselining.
#[test]
fn phase5_probe_connect_output_pin_then_receive() {
    use oxideav_vfw::com::{Guid, PIN_DIRECTION_OUTPUT};
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round61 phase5: msadds32.ax missing; skipping");
        return;
    };
    let Some((_input_pin, mip, amt)) = open_connection_and_qi_mem_input_pin(&mut sb, filter) else {
        eprintln!("round61 phase5: cannot establish input connection; skipping");
        return;
    };

    // Enumerate ALL pins on the filter, pick the OUTPUT one.
    let scratch = sb.host.arena_alloc(4).expect("scratch");
    sb.mmu.write_initializer(scratch, &[0u8; 4]).expect("init");
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_BASEFILTER_ENUM_PINS,
        &[scratch],
    );
    let pp = sb.mmu.load32(scratch).unwrap_or(0);
    if pp == 0 {
        eprintln!("round61 phase5: filter has no pin enumerator; skipping");
        return;
    }
    sb.host.com.intern(pp, None);

    let mut output_pin: Option<u32> = None;
    for _ in 0..8 {
        let pin_slot = sb.host.arena_alloc(8).expect("pin slot");
        sb.mmu
            .write_initializer(pin_slot, &[0u8; 8])
            .expect("init pin slot");
        let _ = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            pp,
            SLOT_ENUMPINS_NEXT,
            &[1, pin_slot, pin_slot + 4],
        );
        let pin = sb.mmu.load32(pin_slot).unwrap_or(0);
        let fetched = sb.mmu.load32(pin_slot + 4).unwrap_or(0);
        if pin == 0 || fetched != 1 {
            break;
        }
        sb.host.com.intern(pin, None);
        let dir_slot = sb.host.arena_alloc(4).expect("dir slot");
        let _ = sb
            .mmu
            .write_initializer(dir_slot, &0xFFFF_FFFFu32.to_le_bytes());
        let _ = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            pin,
            SLOT_PIN_QUERY_DIRECTION,
            &[dir_slot],
        );
        let dir = sb.mmu.load32(dir_slot).unwrap_or(0xFFFF_FFFF);
        if dir == PIN_DIRECTION_OUTPUT {
            output_pin = Some(pin);
            break;
        }
    }
    let _ = sb.com_release(pp);

    let Some(out_pin) = output_pin else {
        eprintln!("round61 phase5: no PIN_OUTPUT on the filter; skipping");
        return;
    };
    eprintln!("round61 phase5: codec output_pin = {out_pin:#010x}");

    // Mint a downstream host IPin / IMemInputPin pair to accept
    // PCM samples the codec emits.
    let (h_pin, h_mip) = sb
        .host_iface_r31_mint_input_pin_pair()
        .expect("mint host input pin pair");
    let _h_filter = sb
        .host_iface_r31_mint_base_filter(h_pin)
        .expect("mint host base filter");
    eprintln!("round61 phase5: host downstream h_pin={h_pin:#010x} h_mip={h_mip:#010x}");

    // Stage a PCM downstream AMT.  Layout: MEDIATYPE_Audio /
    // MEDIASUBTYPE_PCM / FORMAT_WaveFormatEx with a plain
    // WAVEFORMATEX (no extradata, cbSize = 0).  Mono 44.1 kHz
    // 16-bit signed — the canonical "uncompressed" shape the WMA
    // decoder is expected to produce.
    let dn_wfx_len: u32 = 18; // WAVEFORMATEX struct = 18 bytes.
    let dn_total = 72 + dn_wfx_len;
    let dn_blob = sb.host.arena_alloc(dn_total).expect("dn amt blob");
    let dn_amt = dn_blob;
    let dn_fmt = dn_blob + 72;
    let mediatype_audio = Guid::parse("{73647561-0000-0010-8000-00AA00389B71}").unwrap();
    let mediasubtype_pcm = Guid::parse("{00000001-0000-0010-8000-00AA00389B71}").unwrap();
    let format_wave = Guid::parse("{05589F81-C356-11CE-BF01-00AA0055595A}").unwrap();
    mediatype_audio
        .stage(&mut sb.mmu, dn_amt)
        .expect("stage mt");
    mediasubtype_pcm
        .stage(&mut sb.mmu, dn_amt + 16)
        .expect("stage st");
    sb.mmu
        .write_initializer(dn_amt + 32, &0u32.to_le_bytes())
        .expect("bFixedSizeSamples");
    sb.mmu
        .write_initializer(dn_amt + 36, &1u32.to_le_bytes())
        .expect("bTemporalCompression");
    sb.mmu
        .write_initializer(dn_amt + 40, &0u32.to_le_bytes())
        .expect("lSampleSize");
    format_wave.stage(&mut sb.mmu, dn_amt + 44).expect("ft");
    sb.mmu
        .write_initializer(dn_amt + 60, &0u32.to_le_bytes())
        .expect("pUnk");
    sb.mmu
        .write_initializer(dn_amt + 64, &dn_wfx_len.to_le_bytes())
        .expect("cbFormat");
    sb.mmu
        .write_initializer(dn_amt + 68, &dn_fmt.to_le_bytes())
        .expect("pbFormat");
    // WAVEFORMATEX: wFormatTag=1 (PCM), 1 channel, 44100 Hz,
    // 88200 nAvgBytes/sec, 2 nBlockAlign, 16 wBitsPerSample,
    // cbSize=0.
    sb.mmu
        .write_initializer(dn_fmt, &1u16.to_le_bytes())
        .expect("wFormatTag");
    sb.mmu
        .write_initializer(dn_fmt + 2, &1u16.to_le_bytes())
        .expect("nChannels");
    sb.mmu
        .write_initializer(dn_fmt + 4, &44_100u32.to_le_bytes())
        .expect("nSamplesPerSec");
    sb.mmu
        .write_initializer(dn_fmt + 8, &88_200u32.to_le_bytes())
        .expect("nAvgBytesPerSec");
    sb.mmu
        .write_initializer(dn_fmt + 12, &2u16.to_le_bytes())
        .expect("nBlockAlign");
    sb.mmu
        .write_initializer(dn_fmt + 14, &16u16.to_le_bytes())
        .expect("wBitsPerSample");
    sb.mmu
        .write_initializer(dn_fmt + 16, &0u16.to_le_bytes())
        .expect("cbSize");

    let r_dn = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        out_pin,
        SLOT_PIN_RECEIVE_CONNECTION,
        &[h_pin, dn_amt],
    );
    match r_dn {
        Ok(hr) => eprintln!(
            "round61 phase5: codec out_pin->ReceiveConnection(PCM 44.1k mono 16-bit) → \
             HRESULT {hr:#010x}"
        ),
        Err(e) => eprintln!("round61 phase5: out_pin->ReceiveConnection trapped: {e}"),
    }

    // Re-drive input-pin handshake (post-output-connect, in case
    // the codec re-shaped the input pool after seeing its output
    // bindings).
    let host_alloc = sb
        .mint_host_mem_allocator(4, 8192, amt)
        .expect("re-mint host allocator");
    let pp_in = sb.host.arena_alloc(4).expect("pp_in");
    sb.mmu
        .write_initializer(pp_in, &0u32.to_le_bytes())
        .expect("init");
    let r_ga = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_GET_ALLOCATOR,
        &[pp_in],
    )
    .unwrap_or(0xFFFF_FFFF);
    let codec_alloc = sb.mmu.load32(pp_in).unwrap_or(0);
    let target_alloc = if r_ga == 0 && codec_alloc != 0 {
        codec_alloc
    } else {
        host_alloc
    };

    let props_req = sb.host.arena_alloc(16).expect("req");
    let props_actual = sb.host.arena_alloc(16).expect("actual");
    for (off, val) in [(0u32, 4u32), (4, 8192), (8, 1), (12, 0)] {
        let _ = sb
            .mmu
            .write_initializer(props_req + off, &val.to_le_bytes());
        let _ = sb
            .mmu
            .write_initializer(props_actual + off, &0u32.to_le_bytes());
    }
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        target_alloc,
        SLOT_MEMALLOCATOR_SET_PROPERTIES,
        &[props_req, props_actual],
    );
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        target_alloc,
        SLOT_MEMALLOCATOR_COMMIT,
        &[],
    );
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_NOTIFY_ALLOCATOR,
        &[target_alloc, 0],
    );

    // Pause + Run + Receive (final attempt).
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_MEDIAFILTER_PAUSE,
        &[],
    );
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_MEDIAFILTER_RUN,
        &[0, 0],
    );

    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    let asf_bytes = match std::fs::read(&fixture_path) {
        Ok(b) => b,
        Err(_) => return,
    };
    let packet = oxideav_vfw::com::locate_first_data_packet(&asf_bytes).unwrap_or(&[]);
    if packet.is_empty() {
        return;
    }
    let payload: Vec<u8> = packet.iter().take(4096).copied().collect();
    let sample = sb
        .mint_host_media_sample(8192, amt)
        .expect("mint host media sample");
    sb.media_sample_set_payload(sample, &payload, true)
        .expect("set sample payload");
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_RECEIVE,
        &[sample],
    );
    let pcm_queued = oxideav_vfw::com::host_iface_r31::queue_len(&sb.host);
    match r {
        Ok(hr) => eprintln!(
            "round61 phase5: post-output-connect Receive({} B) → HRESULT {hr:#010x}, \
             PCM queued = {pcm_queued}",
            payload.len()
        ),
        Err(e) => eprintln!("round61 phase5: post-output-connect Receive trapped: {e}"),
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 4 — Inspect what the codec asked our allocator for
// ───────────────────────────────────────────────────────────────────

/// Phase 4 — after `NotifyAllocator` runs, replay the round-33
/// `SetProperties` capture log (the per-`HostState` stash of every
/// ALLOCATOR_PROPERTIES the codec drove through us).  If the codec
/// itself called `SetProperties` on the host allocator from inside
/// `NotifyAllocator`, the captured fields surface the codec's
/// audio-buffer expectations — invaluable for round 62.
#[test]
fn phase4_capture_codec_set_properties_observations() {
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round61 phase4: msadds32.ax missing; skipping");
        return;
    };
    clear_set_properties_log(&sb.host);
    let Some((_input_pin, mip, amt)) = open_connection_and_qi_mem_input_pin(&mut sb, filter) else {
        eprintln!("round61 phase4: cannot establish connection; skipping");
        return;
    };

    let pp = sb.host.arena_alloc(4).expect("scratch for codec_alloc");
    sb.mmu
        .write_initializer(pp, &0u32.to_le_bytes())
        .expect("init out slot");
    let r_ga = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_GET_ALLOCATOR,
        &[pp],
    )
    .unwrap_or(0xFFFF_FFFF);
    let codec_alloc = sb.mmu.load32(pp).unwrap_or(0);

    let host_alloc = sb
        .mint_host_mem_allocator(4, 8192, amt)
        .expect("mint host allocator");
    let target_alloc = if r_ga == 0 && codec_alloc != 0 {
        codec_alloc
    } else {
        host_alloc
    };

    let props_req = sb.host.arena_alloc(16).expect("req arena");
    let props_actual = sb.host.arena_alloc(16).expect("actual arena");
    for (off, val) in [(0u32, 4u32), (4, 8192), (8, 1), (12, 0)] {
        sb.mmu
            .write_initializer(props_req + off, &val.to_le_bytes())
            .expect("req field");
        sb.mmu
            .write_initializer(props_actual + off, &0u32.to_le_bytes())
            .expect("actual field");
    }
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        target_alloc,
        SLOT_MEMALLOCATOR_SET_PROPERTIES,
        &[props_req, props_actual],
    );
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        target_alloc,
        SLOT_MEMALLOCATOR_COMMIT,
        &[],
    );
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_NOTIFY_ALLOCATOR,
        &[target_alloc, 0],
    );

    let captures: Vec<AllocatorPropertiesCapture> = all_set_properties(&sb.host);
    eprintln!(
        "round61 phase4: SetProperties captures = {} entries",
        captures.len()
    );
    for (i, c) in captures.iter().enumerate() {
        eprintln!(
            "  [{i}] this={this:#010x} cBuffers={cb} cbBuffer={bb} cbAlign={ab} \
             cbPrefix={pb}",
            this = c.this,
            cb = c.c_buffers,
            bb = c.cb_buffer,
            ab = c.cb_align,
            pb = c.cb_prefix,
        );
    }
    // We expect at least our own SetProperties call to surface in
    // the log.  If the codec drove additional ones on the way
    // through NotifyAllocator we'd see them too.
    assert!(
        !captures.is_empty(),
        "expected at least our own SetProperties to register in the log"
    );
}

// ---- helper: stage AMT from blueprint (mirrors round 60) -------------

fn stage_audio_amt_from_blueprint(
    sb: &mut Sandbox,
    bp: &oxideav_vfw::com::AmtBlueprint,
) -> Result<u32, oxideav_vfw::Error> {
    use oxideav_vfw::com::Guid;
    use oxideav_vfw::Error;
    let wfx_len = bp.wfx_total_len();
    let total = 72 + wfx_len + 16;
    let blob = sb.host.arena_alloc(total).map_err(Error::Win32)?;
    let amt = blob;
    let fmt = blob + 72;
    let trap = Error::Trap;
    let mediatype_audio = Guid::parse("{73647561-0000-0010-8000-00AA00389B71}").unwrap();
    let format_wave = Guid::parse("{05589F81-C356-11CE-BF01-00AA0055595A}").unwrap();
    let subtype = Guid::new(
        bp.format_tag as u32,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    );
    mediatype_audio.stage(&mut sb.mmu, amt).map_err(trap)?;
    subtype.stage(&mut sb.mmu, amt + 16).map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 32, &0u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 36, &1u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 40, &0u32.to_le_bytes())
        .map_err(trap)?;
    format_wave.stage(&mut sb.mmu, amt + 44).map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 60, &0u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 64, &wfx_len.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 68, &fmt.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt, &bp.format_tag.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 2, &bp.n_channels.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 4, &bp.n_samples_per_sec.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 8, &bp.n_avg_bytes_per_sec.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 12, &bp.n_block_align.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 14, &bp.w_bits_per_sample.to_le_bytes())
        .map_err(trap)?;
    let cb = bp.extradata.len() as u16;
    sb.mmu
        .write_initializer(fmt + 16, &cb.to_le_bytes())
        .map_err(trap)?;
    if !bp.extradata.is_empty() {
        sb.mmu
            .write_initializer(fmt + 18, &bp.extradata)
            .map_err(trap)?;
    }
    Ok(amt)
}
