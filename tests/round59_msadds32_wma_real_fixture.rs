//! Round 59 — feed the `msadds32.ax` audio splitter a `WAVEFORMATEX`
//! lifted from a REAL `.wma` ASF fixture, then retry the round-58
//! `IPin::ReceiveConnection` path.
//!
//! Round 58 closed by demonstrating that ReceiveConnection on the
//! splitter's input pin rejects every synthetic AM_MEDIA_TYPE
//! whose `WAVEFORMATEX::cbSize`-bytes-of-extradata blob is
//! all-zero — the splitter validates those bytes against the
//! WMA1/WMA2 header constants its bitstream parser expects.
//!
//! Round 59 attacks that:
//!
//! * **Phase 1** lifts the WAVEFORMATEX+extradata blueprint out of
//!   `tests/fixtures/audio/wma1_440hz_mono_1s.wma` and the
//!   matching WMA2 fixture, asserting the parser surfaces the
//!   spec-cited `wFormatTag` constants `0x0160` / `0x0161` and
//!   non-zero extradata.
//!
//! * **Phase 2** restages each blueprint into a guest-side
//!   `AM_MEDIA_TYPE` and walks the round-58 ReceiveConnection
//!   path against the audio splitter's input pin.  Outcome (S_OK
//!   or HRESULT) is captured in the test stderr; the test does
//!   not assert any specific HRESULT — the actual splitter
//!   reaction is the deliverable.
//!
//! * **Phase 3 (stretch)** — if Phase 2 accepts an AMT, locate
//!   the first data packet inside the ASF Data Object, push it
//!   through `IMemInputPin::Receive`, and read the decoded PCM
//!   bytes off the host-side output sink.  PSNR-vs-ffmpeg is r60+
//!   work.
//!
//! ## Reference material (clean-room only)
//!
//! * Microsoft Advanced Systems Format (ASF) Specification rev
//!   01.20.05 (public; no NDA).  §3 (top-level objects), §3.3
//!   (Stream Properties Object), §11.1 (GUID values).
//! * Microsoft multimedia registry `mmreg.h` for the
//!   `WAVEFORMATEX` layout and `WAVE_FORMAT_MSAUDIO1` /
//!   `WAVE_FORMAT_WMAUDIO2` constants.
//! * MSDN DirectShow `IPin::ReceiveConnection` /
//!   `IMemInputPin::Receive` interface references.
//!
//! No Wine / ReactOS / MinGW / Microsoft DShow / ffmpeg WMA source
//! is consulted.  The `.wma` fixtures themselves are produced by
//! ffmpeg as an opaque byte-stream generator; ffmpeg version +
//! invocation command are documented in
//! `tests/fixtures/audio/HOWTO.md`.

use oxideav_vfw::com::{
    call::{call_method, vtable_is_plausible},
    extract_wma_amt_from_asf, AmtBlueprint, Guid, MSADDS_AUDIO_DECODER_CLSID, PIN_DIRECTION_INPUT,
    SLOT_BASEFILTER_ENUM_PINS, SLOT_BASEFILTER_STOP, SLOT_ENUMPINS_NEXT, SLOT_MEDIAFILTER_PAUSE,
    SLOT_MEDIAFILTER_RUN, SLOT_MEMINPUTPIN_RECEIVE, SLOT_PIN_QUERY_DIRECTION,
    SLOT_PIN_RECEIVE_CONNECTION,
};
use oxideav_vfw::{Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY, IID_IMEMINPUTPIN, IID_IUNKNOWN};
use std::path::PathBuf;

// ---- fixture helpers -------------------------------------------------

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

fn wma1_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio/wma1_440hz_mono_1s.wma")
}

fn wma2_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio/wma2_440hz_mono_1s.wma")
}

fn read_fixture(p: &PathBuf) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| panic!("round59 fixture missing at {}: {e}", p.display()))
}

// ---- AMT staging using a real AmtBlueprint ---------------------------

/// Audio MEDIATYPE GUID — `{73647561-0000-0010-8000-00AA00389B71}`.
fn mediatype_audio() -> Guid {
    Guid::parse("{73647561-0000-0010-8000-00AA00389B71}").unwrap()
}

/// FORMAT_WaveFormatEx GUID — `{05589F81-C356-11CE-BF01-00AA0055595A}`.
fn format_wave_format_ex() -> Guid {
    Guid::parse("{05589F81-C356-11CE-BF01-00AA0055595A}").unwrap()
}

/// MEDIASUBTYPE GUID for an audio `wFormatTag`:
/// `{<tag>:08X-0000-0010-8000-00AA00389B71}`.
fn audio_subtype_for_format_tag(tag: u16) -> Guid {
    Guid::new(
        tag as u32,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    )
}

