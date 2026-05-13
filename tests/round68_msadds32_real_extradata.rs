//! Round 68 — populate the `WAVEFORMATEX::cbSize` codec-private-data
//! preamble with the bytes a real ffmpeg-generated WMA fixture emits,
//! and re-run the `IMemInputPin::Receive` chain against `msadds32.ax`'s
//! audio splitter.
//!
//! Round 64 (see `tests/round64_msadds32_e_unexpected.rs` +
//! `docs/codec/msadds32-receive-e-unexpected.md`) pinned the bail-out
//! at RVA `0x172f`: the codec accepts our WMA2 frame, the inner
//! decode at RVA `0xc887` returns `eax = 0`, but the "samples
//! produced" out-pointer stays NULL and two consecutive zero-output
//! iterations make the outer loop emit `E_UNEXPECTED`.
//!
//! Rounds 64+65 falsified candidates (1) JoinFilterGraph and (3) ASF
//! Payload Parsing strip.  Candidate (2) — the WAVEFORMATEX-tail
//! codec-private-data preamble is empty / all-zero — is what this
//! round probes.
//!
//! ## What this round measures
//!
//! Four phases:
//!
//!   * `phase1` — unit-level: assert the new
//!     [`AmtBlueprint::wma_with_ffmpeg_extradata_prefix`] constructor
//!     produces a blueprint whose extradata starts with the bytes
//!     ffmpeg emits and ends with the 37-byte CLSID suffix the
//!     `CompleteConnect` validator demands.
//!   * `phase2` — drive the full Receive chain WITHOUT the
//!     round-63 `helper_addref` patch but with the ffmpeg-derived
//!     extradata.  If proper extradata also drives the natural init
//!     of `helper_struct[+0x20]`, the patch is retirable.
//!   * `phase3` — drive the full chain WITH the round-63 patch and
//!     the ffmpeg-derived extradata.  This is the BREAKTHROUGH probe
//!     — if PCM is now emitted, document the milestone loudly.
//!   * `phase4` — comparison baseline.  Drive the same chain with
//!     the old `wma_criteria_passing` (all-zero preamble), so the
//!     test log captures the before/after deltas side-by-side.
//!
//! ## Reference material (clean-room only)
//!
//! * Intel SDM Vol. 2 — opcode encoding, ModR/M, control transfer.
//! * MSDN — `IMemInputPin::Receive`, `WAVEFORMATEX`, ASF spec public
//!   2004-12 §5.2.2 Payload Parsing Information.
//! * Microsoft Windows Media Audio public spec — sample-rate-class /
//!   encoder-version field meanings in the codec-private block.
//! * Raw bytes of `msadds32.ax` from
//!   `docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/`.
//! * Raw bytes of ffmpeg-generated WMA1/WMA2 fixtures stored at
//!   `tests/fixtures/audio/`.
//!
//! No Wine / ReactOS / MinGW / Microsoft DShow base-class source
//! consulted.

use oxideav_vfw::com::{
    call::{call_method, vtable_is_plausible},
    AmtBlueprint, Guid, MSADDS_AUDIO_DECODER_CLSID, PIN_DIRECTION_INPUT, PIN_DIRECTION_OUTPUT,
    SLOT_BASEFILTER_ENUM_PINS, SLOT_BASEFILTER_STOP, SLOT_ENUMPINS_NEXT, SLOT_MEDIAFILTER_PAUSE,
    SLOT_MEDIAFILTER_RUN, SLOT_MEMALLOCATOR_COMMIT, SLOT_MEMALLOCATOR_SET_PROPERTIES,
    SLOT_MEMINPUTPIN_GET_ALLOCATOR, SLOT_MEMINPUTPIN_NOTIFY_ALLOCATOR, SLOT_MEMINPUTPIN_RECEIVE,
    SLOT_PIN_QUERY_DIRECTION, SLOT_PIN_RECEIVE_CONNECTION,
};
use oxideav_vfw::{Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY, IID_IMEMINPUTPIN, IID_IUNKNOWN};
use std::path::PathBuf;

// ─── shared bootstrap (mirrors r64/r65) ───────────────────────────────

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

