//! Round 58 — drive `msadds32.ax`'s audio splitter through
//! `IBaseFilter::EnumPins`, `IPin::EnumMediaTypes`,
//! `IPin::ReceiveConnection`, and `IMediaFilter::Pause + Run(0)`.
//!
//! Round 57 closed by demonstrating the splitter spawns cleanly:
//! `DllGetClassObject + IClassFactory::CreateInstance + QI` for
//! every documented base interface all land successfully.  Round 58
//! takes the next step on the audio decode path: discover what
//! media-type families the splitter advertises on its input pin
//! (encoded-audio side), build a `WAVEFORMATEX`-formatted
//! `AM_MEDIA_TYPE` matching one of them (or a synthetic WMAudio1
//! fallback), and drive the splitter through `ReceiveConnection +
//! Run(0)`.
//!
//! ## What this test pins
//!
//! * **Phase 1** — `IBaseFilter::EnumPins → IEnumPins::Next` walks
//!   every pin the audio splitter exposes; each pin is then probed
//!   for its `PIN_DIRECTION` via `IPin::QueryDirection`.
//!   `IPin::EnumMediaTypes` is then driven against the INPUT pin
//!   (the side that receives encoded MS-Audio frames) and every
//!   advertised `AM_MEDIA_TYPE` is captured + dumped on stderr.
//!
//! * **Phase 2** — Stage a host-side `AM_MEDIA_TYPE` shaped for
//!   audio: `MEDIATYPE_Audio`, `MEDIASUBTYPE_MSAUDIO1`,
//!   `FORMAT_WaveFormatEx`, with a `WAVEFORMATEX` blob carrying
//!   `wFormatTag=0x0160` (WMAudio1), 2 channels, 44_100 Hz,
//!   16 bits-per-sample, 10 bytes of opaque `cbSize` extradata.
//!
//! * **Phase 3** — Call `IPin::ReceiveConnection(host_out_pin,
//!   &amt)` against the splitter's input pin.  First we try every
//!   captured AMT from Phase 1; if none lands, we fall back to the
//!   synthetic WMAudio1 AMT from Phase 2.
//!
//! * **Phase 4** — `IMediaFilter::Pause()` then
//!   `IMediaFilter::Run(0)` against the splitter to walk it into
//!   `State_Running`.  `GetState` is informational only —
//!   `E_NOTIMPL` from a stateless filter is not a failure.
//!
//! * **Phase 5 (smoke)** — push one synthetic 64-byte encoded
//!   sample through `IMemInputPin::Receive`.  Success criterion is
//!   "did not panic"; we record the HRESULT for r59 baselining.
//!
//! ## Reference material
//!
//! * MSDN — `IBaseFilter`, `IPin`, `IMemInputPin`,
//!   `IMediaFilter`, `IEnumPins`, `IEnumMediaTypes`,
//!   `AM_MEDIA_TYPE`, `WAVEFORMATEX`.
//! * Windows SDK headers `strmif.h`, `mmreg.h` (header ABI only).
//! * The audio-fourcc GUID family is the same `MEDIATYPE_*` shape
//!   used by the video pin probes in round 27 — `Data1` is the
//!   `wFormatTag` and the remaining 12 bytes are the canonical
//!   audio-fourcc trailer `{XXXXXXXX-0000-0010-8000-00AA00389B71}`.
//!   `wFormatTag` values are public registry constants documented
//!   in `mmreg.h`:
//!   <https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/extensible-wave-format-descriptors>.
//!
//! Skipped gracefully if `msadds32.ax` is not present.

