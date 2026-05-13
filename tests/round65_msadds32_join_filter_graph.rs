//! Round 65 — drive `IBaseFilter::JoinFilterGraph` (DirectShow vtable
//! slot 13) on `msadds32.ax`'s audio splitter BEFORE `Pause` so the
//! codec's own filter-graph-aware setup populates the inner-decode
//! state (`[esi+0xa4]`) and the `helper_struct[+0x20]` "initialised"
//! flag that the round-63
//! [`Sandbox::msadds32_patch_helper_addref`] workaround fakes out.
//!
//! Round 64 (see `tests/round64_msadds32_e_unexpected.rs` +
//! `docs/codec/msadds32-receive-e-unexpected.md`) pinned the
//! `Receive` E_UNEXPECTED bail-out to the inner-decode-no-output
//! guard at RVA `0x172f`: the codec accepts our WMA2 input frame
//! (the inner decode at RVA `0xc887` returns `eax = 0`), but the
//! "samples produced" out-pointer `&[ebp-0x10]` stays NULL, so two
//! consecutive iterations of the outer `Receive` loop emit zero
//! PCM and the codec bails with `E_UNEXPECTED`.
//!
//! The structurally cleanest fix candidate (#1 in the round-64
//! hand-off) is to drive the proper DirectShow bring-up sequence —
//! specifically `IBaseFilter::JoinFilterGraph(pGraph, pName)` —
//! BEFORE `Pause`.  In real DirectShow, the codec uses
//! `JoinFilterGraph` to (a) stash an IFilterGraph back-pointer at
//! some offset on the filter instance and (b) optionally query the
//! graph for interfaces (e.g. `IGraphConfig`) during subsequent
//! Pause-time initialisation.  Without it, the codec's internal
//! state machine never reaches the "ready-to-decode" state where
//! `[esi+0xa4]` is populated.
//!
//! ## What this round measures
//!
//! Five phases:
//!
//!   * `phase1` — drive Bootstrap + JoinFilterGraph + Pause +
//!     introspect helper offsets `[+0x3c]` (the round-63 flag, =
//!     `[ecx+0x20]` from `helper_addref` with `ecx = helper_90 +
//!     0x1c`) on each visible codec pointer.  If JoinFilterGraph
//!     naturally populates it, the workaround is retirable.
//!   * `phase2` — drive the full `Receive` chain WITHOUT the
//!     round-63 patch but WITH `JoinFilterGraph` driven before
//!     `Pause`.  Capture the HRESULT and any trap.
//!   * `phase3` — drive the same chain WITH both `JoinFilterGraph`
//!     AND the `helper_addref` patch (current round-64 baseline +
//!     JoinFilterGraph).  Compare against `phase2` + the round-64
//!     baseline.
//!   * `phase4` — ASF Payload-Parsing-Information stripping experiment.
//!     If `phase3` still bails at `0x172f`, retry with the input
//!     bytes' first 12 bytes stripped (Payload Parsing header per
//!     ASF spec §5.2.2).
//!   * `phase5` — IFilterGraph callback log — report how many times
//!     each `IFilterGraph` method was invoked by the codec during
//!     `JoinFilterGraph + Pause`.  This is the empirical signal for
//!     whether the codec actually used the back-pointer (and which
//!     methods it called).
//!
//! ## Reference material (clean-room only)
//!
//! * Intel SDM Vol. 2 — opcode encoding, ModR/M, control transfer.
//! * MSDN — `IBaseFilter::JoinFilterGraph`, `IFilterGraph`,
//!   `IMediaFilter::Pause`, `IMemInputPin::Receive`.
//! * ASF spec (Microsoft public 2004-12 / Reference Implementation
//!   licence) §5.2.2 — Payload Parsing Information byte layout.
//! * Raw bytes of `msadds32.ax` from
//!   `docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/`.
//!
//! No Wine / ReactOS / MinGW / Microsoft DShow base-class source
//! consulted.