/// Stage an `AM_MEDIA_TYPE` whose `WAVEFORMATEX` is populated from
/// `bp` (rather than the synthetic zero blob round-58 used).
/// Returns the staged AMT's guest VA.
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

    mediatype_audio().stage(&mut sb.mmu, amt).map_err(trap)?;
    audio_subtype_for_format_tag(bp.format_tag)
        .stage(&mut sb.mmu, amt + 16)
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 32, &0u32.to_le_bytes())
        .map_err(trap)?; // bFixedSizeSamples
    sb.mmu
        .write_initializer(amt + 36, &1u32.to_le_bytes())
        .map_err(trap)?; // bTemporalCompression
    sb.mmu
        .write_initializer(amt + 40, &0u32.to_le_bytes())
        .map_err(trap)?; // lSampleSize
    format_wave_format_ex()
        .stage(&mut sb.mmu, amt + 44)
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 60, &0u32.to_le_bytes())
        .map_err(trap)?; // pUnk
    sb.mmu
        .write_initializer(amt + 64, &wfx_len.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 68, &fmt.to_le_bytes())
        .map_err(trap)?;

    // WAVEFORMATEX
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

// ---- splitter bootstrap (copied from round 58) -----------------------

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

fn walk_pins(sb: &mut Sandbox, filter: u32) -> Vec<(u32, u32)> {
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_BASEFILTER_STOP,
        &[],
    );
    let scratch = match sb.host.arena_alloc(4) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    if sb.mmu.write_initializer(scratch, &[0u8; 4]).is_err() {
        return Vec::new();
    }
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_BASEFILTER_ENUM_PINS,
        &[scratch],
    );
    let pp = sb.mmu.load32(scratch).unwrap_or(0);
    if !matches!(r, Ok(0)) || pp == 0 {
        return Vec::new();
    }
    sb.host.com.intern(pp, None);
    let mut pins: Vec<(u32, u32)> = Vec::new();
    for _ in 0..16 {
        let pin_slot = match sb.host.arena_alloc(8) {
            Ok(p) => p,
            Err(_) => break,
        };
        if sb.mmu.write_initializer(pin_slot, &[0u8; 8]).is_err() {
            break;
        }
        let r = call_method(
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
        let terminal = !matches!(r, Ok(0));
        if pin != 0 && fetched == 1 {
            sb.host.com.intern(pin, None);
            let dir_slot = match sb.host.arena_alloc(4) {
                Ok(p) => p,
                Err(_) => break,
            };
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
            pins.push((pin, dir));
        }
        if terminal {
            break;
        }
    }
    let _ = sb.com_release(pp);
    pins
}

// ─── Phase 1 ──────────────────────────────────────────────────────

/// Phase 1 (a) — the WMA1 fixture parses to a blueprint whose
/// `wFormatTag` is `WAVE_FORMAT_MSAUDIO1` (`0x0160`), with non-zero
/// extradata of length matching the `cbSize` field on the wire.
#[test]
fn phase1_wma1_fixture_extracts_format_tag_0x0160() {
    let bytes = read_fixture(&wma1_fixture_path());
    let bp = extract_wma_amt_from_asf(&bytes).expect("WMA1 fixture parse");
    eprintln!("round59 phase1 (WMA1): {bp:?}");
    assert_eq!(bp.format_tag, 0x0160);
    assert_eq!(bp.n_channels, 1);
    assert_eq!(bp.n_samples_per_sec, 44_100);
    // ffmpeg's `wmav1` encoder emits the codec's standard 4-byte
    // bootstrap header for the test bitrate; verify cbSize > 0
    // rather than pinning a specific length, because future
    // ffmpeg releases may expand the header.
    assert!(
        !bp.extradata.is_empty(),
        "WMA1 fixture has empty extradata — codec rejection root cause"
    );
    assert!(
        bp.extradata.iter().any(|&b| b != 0),
        "WMA1 fixture extradata is all zeros — would re-hit the round-58 E_FAIL"
    );
}

/// Phase 1 (b) — the WMA2 fixture parses to a blueprint whose
/// `wFormatTag` is `WAVE_FORMAT_WMAUDIO2` (`0x0161`).
#[test]
fn phase1_wma2_fixture_extracts_format_tag_0x0161() {
    let bytes = read_fixture(&wma2_fixture_path());
    let bp = extract_wma_amt_from_asf(&bytes).expect("WMA2 fixture parse");
    eprintln!("round59 phase1 (WMA2): {bp:?}");
    assert_eq!(bp.format_tag, 0x0161);
    assert_eq!(bp.n_channels, 1);
    assert_eq!(bp.n_samples_per_sec, 44_100);
    assert!(!bp.extradata.is_empty());
    assert!(bp.extradata.iter().any(|&b| b != 0));
}