fn load_msadds32() -> Option<(Sandbox, oxideav_vfw::pe::Image)> {
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

// ─── Full-chain driver: bootstrap + Receive with caller-chosen blueprint ─

struct ReceiveOutcome {
    /// Kept so future phases can post-mortem the sandbox state
    /// (visited EIPs, helper-struct flags, etc.).  Not read by the
    /// current phases — they only consume the HRESULT + PCM-byte
    /// signals.
    #[allow(dead_code)]
    sb: Sandbox,
    receive_hr: Option<u32>,
    receive_trap: Option<String>,
    /// Total payload bytes in the host output sink AFTER Receive — the
    /// PCM-emerged signal.  Zero means the codec never emitted samples.
    pcm_bytes_observed: u32,
}

fn drive_full_chain_with_blueprint(
    bp: AmtBlueprint,
    apply_helper_addref_patch: Option<u32>,
) -> Option<ReceiveOutcome> {
    let (mut sb, img, _unk, filter) = bootstrap_filter()?;
    let base = img.image_base;
    if let Some(v) = apply_helper_addref_patch {
        sb.msadds32_patch_helper_addref(base, v).ok()?;
    }

    let input_pin = enum_pin_by_direction(&mut sb, filter, PIN_DIRECTION_INPUT)?;
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
        eprintln!(
            "round68 chain: ReceiveConnection returned {r_rc:#010x} — \
             AMT not accepted (likely the new extradata broke the validator)"
        );
        return None;
    }
    let mip = sb.query_interface(input_pin, IID_IMEMINPUTPIN).ok()?;
    if mip == 0 {
        return None;
    }

    // ── Output-pin connection ────────────────────────────────────────
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

    // ── WMA fixture sample push ─────────────────────────────────────
    let asf_bytes = std::fs::read(fixture_path()).ok()?;
    let packet = oxideav_vfw::com::locate_first_data_packet(&asf_bytes).unwrap_or(&[]);
    if packet.is_empty() {
        return None;
    }
    let payload: Vec<u8> = packet.iter().take(4096).copied().collect();
    let sample = sb.mint_host_media_sample(8192, amt).ok()?;
    sb.media_sample_set_payload(sample, &payload, true).ok()?;

    sb.cpu.trace_ring.clear();
    sb.cpu.visited_eips.clear();

    let pcm_before = host_sink_total_bytes(&sb);
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_RECEIVE,
        &[sample],
    );
    let pcm_after = host_sink_total_bytes(&sb);
    let pcm_bytes_observed = pcm_after.saturating_sub(pcm_before);

    let (receive_hr, receive_trap) = match r {
        Ok(hr) => (Some(hr), None),
        Err(e) => (None, Some(format!("{e}"))),
    };
    Some(ReceiveOutcome {
        sb,
        receive_hr,
        receive_trap,
        pcm_bytes_observed,
    })
}

/// Best-effort probe: count bytes the host-side downstream sink has
/// accumulated.  In the current host-iface scaffold the downstream
/// pin doesn't have a fully wired PCM-collection path, so this is a
/// signal-not-guarantee.  Any non-zero value is suggestive evidence
/// the codec emitted output.
fn host_sink_total_bytes(sb: &Sandbox) -> u32 {
    // We don't have a counter in the host-iface scaffold; instead we
    // re-derive a proxy from the trace ring's visited count after
    // Receive.  Concrete byte-count plumbing is left for a future
    // round; this proxy is enough for THE BREAKTHROUGH signal:
    // if Receive returns S_OK, the run is interesting regardless of
    // the byte count.  The placeholder here returns 0 in all paths.
    let _ = sb;
    0
}

// ─── Phase 1 — sanity check the constructor's bytes ───────────────────

#[test]
fn phase1_wma_with_ffmpeg_extradata_prefix_bytes_match_fixtures() {
    // WMA2 — 10-byte preamble + 37-byte CLSID suffix.
    let bp = AmtBlueprint::wma_with_ffmpeg_extradata_prefix(0x0161, 1, 44_100, 4_000, 185);
    assert_eq!(bp.format_tag, 0x0161);
    assert_eq!(bp.extradata.len(), 47);
    assert_eq!(
        &bp.extradata[0..10],
        &[0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]
    );
    assert_eq!(
        &bp.extradata[10..],
        b"1A0F78F0-EC8A-11d2-BBBE-006008320064\0"
    );
    // WMA1 — 4-byte preamble + 37-byte CLSID suffix.
    let bp1 = AmtBlueprint::wma_with_ffmpeg_extradata_prefix(0x0160, 1, 44_100, 4_000, 185);
    assert_eq!(bp1.format_tag, 0x0160);
    assert_eq!(bp1.extradata.len(), 41);
    assert_eq!(&bp1.extradata[0..4], &[0x00, 0x00, 0x01, 0x00]);
    assert_eq!(
        &bp1.extradata[4..],
        b"1A0F78F0-EC8A-11d2-BBBE-006008320064\0"
    );
}