use oxideav_vfw::com::{
    call::{call_method, vtable_is_plausible},
    AmtBlueprint, Guid, MSADDS_AUDIO_DECODER_CLSID, PIN_DIRECTION_INPUT, PIN_DIRECTION_OUTPUT,
    SLOT_BASEFILTER_ENUM_PINS, SLOT_BASEFILTER_JOIN_FILTER_GRAPH, SLOT_BASEFILTER_STOP,
    SLOT_ENUMPINS_NEXT, SLOT_MEDIAFILTER_PAUSE, SLOT_MEDIAFILTER_RUN, SLOT_MEMALLOCATOR_COMMIT,
    SLOT_MEMALLOCATOR_SET_PROPERTIES, SLOT_MEMINPUTPIN_GET_ALLOCATOR,
    SLOT_MEMINPUTPIN_NOTIFY_ALLOCATOR, SLOT_MEMINPUTPIN_RECEIVE, SLOT_PIN_QUERY_DIRECTION,
    SLOT_PIN_RECEIVE_CONNECTION,
};
use oxideav_vfw::{Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY, IID_IMEMINPUTPIN, IID_IUNKNOWN};
use std::path::PathBuf;

// ─── shared bootstrap helpers (mirrors r63 / r64) ─────────────────────

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

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio/wma2_440hz_mono_1s.wma")
}

fn load_msadds32_with_big_trace() -> Option<(Sandbox, oxideav_vfw::pe::Image)> {
    let p = msadds32_path()?;
    let bytes = std::fs::read(&p).ok()?;
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(8_000_000_000);
    sb.cpu.enable_trace_ring(1_048_576);
    sb.cpu.track_visited_eips = true;
    let img = sb.load("msadds32.ax", &bytes).ok()?;
    let _ = sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH);
    Some((sb, img))
}

fn bootstrap_filter() -> Option<(Sandbox, oxideav_vfw::pe::Image, u32, u32)> {
    let (mut sb, img) = load_msadds32_with_big_trace()?;
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
    Some((sb, img, unk, filter))
}

fn enum_pin_by_direction(sb: &mut Sandbox, filter: u32, want_dir: u32) -> Option<u32> {
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
    call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_BASEFILTER_ENUM_PINS,
        &[scratch],
    )
    .ok()?;
    let pp = sb.mmu.load32(scratch).ok()?;
    if pp == 0 {
        return None;
    }
    sb.host.com.intern(pp, None);
    let mut found = None;
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
        if dir == want_dir {
            found = Some(pin);
            break;
        }
    }
    let _ = sb.com_release(pp);
    found
}