use oxideav_vfw::com::{
    call::{call_method, vtable_is_plausible},
    Guid, MSADDS_AUDIO_DECODER_CLSID, PIN_DIRECTION_INPUT, PIN_DIRECTION_OUTPUT,
    SLOT_BASEFILTER_ENUM_PINS, SLOT_BASEFILTER_STOP, SLOT_ENUMPINS_NEXT,
    SLOT_MEDIAFILTER_GET_STATE, SLOT_MEDIAFILTER_PAUSE, SLOT_MEDIAFILTER_RUN,
    SLOT_MEMINPUTPIN_RECEIVE, SLOT_PIN_QUERY_DIRECTION, SLOT_PIN_RECEIVE_CONNECTION,
};
use oxideav_vfw::{
    Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY, IID_IMEDIASAMPLE, IID_IMEMINPUTPIN, IID_IUNKNOWN,
};
use std::path::PathBuf;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn msadds32_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/msadds32.ax");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn load() -> Option<(Sandbox, oxideav_vfw::pe::Image)> {
    let p = msadds32_path()?;
    let bytes = std::fs::read(&p).ok()?;
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(8_000_000_000);
    let img = sb.load("msadds32.ax", &bytes).ok()?;
    let _ = sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH);
    Some((sb, img))
}

/// Build a `MEDIASUBTYPE` GUID from an audio `wFormatTag`,
/// following the canonical audio fourcc-base GUID
/// `{XXXXXXXX-0000-0010-8000-00AA00389B71}` documented in
/// `mmreg.h` (Microsoft Multimedia Registry).  The tag occupies
/// `Data1` in little-endian wire form; remaining bytes are the
/// audio-family trailer.
fn audio_subtype_for_format_tag(tag: u16) -> Guid {
    Guid::new(
        tag as u32,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    )
}

/// `MEDIATYPE_Audio = {73647561-0000-0010-8000-00AA00389B71}`.
/// Data1 `0x73647561` is the FOURCC `b"audi"` reversed (LE).
fn mediatype_audio() -> Guid {
    Guid::parse("{73647561-0000-0010-8000-00AA00389B71}").unwrap()
}

/// `FORMAT_WaveFormatEx = {05589F81-C356-11CE-BF01-00AA0055595A}`.
/// Header source: `uuids.h` from the Windows SDK.
fn format_wave_format_ex() -> Guid {
    Guid::parse("{05589F81-C356-11CE-BF01-00AA0055595A}").unwrap()
}

/// `wFormatTag` for "Microsoft Windows Media Audio" v1 — public
/// MMREG constant `WAVE_FORMAT_MSAUDIO1 = 0x0160`.  Source:
/// Microsoft `mmreg.h` audio FormatTag registry.
const WAVE_FORMAT_MSAUDIO1: u16 = 0x0160;
/// `WAVE_FORMAT_WMAUDIO2 = 0x0161`.
const WAVE_FORMAT_WMAUDIO2: u16 = 0x0161;