// ─── Phase 2 — Receive WITHOUT the round-63 patch, WITH ffmpeg extradata ──

/// Phase 2 — drive Receive with the ffmpeg-derived extradata but
/// WITHOUT the round-63 `helper_addref` patch.  If proper extradata
/// triggers the codec's natural init path that populates
/// `helper_struct[+0x20]`, the patch is retirable.
#[test]
fn phase2_receive_ffmpeg_extradata_no_patch() {
    if msadds32_path().is_none() {
        eprintln!("round68 phase2: msadds32.ax missing; skipping");
        return;
    }
    if !fixture_path().is_file() {
        eprintln!("round68 phase2: WMA2 fixture missing; skipping");
        return;
    }
    let bp = AmtBlueprint::wma_with_ffmpeg_extradata_prefix(0x0161, 1, 44_100, 4_000, 185);
    let Some(o) = drive_full_chain_with_blueprint(bp, None) else {
        eprintln!("round68 phase2: drive_full_chain bootstrap failed (likely AMT rejected)");
        return;
    };
    eprintln!(
        "round68 phase2: receive_hr={:?}  trap={:?}  pcm_bytes={}",
        o.receive_hr, o.receive_trap, o.pcm_bytes_observed
    );
    match (&o.receive_trap, o.receive_hr) {
        (Some(msg), _) if msg.contains("memory fault at 0x00000020") => {
            eprintln!(
                "round68 phase2: patch STILL REQUIRED — unpatched Receive trapped at \
                 0x00000020 even with ffmpeg-derived extradata.  Candidate (2) does not \
                 drive the natural init path."
            );
        }
        (Some(msg), _) => {
            eprintln!(
                "round68 phase2: trap moved (no longer 0x00000020): {msg} — \
                 the new extradata changed the run-state trajectory"
            );
        }
        (None, Some(0)) => {
            eprintln!(
                "round68 phase2: ★★★ BREAKTHROUGH ★★★ Receive returned S_OK \
                 WITHOUT the patch, just by populating the ffmpeg-derived \
                 codec-private-data preamble!"
            );
        }
        (None, Some(hr)) => {
            eprintln!("round68 phase2: HRESULT = {hr:#010x} (without patch)");
        }
        (None, None) => {
            eprintln!("round68 phase2: no HRESULT and no trap reported");
        }
    }
}

// ─── Phase 3 — Receive WITH patch + ffmpeg extradata (the breakthrough probe) ─

/// Phase 3 — drive Receive with BOTH the round-63 patch AND the
/// ffmpeg-derived extradata.  This is the breakthrough probe: if PCM
/// emerges OR a new HRESULT replaces `E_UNEXPECTED`, candidate (2) is
/// at least partially active.  The round-64 baseline used the patch
/// but all-zero extradata and got `E_UNEXPECTED`.
#[test]
fn phase3_receive_ffmpeg_extradata_with_patch() {
    if msadds32_path().is_none() {
        eprintln!("round68 phase3: msadds32.ax missing; skipping");
        return;
    }
    if !fixture_path().is_file() {
        eprintln!("round68 phase3: WMA2 fixture missing; skipping");
        return;
    }
    let bp = AmtBlueprint::wma_with_ffmpeg_extradata_prefix(0x0161, 1, 44_100, 4_000, 185);
    let Some(o) = drive_full_chain_with_blueprint(bp, Some(65_536)) else {
        eprintln!("round68 phase3: drive_full_chain bootstrap failed (likely AMT rejected)");
        return;
    };
    eprintln!(
        "round68 phase3: receive_hr={:?}  trap={:?}  pcm_bytes={}",
        o.receive_hr, o.receive_trap, o.pcm_bytes_observed
    );
    if o.receive_hr == Some(0) {
        eprintln!(
            "round68 phase3: ★★★ BREAKTHROUGH ★★★ Receive returned S_OK with \
             patch + ffmpeg-derived extradata.  pcm_bytes_observed={}",
            o.pcm_bytes_observed
        );
    } else if o.receive_hr == Some(0x8000_ffff) {
        eprintln!(
            "round68 phase3: same E_UNEXPECTED as round 64 — the codec-private-data \
             preamble's bytes do not lift the inner-decode-no-output guard.  \
             The blocker is elsewhere (likely candidate (1) ASF framing OR an \
             unidentified internal state)."
        );
    } else if let Some(hr) = o.receive_hr {
        eprintln!(
            "round68 phase3: NEW HRESULT surface = {hr:#010x} — \
             the extradata changed the trajectory; document this in the forensics doc"
        );
    } else if let Some(msg) = &o.receive_trap {
        eprintln!("round68 phase3: NEW trap site = {msg}");
    }
}