fn stage_audio_amt_from_blueprint(
    sb: &mut Sandbox,
    bp: &AmtBlueprint,
) -> Result<u32, oxideav_vfw::Error> {
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

// ─── helper-struct introspection ──────────────────────────────────────

/// Probe each candidate codec-side pointer at offset `+0x90` and,
/// if that dereferences to a non-NULL "helper struct", read both
/// the `+0x3c` field (= round-63 `[ecx+0x20]` "initialised" flag,
/// because callers pass `helper_90 + 0x1c` as `ecx`, so the codec-
/// view field is at `helper_90 + 0x3c`) and the `+0x44` field (=
/// the cached value `[ecx+0x28]`).
///
/// Returns `(flag, cached, helper_ptr)` for the FIRST pointer that
/// has a non-zero `+0x90`, or `(0, 0, 0)` if none do.  Caller
/// stderr-logs the full set.
fn probe_helper_init(sb: &Sandbox, pointers: &[(&str, u32)]) -> (u32, u32, u32) {
    let mut answer = (0u32, 0u32, 0u32);
    for &(label, p) in pointers {
        if p == 0 {
            eprintln!("  {label}: NULL (skipped)");
            continue;
        }
        let helper_field = sb.mmu.load32(p + 0x90).unwrap_or(0);
        eprintln!("  {label}+0x90 = {helper_field:#010x}");
        if helper_field != 0 {
            let flag = sb.mmu.load32(helper_field + 0x3c).unwrap_or(0xdead_beef);
            let cached = sb.mmu.load32(helper_field + 0x44).unwrap_or(0xdead_beef);
            eprintln!("    helper[+0x3c]={flag:#010x}  helper[+0x44]={cached:#010x}");
            if answer.2 == 0 {
                answer = (flag, cached, helper_field);
            }
        }
    }
    answer
}

// ─── full-chain driver: bootstrap + JoinFilterGraph + Receive ──────────

struct ReceiveOutcome {
    sb: Sandbox,
    filter: u32,
    unk: u32,
    input_pin: u32,
    mip: u32,
    /// Whether `JoinFilterGraph` was actually driven before `Pause`.
    joined: bool,
    /// HRESULT of `IBaseFilter::JoinFilterGraph` (if joined).
    join_hr: Option<u32>,
    /// HRESULT of `IMediaFilter::Pause`.
    pause_hr: Option<u32>,
    /// HRESULT of `IMemInputPin::Receive` (or the trap message).
    receive_hr: Option<u32>,
    receive_trap: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn drive_full_chain(
    apply_helper_addref_patch: Option<u32>,
    drive_join_filter_graph: bool,
    strip_asf_payload_parsing: bool,
) -> Option<ReceiveOutcome> {
    let (mut sb, img, unk, filter) = bootstrap_filter()?;
    let base = img.image_base;
    let _ = base;
    if let Some(v) = apply_helper_addref_patch {
        sb.msadds32_patch_helper_addref(base, v).ok()?;
    }

    // ── PHASE 1 of DShow bring-up: JoinFilterGraph ──────────────────
    let (joined, join_hr) = if drive_join_filter_graph {
        let host_graph = sb.mint_host_filter_graph().ok()?;
        // Stage `L"Audio Splitter"` as a NUL-terminated UTF-16LE
        // string for the `pName` arg.
        let name_utf16: Vec<u8> = "Audio Splitter\0"
            .encode_utf16()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        let name = sb.host.arena_alloc(name_utf16.len() as u32).ok()?;
        sb.mmu.write_initializer(name, &name_utf16).ok()?;
        let hr = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            filter,
            SLOT_BASEFILTER_JOIN_FILTER_GRAPH,
            &[host_graph, name],
        )
        .ok();
        (true, hr)
    } else {
        (false, None)
    };

    // ── Input-pin connection ────────────────────────────────────────
    let input_pin = enum_pin_by_direction(&mut sb, filter, PIN_DIRECTION_INPUT)?;
    let bp = AmtBlueprint::wma_criteria_passing(0x0161, 1, 44_100, 4_000, 185);
    let amt = stage_audio_amt_from_blueprint(&mut sb, &bp).ok()?;
    let host_out = sb
        .mint_host_output_pin_with_connection(amt, input_pin)
        .ok()?;
    let r_rc = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        input_pin,
        SLOT_PIN_RECEIVE_CONNECTION,
        &[host_out, amt],
    )
    .ok()?;
    if r_rc != 0 {
        eprintln!("drive_full_chain: ReceiveConnection returned {r_rc:#010x}, aborting chain");
        return None;
    }
    let mip = sb.query_interface(input_pin, IID_IMEMINPUTPIN).ok()?;
    if mip == 0 {
        return None;
    }

    // ── Output-pin connection (PCM downstream) ──────────────────────
    let out_pin = enum_pin_by_direction(&mut sb, filter, PIN_DIRECTION_OUTPUT)?;
    let (h_pin, _h_mip) = sb.host_iface_r31_mint_input_pin_pair().ok()?;
    let _ = sb.host_iface_r31_mint_base_filter(h_pin).ok()?;
    let dn_wfx_len: u32 = 18;
    let dn_total = 72 + dn_wfx_len;
    let dn_blob = sb.host.arena_alloc(dn_total).ok()?;
    let dn_amt = dn_blob;
    let dn_fmt = dn_blob + 72;
    let mediatype_audio = Guid::parse("{73647561-0000-0010-8000-00AA00389B71}").unwrap();
    let mediasubtype_pcm = Guid::parse("{00000001-0000-0010-8000-00AA00389B71}").unwrap();
    let format_wave = Guid::parse("{05589F81-C356-11CE-BF01-00AA0055595A}").unwrap();
    let _ = mediatype_audio.stage(&mut sb.mmu, dn_amt);
    let _ = mediasubtype_pcm.stage(&mut sb.mmu, dn_amt + 16);
    let _ = sb.mmu.write_initializer(dn_amt + 32, &0u32.to_le_bytes());
    let _ = sb.mmu.write_initializer(dn_amt + 36, &1u32.to_le_bytes());
    let _ = sb.mmu.write_initializer(dn_amt + 40, &0u32.to_le_bytes());
    let _ = format_wave.stage(&mut sb.mmu, dn_amt + 44);
    let _ = sb.mmu.write_initializer(dn_amt + 60, &0u32.to_le_bytes());
    let _ = sb
        .mmu
        .write_initializer(dn_amt + 64, &dn_wfx_len.to_le_bytes());
    let _ = sb.mmu.write_initializer(dn_amt + 68, &dn_fmt.to_le_bytes());
    let _ = sb.mmu.write_initializer(dn_fmt, &1u16.to_le_bytes());
    let _ = sb.mmu.write_initializer(dn_fmt + 2, &1u16.to_le_bytes());
    let _ = sb
        .mmu
        .write_initializer(dn_fmt + 4, &44_100u32.to_le_bytes());
    let _ = sb
        .mmu
        .write_initializer(dn_fmt + 8, &88_200u32.to_le_bytes());
    let _ = sb.mmu.write_initializer(dn_fmt + 12, &2u16.to_le_bytes());
    let _ = sb.mmu.write_initializer(dn_fmt + 14, &16u16.to_le_bytes());
    let _ = sb.mmu.write_initializer(dn_fmt + 16, &0u16.to_le_bytes());
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        out_pin,
        SLOT_PIN_RECEIVE_CONNECTION,
        &[h_pin, dn_amt],
    );

    // ── Allocator handshake ──────────────────────────────────────────
    let pp = sb.host.arena_alloc(4).ok()?;
    let _ = sb.mmu.write_initializer(pp, &0u32.to_le_bytes());
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
    let host_alloc = sb.mint_host_mem_allocator(4, 8192, amt).ok()?;
    let target_alloc = if r_ga == 0 && codec_alloc != 0 {
        codec_alloc
    } else {
        host_alloc
    };
    let req = sb.host.arena_alloc(16).ok()?;
    let actual = sb.host.arena_alloc(16).ok()?;
    for (off, val) in [(0u32, 4u32), (4, 8192), (8, 1), (12, 0)] {
        let _ = sb.mmu.write_initializer(req + off, &val.to_le_bytes());
        let _ = sb.mmu.write_initializer(actual + off, &0u32.to_le_bytes());
    }
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        target_alloc,
        SLOT_MEMALLOCATOR_SET_PROPERTIES,
        &[req, actual],
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

    // ── Pause + Run ──────────────────────────────────────────────────
    let pause_hr = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_MEDIAFILTER_PAUSE,
        &[],
    )
    .ok();
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_MEDIAFILTER_RUN,
        &[0, 0],
    );

    // ── WMA fixture sample ───────────────────────────────────────────
    let asf_bytes = std::fs::read(fixture_path()).ok()?;
    let packet = oxideav_vfw::com::locate_first_data_packet(&asf_bytes).unwrap_or(&[]);
    if packet.is_empty() {
        return None;
    }
    let raw: Vec<u8> = packet.iter().take(4096).copied().collect();
    let payload: Vec<u8> = if strip_asf_payload_parsing {
        // ASF Payload Parsing Information (§5.2.2): the first byte
        // is the Length Type Flags / ECC byte; the minimum well-
        // formed PPI header is 12 bytes (ECC + LTF + PropFlags +
        // PacketLen + Sequence + Padding + SendTime + Duration).
        // Strip a conservative 12-byte prefix; the codec consumes
        // the remainder as raw WMA super-frame bytes.
        raw.iter().skip(12).copied().collect()
    } else {
        raw
    };
    let sample = sb.mint_host_media_sample(8192, amt).ok()?;
    sb.media_sample_set_payload(sample, &payload, true).ok()?;

    sb.cpu.trace_ring.clear();
    sb.cpu.visited_eips.clear();

    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_RECEIVE,
        &[sample],
    );
    let (receive_hr, receive_trap) = match r {
        Ok(hr) => (Some(hr), None),
        Err(e) => (None, Some(format!("{e}"))),
    };

    Some(ReceiveOutcome {
        sb,
        filter,
        unk,
        input_pin,
        mip,
        joined,
        join_hr,
        pause_hr,
        receive_hr,
        receive_trap,
    })
}