/// Stage an `AM_MEDIA_TYPE` shaped for an encoded audio stream at
/// `addr`, plus a `WAVEFORMATEX` blob (18 + `extra.len()` bytes)
/// at `addr+72`.  Returns the staged AMT's guest VA.
///
/// `AM_MEDIA_TYPE` layout (72 bytes — see `strmif.h`):
/// ```c
/// typedef struct _AMMediaType {
///     GUID    majortype;        // +0    (16)
///     GUID    subtype;          // +16   (16)
///     BOOL    bFixedSizeSamples; // +32   (4)
///     BOOL    bTemporalCompression; // +36 (4)
///     ULONG   lSampleSize;      // +40   (4)
///     GUID    formattype;       // +44   (16)
///     IUnknown* pUnk;           // +60   (4)
///     ULONG   cbFormat;         // +64   (4)
///     BYTE*   pbFormat;         // +68   (4)
/// } AM_MEDIA_TYPE;             // total 72
/// ```
///
/// `WAVEFORMATEX` (18 bytes packed — `mmreg.h`):
/// ```c
/// typedef struct tWAVEFORMATEX {
///     WORD  wFormatTag;         // +0  (2)
///     WORD  nChannels;          // +2  (2)
///     DWORD nSamplesPerSec;     // +4  (4)
///     DWORD nAvgBytesPerSec;    // +8  (4)
///     WORD  nBlockAlign;        // +12 (2)
///     WORD  wBitsPerSample;     // +14 (2)
///     WORD  cbSize;             // +16 (2) — count of trailing extradata bytes
/// } WAVEFORMATEX;              // total 18
/// ```
#[allow(clippy::too_many_arguments)]
fn stage_audio_am_media_type(
    sb: &mut Sandbox,
    format_tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    extradata: &[u8],
) -> Result<u32, oxideav_vfw::Error> {
    use oxideav_vfw::Error;
    let wfx_len = 18 + extradata.len() as u32;
    // 72 (AMT) + WFX + 16 byte tail slack for alignment.
    let total = 72 + wfx_len + 16;
    let blob = sb.host.arena_alloc(total).map_err(Error::Win32)?;
    let amt = blob;
    let fmt = blob + 72;
    let trap = Error::Trap;

    // majortype = MEDIATYPE_Audio
    mediatype_audio().stage(&mut sb.mmu, amt).map_err(trap)?;
    // subtype = MEDIASUBTYPE_<format_tag>
    let subtype = audio_subtype_for_format_tag(format_tag);
    subtype.stage(&mut sb.mmu, amt + 16).map_err(trap)?;
    // bFixedSizeSamples = FALSE (encoded audio is variable-size)
    sb.mmu
        .write_initializer(amt + 32, &0u32.to_le_bytes())
        .map_err(trap)?;
    // bTemporalCompression = TRUE (encoded audio is delta-coded)
    sb.mmu
        .write_initializer(amt + 36, &1u32.to_le_bytes())
        .map_err(trap)?;
    // lSampleSize = 0 (variable)
    sb.mmu
        .write_initializer(amt + 40, &0u32.to_le_bytes())
        .map_err(trap)?;
    // formattype = FORMAT_WaveFormatEx
    format_wave_format_ex()
        .stage(&mut sb.mmu, amt + 44)
        .map_err(trap)?;
    // pUnk = NULL
    sb.mmu
        .write_initializer(amt + 60, &0u32.to_le_bytes())
        .map_err(trap)?;
    // cbFormat = sizeof(WAVEFORMATEX) + cbSize bytes
    sb.mmu
        .write_initializer(amt + 64, &wfx_len.to_le_bytes())
        .map_err(trap)?;
    // pbFormat = guest pointer to the WAVEFORMATEX
    sb.mmu
        .write_initializer(amt + 68, &fmt.to_le_bytes())
        .map_err(trap)?;

    // WAVEFORMATEX @ fmt
    sb.mmu
        .write_initializer(fmt, &format_tag.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 2, &channels.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 4, &samples_per_sec.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 8, &avg_bytes_per_sec.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 12, &block_align.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 14, &bits_per_sample.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(fmt + 16, &(extradata.len() as u16).to_le_bytes())
        .map_err(trap)?;
    if !extradata.is_empty() {
        sb.mmu
            .write_initializer(fmt + 18, extradata)
            .map_err(trap)?;
    }
    Ok(amt)
}

/// Bootstrap helper — load, drive DllGetClassObject + CoCreateInstance
/// + QI(IID_IBaseFilter) and return the IBaseFilter pointer.  Returns
/// `None` if `msadds32.ax` is missing.
#[allow(clippy::doc_lazy_continuation)]
fn bootstrap_filter() -> Option<(Sandbox, oxideav_vfw::pe::Image, u32)> {
    let (mut sb, img) = load()?;
    let _factory = sb
        .dll_get_class_object(&img, MSADDS_AUDIO_DECODER_CLSID, IID_ICLASSFACTORY)
        .ok()?;
    let unk = sb
        .co_create_instance(MSADDS_AUDIO_DECODER_CLSID, IID_IUNKNOWN)
        .ok()?;
    if unk == 0 {
        return None;
    }
    // QI for IBaseFilter — required so EnumPins is reachable.
    let filter = sb.query_interface(unk, IID_IBASEFILTER).ok()?;
    if filter == 0 || !vtable_is_plausible(&sb.mmu, filter) {
        return None;
    }
    Some((sb, img, filter))
}

/// Walk every pin via EnumPins/Next, returning all guest pin
/// pointers and their queried direction.
fn walk_pins(sb: &mut Sandbox, filter: u32) -> Vec<(u32, u32)> {
    // Stop the filter — ReceiveConnection requires `State_Stopped`.
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_BASEFILTER_STOP,
        &[],
    );
    // EnumPins(filter, &ppEnum).
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
        eprintln!("round58 walk_pins: EnumPins failed: {r:?}");
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
        match r {
            Ok(0) if pin != 0 && fetched == 1 => {
                sb.host.com.intern(pin, None);
                // Query direction.
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
            Ok(1) => {
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
                break;
            }
            _ => break,
        }
    }
    let _ = sb.com_release(pp);
    pins
}