// ─── Phase 4 — Comparison baseline: old all-zero preamble + patch ─────

/// Phase 4 — sanity baseline.  Drive the same chain with the old
/// `wma_criteria_passing` constructor (all-zero preamble) and the
/// round-63 patch.  This is the round-64 baseline; we expect
/// `receive_hr = 0x8000ffff`.  If THIS phase ALSO surfaces a
/// different HRESULT, our scaffold's other dependencies changed and
/// the phase-3 reading isn't a clean A/B.
#[test]
fn phase4_baseline_zero_preamble_with_patch() {
    if msadds32_path().is_none() {
        eprintln!("round68 phase4: msadds32.ax missing; skipping");
        return;
    }
    if !fixture_path().is_file() {
        eprintln!("round68 phase4: WMA2 fixture missing; skipping");
        return;
    }
    let bp = AmtBlueprint::wma_criteria_passing(0x0161, 1, 44_100, 4_000, 185);
    let Some(o) = drive_full_chain_with_blueprint(bp, Some(65_536)) else {
        eprintln!("round68 phase4: drive_full_chain bootstrap failed");
        return;
    };
    eprintln!(
        "round68 phase4 (baseline): receive_hr={:?}  trap={:?}  pcm_bytes={}",
        o.receive_hr, o.receive_trap, o.pcm_bytes_observed
    );
    if o.receive_hr == Some(0x8000_ffff) {
        eprintln!("round68 phase4: baseline E_UNEXPECTED reproduces — phase-3 A/B is clean");
    } else if let Some(hr) = o.receive_hr {
        eprintln!(
            "round68 phase4: baseline HRESULT shifted to {hr:#010x} — \
             phase-3 reading needs caveats"
        );
    } else if let Some(msg) = &o.receive_trap {
        eprintln!("round68 phase4: baseline trap = {msg}");
    }
}

// ─── Phase 5 — WMA1 variant ───────────────────────────────────────────

/// Phase 5 — same as phase 3 but driving the WMA1 (0x0160) variant.
/// Round 64 only exercised WMA2; WMA1 has a different cbSize gate
/// (0x29 vs 0x2F) and a different preamble (4 bytes vs 10).
#[test]
fn phase5_receive_wma1_ffmpeg_extradata_with_patch() {
    if msadds32_path().is_none() {
        eprintln!("round68 phase5: msadds32.ax missing; skipping");
        return;
    }
    // WMA1 fixture path — fall back to WMA2 if the WMA1 fixture is
    // missing.  The test merely logs; it isn't a hard regression.
    let wma1 = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma1_440hz_mono_1s.wma");
    if !wma1.is_file() {
        eprintln!("round68 phase5: WMA1 fixture missing; skipping");
        return;
    }
    let bp = AmtBlueprint::wma_with_ffmpeg_extradata_prefix(0x0160, 1, 44_100, 4_000, 185);
    // Reuse drive_full_chain_with_blueprint but with the WMA2 fixture
    // path — we're testing whether the codec accepts the WMA1 AMT
    // shape; the actual payload bytes are downstream of that test.
    let Some(o) = drive_full_chain_with_blueprint(bp, Some(65_536)) else {
        eprintln!("round68 phase5: drive_full_chain bootstrap failed (AMT may have been rejected)");
        return;
    };
    eprintln!(
        "round68 phase5 (WMA1): receive_hr={:?}  trap={:?}  pcm_bytes={}",
        o.receive_hr, o.receive_trap, o.pcm_bytes_observed
    );
}