// ─── Phase 1 — JoinFilterGraph effect on helper-struct flags ──────────

/// Phase 1 — drive Bootstrap → JoinFilterGraph(host_graph,
/// L"Audio Splitter") → enum-pins → ReceiveConnection → allocator
/// handshake → Pause → Run, then probe `[+0x90]` on every codec
/// pointer we have a handle to.  Report whether
/// `helper_struct[+0x3c]` (= `[ecx+0x20]`, the round-63 flag) is
/// now set without the patch.
#[test]
fn phase1_join_filter_graph_then_probe_helper_init_flag() {
    if msadds32_path().is_none() {
        eprintln!("round65 phase1: msadds32.ax missing; skipping");
        return;
    }
    if !fixture_path().is_file() {
        eprintln!("round65 phase1: WMA2 fixture missing; skipping");
        return;
    }
    let Some(o) = drive_full_chain(None, true, false) else {
        eprintln!("round65 phase1: drive_full_chain bootstrap failed");
        return;
    };
    eprintln!(
        "round65 phase1: joined={}  join_hr={:?}  pause_hr={:?}  receive_hr={:?}  trap={:?}",
        o.joined, o.join_hr, o.pause_hr, o.receive_hr, o.receive_trap
    );
    eprintln!("round65 phase1: helper-struct probe across codec pointers:");
    let pointers = [
        ("unk", o.unk),
        ("filter", o.filter),
        ("input_pin", o.input_pin),
        ("mip", o.mip),
    ];
    let (flag, cached, helper) = probe_helper_init(&o.sb, &pointers);
    eprintln!(
        "round65 phase1: aggregate — helper={:#010x} flag={:#010x} cached={:#010x}",
        helper, flag, cached
    );
    // Empirical finding gates: we don't fail the test if the flag
    // remains zero — that *is* the expected outcome if
    // JoinFilterGraph alone doesn't drive the codec's run-state
    // machinery.  Phase 2 + 3 quantify the consequence.
}