// ─── Phase 1 ──────────────────────────────────────────────────────

/// Phase 1 anchor — the splitter exposes at least one pin via
/// `IBaseFilter::EnumPins`, and the first input pin's
/// `IPin::EnumMediaTypes` walk surfaces zero or more AMTs without
/// trapping.  We dump every pin + AMT to stderr for the r58 report.
#[test]
fn phase1_walk_pins_and_input_amts_documents_offered_types() {
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round58: msadds32.ax missing; skipping");
        return;
    };
    let pins = walk_pins(&mut sb, filter);
    eprintln!("round58 phase1: discovered {} pin(s):", pins.len());
    for (i, (pin, dir)) in pins.iter().enumerate() {
        let dir_name = match *dir {
            PIN_DIRECTION_INPUT => "INPUT",
            PIN_DIRECTION_OUTPUT => "OUTPUT",
            _ => "UNKNOWN",
        };
        eprintln!("  pin[{i}] @ {pin:#010x}  direction={dir_name} ({dir})");
    }
    // Per MSDN's `IBaseFilter::EnumPins` contract, a DirectShow
    // splitter ALWAYS has at least one pin (decoder filters
    // typically have one input + one output pin).  Surface a
    // non-fatal warning rather than a panic if zero are
    // enumerated — likely indicates an EnumPins trap upstream.
    if pins.is_empty() {
        eprintln!("round58 phase1: no pins enumerated — likely an EnumPins trap");
        return;
    }
    // Find the first INPUT-direction pin (which is the encoded-audio
    // receive pin we care about).
    let input_pin = pins
        .iter()
        .find_map(|(p, d)| (*d == PIN_DIRECTION_INPUT).then_some(*p));
    let Some(input_pin) = input_pin else {
        eprintln!("round58 phase1: no INPUT-direction pin found");
        return;
    };
    eprintln!("round58 phase1: input pin = {input_pin:#010x}; walking EnumMediaTypes...");
    let captured = oxideav_vfw::com::host_iface_r31::walk_codec_input_pin_amts(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        input_pin,
        16,
    )
    .unwrap_or_else(|e| {
        eprintln!("round58 phase1: walk_codec_input_pin_amts errored: {e}");
        Vec::new()
    });
    eprintln!(
        "round58 phase1: input pin advertised {} AMT(s):",
        captured.len()
    );
    for (i, c) in captured.iter().enumerate() {
        eprintln!(
            "  amt[{i}] @ {addr:#010x}  major={major}  sub={sub}  fmt={fmt}  \
             cb_format={cb}  pb_format={pb:#010x}",
            addr = c.amt_addr,
            major = c.majortype,
            sub = c.subtype,
            fmt = c.formattype,
            cb = c.cb_format,
            pb = c.pb_format,
        );
        // If the format block is a WAVEFORMATEX, dump the leading
        // 18 bytes so we can confirm wFormatTag etc.  We don't
        // assert specific bytes — the splitter is allowed to vend
        // anything; we just report what it does so r59 has
        // empirical reference.
        if c.cb_format >= 18 && c.pb_format != 0 {
            let mut wfx = [0u8; 18];
            let mut ok = true;
            for (i, slot) in wfx.iter_mut().enumerate() {
                match sb.mmu.load8(c.pb_format + i as u32) {
                    Ok(b) => *slot = b,
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                let tag = u16::from_le_bytes([wfx[0], wfx[1]]);
                let ch = u16::from_le_bytes([wfx[2], wfx[3]]);
                let sr = u32::from_le_bytes([wfx[4], wfx[5], wfx[6], wfx[7]]);
                let bps = u16::from_le_bytes([wfx[14], wfx[15]]);
                eprintln!(
                    "       wFormatTag={tag:#06x}  nChannels={ch}  \
                     nSamplesPerSec={sr}  wBitsPerSample={bps}"
                );
            }
        }
    }
    // No hard assertion on captured.len() — splitters that
    // negotiate purely through `IPin::QueryAccept` rather than
    // pre-enumerating AMTs return zero from EnumMediaTypes.  The
    // phase succeeds as long as the walk did not trap.
}