// ─── Phase 2 ──────────────────────────────────────────────────────

/// Phase 2 — stage the WMA1 blueprint into a guest-side
/// AM_MEDIA_TYPE, confirm round-trip readback of every field.  No
/// codec dependency — this exercises the host-only staging path.
#[test]
fn phase2_staged_amt_round_trips_every_wave_format_ex_field() {
    let bytes = read_fixture(&wma1_fixture_path());
    let bp = extract_wma_amt_from_asf(&bytes).expect("WMA1 fixture parse");
    let mut sb = Sandbox::new();
    let amt = stage_audio_amt_from_blueprint(&mut sb, &bp).expect("stage WMA1 AMT");
    assert_eq!(Guid::load(&sb.mmu, amt).unwrap(), mediatype_audio());
    assert_eq!(
        Guid::load(&sb.mmu, amt + 16).unwrap(),
        audio_subtype_for_format_tag(bp.format_tag)
    );
    assert_eq!(
        sb.mmu.load32(amt + 64).unwrap(),
        18 + bp.extradata.len() as u32
    );
    let fmt = sb.mmu.load32(amt + 68).unwrap();
    assert_ne!(fmt, 0);
    let read_u16 = |off: u32| {
        u16::from_le_bytes([
            sb.mmu.load8(fmt + off).unwrap(),
            sb.mmu.load8(fmt + off + 1).unwrap(),
        ])
    };
    let read_u32 = |off: u32| {
        u32::from_le_bytes([
            sb.mmu.load8(fmt + off).unwrap(),
            sb.mmu.load8(fmt + off + 1).unwrap(),
            sb.mmu.load8(fmt + off + 2).unwrap(),
            sb.mmu.load8(fmt + off + 3).unwrap(),
        ])
    };
    assert_eq!(read_u16(0), bp.format_tag);
    assert_eq!(read_u16(2), bp.n_channels);
    assert_eq!(read_u32(4), bp.n_samples_per_sec);
    assert_eq!(read_u32(8), bp.n_avg_bytes_per_sec);
    assert_eq!(read_u16(12), bp.n_block_align);
    assert_eq!(read_u16(14), bp.w_bits_per_sample);
    assert_eq!(read_u16(16) as usize, bp.extradata.len());
    // Extradata round-trips byte-for-byte.
    for (i, b) in bp.extradata.iter().enumerate() {
        assert_eq!(sb.mmu.load8(fmt + 18 + i as u32).unwrap(), *b);
    }
}

// ─── Phase 3 ──────────────────────────────────────────────────────

/// Phase 3 — drive `IPin::ReceiveConnection` against the real
/// `msadds32.ax` audio splitter input pin, using the
/// WMA1-fixture-derived `WAVEFORMATEX`.  No assertion on the
/// returned HRESULT — we record the splitter's reaction (S_OK /
/// E_FAIL / other) on stderr for the round-59 report.
///
/// Skipped if `msadds32.ax` is not present.
#[test]
fn phase3_receive_connection_with_real_wma1_extradata_reports_outcome() {
    let bytes = read_fixture(&wma1_fixture_path());
    let bp = extract_wma_amt_from_asf(&bytes).expect("WMA1 fixture parse");
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round59 phase3: msadds32.ax missing; skipping");
        return;
    };
    let pins = walk_pins(&mut sb, filter);
    let Some(input_pin) = pins
        .iter()
        .find_map(|(p, d)| (*d == PIN_DIRECTION_INPUT).then_some(*p))
    else {
        eprintln!("round59 phase3: no INPUT pin; skipping");
        return;
    };
    let amt = stage_audio_amt_from_blueprint(&mut sb, &bp).expect("stage WMA1 AMT");
    let host_out = sb
        .mint_host_output_pin_with_connection(amt, input_pin)
        .expect("mint host output pin");
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        input_pin,
        SLOT_PIN_RECEIVE_CONNECTION,
        &[host_out, amt],
    )
    .expect("ReceiveConnection (real WMA1 extradata) must not trap");
    eprintln!(
        "round59 phase3: ReceiveConnection (real WMA1 extradata, \
         wFormatTag={tag:#06x}, cbSize={cb}, extradata[0..min(8)]={extra:02x?}) → \
         HRESULT {r:#010x}",
        tag = bp.format_tag,
        cb = bp.extradata.len(),
        extra = &bp.extradata[..bp.extradata.len().min(8)],
    );
    // No hard assertion on the returned HRESULT — Phase 3's
    // deliverable is the empirical reaction, not codec
    // compatibility.  If r==S_OK we proceed to Phase 4 in the
    // dedicated test below.
}