// ─── Phase 2 — Receive without helper-addref patch ────────────────────

/// Phase 2 — drive the full chain WITHOUT the round-63 patch but
/// WITH `JoinFilterGraph` driven before `Pause`.  If `JoinFilterGraph`
/// naturally populates `helper_struct[+0x20]`, this run will pass
/// past the round-62 LIFO-push trap and EITHER return S_OK +
/// produce PCM (THE BREAKTHROUGH) OR fall back to the round-64
/// `E_UNEXPECTED` surface OR hit a different failure.
#[test]
fn phase2_receive_no_patch_with_join_filter_graph() {
    if msadds32_path().is_none() {
        eprintln!("round65 phase2: msadds32.ax missing; skipping");
        return;
    }
    if !fixture_path().is_file() {
        eprintln!("round65 phase2: WMA2 fixture missing; skipping");
        return;
    }
    let Some(o) = drive_full_chain(None, true, false) else {
        eprintln!("round65 phase2: drive_full_chain bootstrap failed");
        return;
    };
    eprintln!(
        "round65 phase2: joined={}  join_hr={:?}  pause_hr={:?}  receive_hr={:?}  trap={:?}",
        o.joined, o.join_hr, o.pause_hr, o.receive_hr, o.receive_trap
    );
    // The phase reports outcome without a hard assertion on the
    // HRESULT: this is forensic.  But we DO sanity-pin two
    // structural sentinels so the run is replicable:
    //   * `joined == true`
    //   * `join_hr` returned a value (i.e. JoinFilterGraph didn't
    //     trap mid-call)
    assert!(o.joined, "round65 phase2: JoinFilterGraph was not driven");
    if let Some(hr) = o.join_hr {
        eprintln!("round65 phase2: JoinFilterGraph HRESULT = {hr:#010x}");
    } else {
        eprintln!("round65 phase2: JoinFilterGraph trapped (no HRESULT returned)");
    }
    // The BREAKTHROUGH report.  If Receive returns S_OK we want
    // a loud signal in the test log.
    if o.receive_hr == Some(0) {
        eprintln!("round65 phase2: ★★★ BREAKTHROUGH ★★★ Receive returned S_OK without patch!");
    } else if o.receive_hr == Some(0x8000_ffff) {
        eprintln!(
            "round65 phase2: same E_UNEXPECTED as round 64 — JoinFilterGraph alone \
             did NOT unblock the inner-decode-no-output path"
        );
    } else if let Some(hr) = o.receive_hr {
        eprintln!("round65 phase2: new HRESULT surface = {hr:#010x}");
    } else if let Some(msg) = &o.receive_trap {
        eprintln!("round65 phase2: trap = {msg}");
    }
}

// ─── Phase 3 — JoinFilterGraph + helper-addref patch combined ─────────