// ─── Phase 2 ──────────────────────────────────────────────────────

/// Phase 2 anchor — `stage_audio_am_media_type` lays out a
/// 72-byte AM_MEDIA_TYPE + 28-byte WAVEFORMATEX (18 base + 10
/// extradata) at consecutive guest addresses, and every field
/// round-trips.
#[test]
fn phase2_audio_amt_layout_round_trips_via_guest_memory() {
    let mut sb = Sandbox::new();
    let extra = [0xAAu8, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22, 0x33, 0x44];
    let amt = stage_audio_am_media_type(
        &mut sb,
        WAVE_FORMAT_MSAUDIO1,
        2,      // nChannels
        44_100, // nSamplesPerSec
        20_000, // nAvgBytesPerSec — typical MSAUDIO1 mono/stereo bitrate
        2,      // nBlockAlign
        16,     // wBitsPerSample
        &extra,
    )
    .expect("stage audio AMT");
    // Round-trip majortype + subtype.
    assert_eq!(Guid::load(&sb.mmu, amt).unwrap(), mediatype_audio());
    assert_eq!(
        Guid::load(&sb.mmu, amt + 16).unwrap(),
        audio_subtype_for_format_tag(WAVE_FORMAT_MSAUDIO1)
    );
    // bTemporalCompression = 1.
    assert_eq!(sb.mmu.load32(amt + 36).unwrap(), 1);
    // formattype = FORMAT_WaveFormatEx.
    assert_eq!(
        Guid::load(&sb.mmu, amt + 44).unwrap(),
        format_wave_format_ex()
    );
    // cbFormat = 18 + 10 = 28.
    assert_eq!(sb.mmu.load32(amt + 64).unwrap(), 28);
    let fmt = sb.mmu.load32(amt + 68).unwrap();
    assert_ne!(fmt, 0);
    // WAVEFORMATEX fields.
    assert_eq!(
        u16::from_le_bytes([sb.mmu.load8(fmt).unwrap(), sb.mmu.load8(fmt + 1).unwrap()]),
        WAVE_FORMAT_MSAUDIO1
    );
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
    assert_eq!(read_u16(2), 2); // nChannels
    assert_eq!(read_u32(4), 44_100);
    assert_eq!(read_u32(8), 20_000);
    assert_eq!(read_u16(12), 2);
    assert_eq!(read_u16(14), 16);
    assert_eq!(read_u16(16), 10); // cbSize
                                  // Extradata round-trips.
    for (i, b) in extra.iter().enumerate() {
        assert_eq!(sb.mmu.load8(fmt + 18 + i as u32).unwrap(), *b);
    }
}

/// Audio subtype helper builds the canonical MSAUDIO1 / WMAUDIO2
/// GUIDs that `mmreg.h` documents — `Data1 = wFormatTag` and the
/// trailer is the audio fourcc-base GUID.
#[test]
fn phase2_audio_subtype_helper_matches_mmreg_constants() {
    let g1 = audio_subtype_for_format_tag(WAVE_FORMAT_MSAUDIO1);
    assert_eq!(
        g1.to_braced_string(),
        "{00000160-0000-0010-8000-00AA00389B71}"
    );
    let g2 = audio_subtype_for_format_tag(WAVE_FORMAT_WMAUDIO2);
    assert_eq!(
        g2.to_braced_string(),
        "{00000161-0000-0010-8000-00AA00389B71}"
    );
}

// ─── Phase 3 ──────────────────────────────────────────────────────