/// Same as `phase3_*` but feeds the splitter the WMA2 blueprint.
#[test]
fn phase3_receive_connection_with_real_wma2_extradata_reports_outcome() {
    let bytes = read_fixture(&wma2_fixture_path());
    let bp = extract_wma_amt_from_asf(&bytes).expect("WMA2 fixture parse");
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round59 phase3: msadds32.ax missing; skipping");
        return;
    };
    let pins = walk_pins(&mut sb, filter);
    let Some(input_pin) = pins
        .iter()
        .find_map(|(p, d)| (*d == PIN_DIRECTION_INPUT).then_some(*p))
    else {
        eprintln!("round59 phase3: no INPUT pin; skipping");
        return;
    };
    let amt = stage_audio_amt_from_blueprint(&mut sb, &bp).expect("stage WMA2 AMT");
    let host_out = sb
        .mint_host_output_pin_with_connection(amt, input_pin)
        .expect("mint host output pin");
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        input_pin,
        SLOT_PIN_RECEIVE_CONNECTION,
        &[host_out, amt],
    )
    .expect("ReceiveConnection (real WMA2 extradata) must not trap");
    eprintln!(
        "round59 phase3: ReceiveConnection (real WMA2 extradata, \
         wFormatTag={tag:#06x}, cbSize={cb}, extradata[0..min(8)]={extra:02x?}) → \
         HRESULT {r:#010x}",
        tag = bp.format_tag,
        cb = bp.extradata.len(),
        extra = &bp.extradata[..bp.extradata.len().min(8)],
    );
}

// ─── Phase 4 (stretch) ─────────────────────────────────────────────

/// Phase 4 stretch — if any AMT was accepted in Phase 3, locate
/// the first ASF Data Packet payload, push it through
/// `IMemInputPin::Receive`, and report what came out.  This is
/// purely diagnostic in r59: no PCM assertion (PSNR-vs-ffmpeg is
/// r60+ work), just "did the codec take the bytes without
/// trapping, and how many PCM bytes (if any) surfaced on the host
/// sink".
#[test]
fn phase4_push_first_data_packet_through_receive_reports_state() {
    let bytes = read_fixture(&wma2_fixture_path());
    let bp = extract_wma_amt_from_asf(&bytes).expect("WMA2 fixture parse");
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round59 phase4: msadds32.ax missing; skipping");
        return;
    };
    let pins = walk_pins(&mut sb, filter);
    let Some(input_pin) = pins
        .iter()
        .find_map(|(p, d)| (*d == PIN_DIRECTION_INPUT).then_some(*p))
    else {
        eprintln!("round59 phase4: no INPUT pin; skipping");
        return;
    };
    let amt = stage_audio_amt_from_blueprint(&mut sb, &bp).expect("stage WMA2 AMT");
    let host_out = sb
        .mint_host_output_pin_with_connection(amt, input_pin)
        .expect("mint host output pin");
    let rc = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        input_pin,
        SLOT_PIN_RECEIVE_CONNECTION,
        &[host_out, amt],
    )
    .expect("ReceiveConnection must not trap");
    if rc != 0 {
        eprintln!(
            "round59 phase4: ReceiveConnection rejected (HRESULT {rc:#010x}); \
             cannot push samples — recording as r59 partial-progress"
        );
        return;
    }
    eprintln!("round59 phase4: ReceiveConnection ACCEPTED — pushing first data packet");
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
    let mip = match sb.query_interface(input_pin, IID_IMEMINPUTPIN) {
        Ok(p) if p != 0 => p,
        _ => {
            eprintln!("round59 phase4: QI(IMemInputPin) failed");
            return;
        }
    };
    let packet = oxideav_vfw::com::locate_first_data_packet(&bytes).unwrap_or(&[]);
    eprintln!(
        "round59 phase4: first data packet candidate = {} bytes",
        packet.len()
    );
    if packet.is_empty() {
        eprintln!("round59 phase4: no data packet found; skipping push");
        return;
    }
    // Cap at 4 KiB — enough for one fixed-size packet of the
    // small synthetic fixture.
    let payload_bytes: Vec<u8> = packet.iter().take(4096).copied().collect();
    let sample = sb
        .mint_host_media_sample(/*data_capacity=*/ 8192, amt)
        .expect("mint host media sample");
    sb.media_sample_set_payload(sample, &payload_bytes, /*sync_point=*/ true)
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
    match r {
        Ok(hr) => eprintln!(
            "round59 phase4: IMemInputPin::Receive({} B encoded) → HRESULT {hr:#010x}",
            payload_bytes.len()
        ),
        Err(e) => eprintln!("round59 phase4: IMemInputPin::Receive trapped: {e}"),
    }
    let pcm_queued = oxideav_vfw::com::host_iface_r31::queue_len(&sb.host);
    eprintln!("round59 phase4: PCM samples queued on host sink = {pcm_queued}");
}