/// Phase 3 — drive with BOTH the round-63 patch AND `JoinFilterGraph`.
/// If JoinFilterGraph's own initialisation also wires up `[esi+0xa4]`
/// (the inner-decode context), this is the path that would let the
/// inner decode actually emit PCM.  Compare against the round-64
/// baseline which used the patch but no JoinFilterGraph.
#[test]
fn phase3_receive_with_patch_and_join_filter_graph() {
    if msadds32_path().is_none() {
        eprintln!("round65 phase3: msadds32.ax missing; skipping");
        return;
    }
    if !fixture_path().is_file() {
        eprintln!("round65 phase3: WMA2 fixture missing; skipping");
        return;
    }
    let Some(o) = drive_full_chain(Some(65_536), true, false) else {
        eprintln!("round65 phase3: drive_full_chain bootstrap failed");
        return;
    };
    eprintln!(
        "round65 phase3: joined={}  join_hr={:?}  pause_hr={:?}  receive_hr={:?}  trap={:?}",
        o.joined, o.join_hr, o.pause_hr, o.receive_hr, o.receive_trap
    );
    if o.receive_hr == Some(0) {
        eprintln!(
            "round65 phase3: ★★★ BREAKTHROUGH ★★★ Receive returned S_OK \
             with patch + JoinFilterGraph!"
        );
    } else if o.receive_hr == Some(0x8000_ffff) {
        eprintln!(
            "round65 phase3: same E_UNEXPECTED as round 64 — JoinFilterGraph + patch \
             still hit inner-decode-no-output"
        );
    } else if let Some(hr) = o.receive_hr {
        eprintln!("round65 phase3: new HRESULT surface = {hr:#010x}");
    } else if let Some(msg) = &o.receive_trap {
        eprintln!("round65 phase3: trap = {msg}");
    }
}

// ─── Phase 4 — strip ASF Payload Parsing Information framing ──────────

/// Phase 4 — fall-back probe: keep the round-63 patch and the
/// JoinFilterGraph wiring, but strip the first 12 bytes of the ASF
/// data-packet body before pushing it as the `Receive` sample.
/// These 12 bytes are the minimum-size ASF Payload Parsing
/// Information header (ECC + LTF + PropFlags + PacketLen + Sequence,
/// Padding + SendTime + Duration per ASF §5.2.2); under real
/// playback the ASF demuxer strips them before handing the codec
/// the raw WMA super-frame.  Our scaffold doesn't do this.
#[test]
fn phase4_strip_asf_payload_parsing() {
    if msadds32_path().is_none() {
        eprintln!("round65 phase4: msadds32.ax missing; skipping");
        return;
    }
    if !fixture_path().is_file() {
        eprintln!("round65 phase4: WMA2 fixture missing; skipping");
        return;
    }
    let Some(o) = drive_full_chain(Some(65_536), true, true) else {
        eprintln!("round65 phase4: drive_full_chain bootstrap failed");
        return;
    };
    eprintln!(
        "round65 phase4: joined={}  join_hr={:?}  pause_hr={:?}  receive_hr={:?}  trap={:?}",
        o.joined, o.join_hr, o.pause_hr, o.receive_hr, o.receive_trap
    );
    if o.receive_hr == Some(0) {
        eprintln!(
            "round65 phase4: ★★★ BREAKTHROUGH ★★★ ASF framing strip + patch + \
             JoinFilterGraph yields S_OK!"
        );
    } else if let Some(hr) = o.receive_hr {
        eprintln!("round65 phase4: HRESULT = {hr:#010x}");
    } else if let Some(msg) = &o.receive_trap {
        eprintln!("round65 phase4: trap = {msg}");
    }
}

// ─── Phase 5 — IFilterGraph callback log ──────────────────────────────