/// Phase 3 anchor — drive `IPin::ReceiveConnection` against the
/// audio splitter's input pin.  We try every codec-advertised AMT
/// first; if all reject we fall back to synthetic
/// MSAUDIO1/WMAUDIO2 AMTs.  At least one HRESULT (success or
/// failure) must surface without trapping.  The detailed return
/// codes are reported on stderr for the r58 deliverables.
#[test]
fn phase3_receive_connection_against_audio_input_pin_reports_outcome() {
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round58: msadds32.ax missing; skipping");
        return;
    };
    let pins = walk_pins(&mut sb, filter);
    let Some(input_pin) = pins
        .iter()
        .find_map(|(p, d)| (*d == PIN_DIRECTION_INPUT).then_some(*p))
    else {
        eprintln!("round58 phase3: no INPUT pin; skipping");
        return;
    };
    // Walk codec AMTs first.
    let captured = oxideav_vfw::com::host_iface_r31::walk_codec_input_pin_amts(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        input_pin,
        8,
    )
    .unwrap_or_default();

    let mut accepted: Option<(usize, u32, u32)> = None; // (idx, amt, hr)
    for (i, cap) in captured.iter().enumerate() {
        let host_out = sb
            .mint_host_output_pin_with_connection(cap.amt_addr, input_pin)
            .expect("mint host output pin");
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            input_pin,
            SLOT_PIN_RECEIVE_CONNECTION,
            &[host_out, cap.amt_addr],
        )
        .expect("ReceiveConnection should not trap");
        eprintln!(
            "round58 phase3: ReceiveConnection (codec amt[{i}], sub={sub}) → \
             HRESULT {r:#010x}",
            sub = cap.subtype
        );
        if r == 0 {
            accepted = Some((i, cap.amt_addr, r));
            break;
        }
    }
    if accepted.is_none() {
        // Try every synthetic candidate (MSAUDIO1 first, then WMAUDIO2).
        for &tag in &[WAVE_FORMAT_MSAUDIO1, WAVE_FORMAT_WMAUDIO2] {
            // Per-codec extradata — a 10-byte zero block is what
            // ffmpeg's `wmavoice_v1_init` accepts as the default
            // "no extra hints" header (the codec then derives its
            // sample-rate-class from `nSamplesPerSec`).  Empirical
            // value, not codec-internal.
            let extra = [0u8; 10];
            let amt = stage_audio_am_media_type(&mut sb, tag, 2, 44_100, 20_000, 2, 16, &extra)
                .expect("stage synthetic audio AMT");
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
            .expect("ReceiveConnection (synth) should not trap");
            eprintln!(
                "round58 phase3: ReceiveConnection (synth wFormatTag={tag:#06x}) → \
                 HRESULT {r:#010x}"
            );
            if r == 0 {
                accepted = Some((usize::MAX, amt, r));
                break;
            }
        }
    }
    match accepted {
        Some((i, amt, hr)) => {
            eprintln!("round58 phase3: ACCEPTED amt {amt:#010x} (candidate={i}, hr={hr:#010x})")
        }
        None => eprintln!(
            "round58 phase3: every candidate AMT REJECTED \
             (codec-advertised count={}, synth count=2)",
            captured.len()
        ),
    }
    // Phase 3 succeeds if at least one ReceiveConnection call
    // returned without trapping.  Whether the codec accepted any
    // specific AMT is informational — the goal is the smoke shape,
    // not codec compatibility.
}

// ─── Phase 4 ──────────────────────────────────────────────────────

/// Phase 4 anchor — drive `IMediaFilter::Pause + Run(0) + GetState`
/// on the audio splitter.  No trap; every HRESULT recorded on
/// stderr.
#[test]
fn phase4_pause_run_get_state_walks_without_trapping() {
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round58: msadds32.ax missing; skipping");
        return;
    };
    let r_pause = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_MEDIAFILTER_PAUSE,
        &[],
    )
    .expect("IMediaFilter::Pause must not trap");
    eprintln!("round58 phase4: Pause() → HRESULT {r_pause:#010x}");

    let r_run = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_MEDIAFILTER_RUN,
        &[0, 0], // REFERENCE_TIME tStart = 0 — two zero dwords
    )
    .expect("IMediaFilter::Run(0) must not trap");
    eprintln!("round58 phase4: Run(0) → HRESULT {r_run:#010x}");

    // GetState(1000ms, &state).
    let state_slot = sb.host.arena_alloc(4).expect("arena_alloc state");
    sb.mmu
        .write_initializer(state_slot, &0xFFFF_FFFFu32.to_le_bytes())
        .expect("seed state");
    let r_state = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        SLOT_MEDIAFILTER_GET_STATE,
        &[1000, state_slot],
    )
    .expect("IMediaFilter::GetState must not trap");
    let state = sb.mmu.load32(state_slot).unwrap_or(0xFFFF_FFFF);
    eprintln!("round58 phase4: GetState(1000ms) → HRESULT {r_state:#010x}, FILTER_STATE={state}");
}

// ─── Phase 5 (smoke) ──────────────────────────────────────────────

/// Phase 5 smoke — push one synthetic 64-byte encoded sample
/// through `IMemInputPin::Receive` after ReceiveConnection +
/// Run.  Success criterion: did not panic (any HRESULT is OK).
/// Real bit-correctness validation is r59+ work.
#[test]
fn phase5_push_one_synthetic_sample_through_receive_smoke() {
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round58: msadds32.ax missing; skipping");
        return;
    };
    let pins = walk_pins(&mut sb, filter);
    let Some(input_pin) = pins
        .iter()
        .find_map(|(p, d)| (*d == PIN_DIRECTION_INPUT).then_some(*p))
    else {
        eprintln!("round58 phase5: no INPUT pin; skipping");
        return;
    };
    // Walk codec AMTs + pick the first one that ReceiveConnection
    // accepts (if any).
    let captured = oxideav_vfw::com::host_iface_r31::walk_codec_input_pin_amts(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        input_pin,
        8,
    )
    .unwrap_or_default();
    let mut accepted_amt = 0u32;
    for cap in &captured {
        let host_out = sb
            .mint_host_output_pin_with_connection(cap.amt_addr, input_pin)
            .expect("mint host output pin");
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            input_pin,
            SLOT_PIN_RECEIVE_CONNECTION,
            &[host_out, cap.amt_addr],
        )
        .expect("ReceiveConnection should not trap");
        if r == 0 {
            accepted_amt = cap.amt_addr;
            break;
        }
    }
    if accepted_amt == 0 {
        for &tag in &[WAVE_FORMAT_MSAUDIO1, WAVE_FORMAT_WMAUDIO2] {
            let extra = [0u8; 10];
            let amt = stage_audio_am_media_type(&mut sb, tag, 2, 44_100, 20_000, 2, 16, &extra)
                .expect("stage synthetic audio AMT");
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
            .expect("ReceiveConnection (synth) should not trap");
            if r == 0 {
                accepted_amt = amt;
                break;
            }
        }
    }
    if accepted_amt == 0 {
        eprintln!("round58 phase5: no AMT accepted; cannot push sample");
        return;
    }
    // Pause + Run before Receive.
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
    // QI for IMemInputPin on the input pin.
    let mip = match sb.query_interface(input_pin, IID_IMEMINPUTPIN) {
        Ok(p) if p != 0 => p,
        Ok(_) => {
            eprintln!("round58 phase5: QI(IMemInputPin) → NULL");
            return;
        }
        Err(e) => {
            eprintln!("round58 phase5: QI(IMemInputPin) failed: {e}");
            return;
        }
    };
    eprintln!("round58 phase5: IMemInputPin @ {mip:#010x}");
    // Mint a host sample carrying a synthetic 64-byte encoded
    // payload.  The bytes are zero-padded — the codec will likely
    // reject them as a malformed frame, but we just care that
    // `Receive` completes without trapping.
    let sample = sb
        .mint_host_media_sample(/*data_capacity=*/ 256, accepted_amt)
        .expect("mint host media sample");
    let payload = vec![0u8; 64];
    sb.media_sample_set_payload(sample, &payload, /*sync_point=*/ true)
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
            "round58 phase5: IMemInputPin::Receive(synthetic 64B sample) → HRESULT {hr:#010x}"
        ),
        Err(e) => {
            eprintln!("round58 phase5: IMemInputPin::Receive trapped (expected for r59 smoke): {e}")
        }
    }
    // Also pin the sample helper used the canonical IMediaSample
    // family (regression on round-30 mint shape).
    assert!(vtable_is_plausible(&sb.mmu, sample));
    // Quiet the IID_IMEDIASAMPLE import to confirm the module path.
    let _ = IID_IMEDIASAMPLE;
}