/// Phase 5 — drive a minimal bring-up (CoCreateInstance + QI +
/// JoinFilterGraph + Pause) with the trace ring kept hot across
/// those calls, and report which IFilterGraph thunk addresses the
/// codec called back into.  This is the empirical signal for
/// whether the codec uses the graph back-pointer at all during
/// bring-up.
///
/// The host filter-graph object's vtable function pointers are
/// the synthetic thunk addresses registered with the stub registry
/// under names `"IFilterGraph::*"`.  We resolve them once and
/// scan the post-Pause trace ring (with `visited_eips`) for any EIP
/// that matches.
#[test]
fn phase5_count_ifiltergraph_callbacks() {
    if msadds32_path().is_none() {
        eprintln!("round65 phase5: msadds32.ax missing; skipping");
        return;
    }
    let Some((mut sb, _img, _unk, filter)) = bootstrap_filter() else {
        eprintln!("round65 phase5: bootstrap_filter failed");
        return;
    };
    // Mint the host IFilterGraph + drive JoinFilterGraph then Pause.
    let host_graph = sb.mint_host_filter_graph().expect("mint host graph");
    let name_utf16: Vec<u8> = "Audio Splitter\0"
        .encode_utf16()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    let name = sb.host.arena_alloc(name_utf16.len() as u32).unwrap();
    sb.mmu.write_initializer(name, &name_utf16).unwrap();
    // Reset trace ring + visited so this phase captures only the
    // JoinFilterGraph + Pause window.
    sb.cpu.trace_ring.clear();
    sb.cpu.visited_eips.clear();
    let join_hr = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_BASEFILTER_JOIN_FILTER_GRAPH,
        &[host_graph, name],
    )
    .ok();
    let pause_hr = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_MEDIAFILTER_PAUSE,
        &[],
    )
    .ok();
    eprintln!("round65 phase5: join_hr={join_hr:?}  pause_hr={pause_hr:?}");
    let host_dll = "host-com.host";
    let methods = [
        "IFilterGraph::QueryInterface",
        "IFilterGraph::AddRef",
        "IFilterGraph::Release",
        "IFilterGraph::AddFilter",
        "IFilterGraph::RemoveFilter",
        "IFilterGraph::EnumFilters",
        "IFilterGraph::FindFilterByName",
        "IFilterGraph::ConnectDirect",
        "IFilterGraph::Reconnect",
        "IFilterGraph::Disconnect",
        "IFilterGraph::SetDefaultSyncSource",
    ];
    eprintln!("round65 phase5: IFilterGraph thunk visit counts in trace ring:");
    for name in methods {
        let Some(thunk) = sb.registry.resolve(host_dll, name) else {
            eprintln!("  {name}: thunk not registered");
            continue;
        };
        let hits = sb
            .cpu
            .trace_ring
            .iter()
            .filter(|&&eip| eip == thunk)
            .count();
        eprintln!("  {name:<36} thunk={thunk:#010x}  hits={hits}");
    }
    eprintln!(
        "round65 phase5: trace ring length = {}, unique EIPs = {}",
        sb.cpu.trace_ring.len(),
        sb.cpu.visited_eips.len()
    );
}

// ─── Phase 6 — workaround-retirement assertion ────────────────────────

/// Phase 6 — assertion: if `JoinFilterGraph` populates the
/// `helper_struct[+0x20]` flag natively, the round-63
/// [`Sandbox::msadds32_patch_helper_addref`] workaround is
/// retirable: a Receive run WITHOUT the patch should NOT trap at
/// the round-62 `0x00000020` site.
///
/// The test reports the outcome without an unconditional
/// failure assertion; it logs whether the workaround can be
/// retired (Receive runs to completion without patch) or whether
/// the workaround is still required (trap recurs).
#[test]
fn phase6_workaround_retirement_check() {
    if msadds32_path().is_none() {
        eprintln!("round65 phase6: msadds32.ax missing; skipping");
        return;
    }
    if !fixture_path().is_file() {
        eprintln!("round65 phase6: WMA2 fixture missing; skipping");
        return;
    }
    let Some(o) = drive_full_chain(None, true, false) else {
        eprintln!("round65 phase6: drive_full_chain bootstrap failed");
        return;
    };
    let retirable = match (&o.receive_trap, o.receive_hr) {
        (Some(msg), _) if msg.contains("memory fault at 0x00000020") => {
            eprintln!(
                "round65 phase6: workaround STILL REQUIRED — \
                 unpatched Receive trapped at 0x00000020 even with \
                 JoinFilterGraph driven"
            );
            false
        }
        (Some(msg), _) => {
            eprintln!(
                "round65 phase6: trap moved (no longer 0x00000020): {msg} — \
                 workaround MAY be retirable but a new failure surface emerged"
            );
            false
        }
        (None, Some(hr)) => {
            eprintln!(
                "round65 phase6: workaround RETIREMENT VIABLE — \
                 unpatched Receive returned HRESULT {hr:#010x} \
                 (no 0x00000020 trap)"
            );
            true
        }
        (None, None) => {
            eprintln!("round65 phase6: drive_full_chain returned no HRESULT and no trap?");
            false
        }
    };
    eprintln!("round65 phase6: retirable = {retirable}");
}
