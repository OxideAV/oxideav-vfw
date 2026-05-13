//! Round 64 — `msadds32.ax` `IMemInputPin::Receive` now returns
//! `E_UNEXPECTED` (`0x8000ffff`) after round-63 cleared the
//! `helper_addref` NULL-deref trap via
//! [`Sandbox::msadds32_patch_helper_addref`].  Round 64 walks the
//! trace ring forensically to identify which of the codec's many
//! `mov eax, 0x8000FFFF; ret` sites actually fires and what
//! conditional check led there.
//!
//! ## Candidate sites (from raw `msadds32.ax` byte scan)
//!
//! `mov eax, 0x8000FFFF` encodes as `b8 ff ff 00 80`.  A linear
//! scan of the `.text` section (RVA `0x1000..0xee9d`) finds 10
//! occurrences (file offsets equal RVAs because `.text` raw_data
//! == virt_addr == `0x1000`):
//!
//!   `0x4c7a, 0x5370, 0x59ee, 0x66a6, 0x66ce, 0x6750, 0x6787,
//!    0x67ce, 0x6832, 0x685a`
//!
//! Each is the failure tail of a separate vtable method.  Phase 1
//! turns on a large trace ring + visited-EIP tracking, drives the
//! patched `Receive`, then scans for the LAST candidate-RVA
//! encountered — that's the one whose `ret` produced `eax =
//! 0x8000ffff`.
//!
//! ## Reference material (clean-room only)
//!
//! * Intel SDM Vol. 2 — opcode encoding, ModR/M, SIB, control
//!   transfer.
//! * MSDN — `IMemInputPin::Receive`, `IPin::EndOfStream`,
//!   `IMediaSample`, COM HRESULT semantics
//!   (`E_UNEXPECTED = 0x8000FFFF`).
//! * Raw bytes of `msadds32.ax` from
//!   `docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/`.
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
use oxideav_vfw::emulator::regs::Reg32;
use oxideav_vfw::{Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY, IID_IMEMINPUTPIN, IID_IUNKNOWN};
use std::path::PathBuf;

// ─── E_UNEXPECTED-emitting RVAs in msadds32.ax (clean-room scan) ─────

/// File offsets where the byte sequence `b8 ff ff 00 80` appears
/// inside `msadds32.ax`'s `.text` section.  Because the binary's
/// `.text` raw_data == virt_addr == `0x1000`, the file offset
/// equals the RVA.  Each is the start of `mov eax, 0x8000FFFF;
/// ret` — the canonical x86 emission for "return `E_UNEXPECTED`".
///
/// Scan command:
///
/// ```sh
/// python3 -c "import sys; d=open(sys.argv[1],'rb').read();
///     o=0;
///     while True:
///         i = d.find(b'\\xb8\\xff\\xff\\x00\\x80', o)
///         if i<0: break
///         print(hex(i)); o=i+1" docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/msadds32.ax
/// ```
const E_UNEXPECTED_SITES: &[u32] = &[
    0x4c7a, 0x5370, 0x59ee, 0x66a6, 0x66ce, 0x6750, 0x6787, 0x67ce, 0x6832, 0x685a,
];

// ─── Drive helpers (mirror r63's drive_receive_with_patch) ───────────

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

fn load_msadds32_with_big_trace() -> Option<(Sandbox, oxideav_vfw::pe::Image)> {
    let p = msadds32_path()?;
    let bytes = std::fs::read(&p).ok()?;
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(8_000_000_000);
    // 1 048 576-entry trace ring is enough for the full Receive body;
    // empirically it fits ~100K x86 instructions per audio frame.
    sb.cpu.enable_trace_ring(1_048_576);
    sb.cpu.track_visited_eips = true;
    let img = sb.load("msadds32.ax", &bytes).ok()?;
    let _ = sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH);
    Some((sb, img))
}

fn bootstrap_filter() -> Option<(Sandbox, oxideav_vfw::pe::Image, u32)> {
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
    Some((sb, img, filter))
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

fn fmt_bytes(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn dump_bytes(sb: &Sandbox, va: u32, len: u32) -> Vec<u8> {
    (0..len)
        .map(|i| sb.mmu.load8(va.wrapping_add(i)).unwrap_or(0))
        .collect()
}

/// Drive the full chain up to (and including) `IMemInputPin::Receive`
/// with the round-63 `helper_addref` patch applied (so we don't
/// re-hit the NULL+0x20 trap).  Returns the final `eax` and the
/// trace ring so the caller can scan for the last `E_UNEXPECTED`-
/// emitting RVA.
struct ReceiveRun {
    sb: Sandbox,
    base: u32,
    hr: Option<u32>,
    trap_msg: Option<String>,
    eax: u32,
    edx: u32,
    /// Set to the `(sample, payload_len)` of the input frame the
    /// codec was given so phase-5 callers can re-inspect.
    sample: u32,
}

#[allow(clippy::too_many_arguments)]
fn drive_receive_full(
    patch_value: Option<u32>,
    set_sync_point: bool,
    set_media_time: bool,
    set_discontinuity: bool,
) -> Option<ReceiveRun> {
    let (mut sb, img, filter) = bootstrap_filter()?;
    let base = img.image_base;
    if let Some(v) = patch_value {
        sb.msadds32_patch_helper_addref(base, v).ok()?;
    }

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
        return None;
    }
    let mip = sb.query_interface(input_pin, IID_IMEMINPUTPIN).ok()?;
    if mip == 0 {
        return None;
    }

    // Output-pin connection (PCM downstream).
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

    // Allocator handshake.
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

    // Pause + Run.
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

    // WMA fixture sample.
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    let asf_bytes = std::fs::read(&fixture_path).ok()?;
    let packet = oxideav_vfw::com::locate_first_data_packet(&asf_bytes).unwrap_or(&[]);
    if packet.is_empty() {
        return None;
    }
    let payload: Vec<u8> = packet.iter().take(4096).copied().collect();
    let sample = sb.mint_host_media_sample(8192, amt).ok()?;
    sb.media_sample_set_payload(sample, &payload, set_sync_point)
        .ok()?;
    if set_media_time {
        // IMediaSample header layout (mint_host_media_sample):
        //   obj+40 = media-start (LONGLONG low)
        //   obj+44 = media-start (LONGLONG high)
        //   obj+48 = media-stop  (LONGLONG low)
        //   obj+52 = media-stop  (LONGLONG high)
        // Plant start=0, stop=payload.len() (1 stream-unit per byte
        // for this probe — codec doesn't validate the value, just
        // the presence-of-set bit which lives at obj+36).
        let _ = sb.mmu.write_initializer(sample + 36, &1u32.to_le_bytes());
        let _ = sb.mmu.write_initializer(sample + 40, &0u32.to_le_bytes());
        let _ = sb.mmu.write_initializer(sample + 44, &0u32.to_le_bytes());
        let _ = sb
            .mmu
            .write_initializer(sample + 48, &(payload.len() as u32).to_le_bytes());
        let _ = sb.mmu.write_initializer(sample + 52, &0u32.to_le_bytes());
    }
    if set_discontinuity {
        // obj+28 holds the discontinuity flag in our host sample
        // layout (see mint_host_media_sample).
        let _ = sb.mmu.write_initializer(sample + 28, &1u32.to_le_bytes());
    }

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

    let (hr, trap_msg) = match r {
        Ok(hr) => (Some(hr), None),
        Err(e) => (None, Some(format!("{e}"))),
    };
    let run = ReceiveRun {
        eax: sb.cpu.regs.get32(Reg32::Eax),
        edx: sb.cpu.regs.get32(Reg32::Edx),
        sb,
        base,
        hr,
        trap_msg,
        sample,
    };
    Some(run)
}

/// Scan a trace ring for the last execution of any RVA in
/// [`E_UNEXPECTED_SITES`].  Returns `Some(rva)` if found.
fn last_e_unexpected_site(trace_ring: &[u32], base: u32) -> Option<u32> {
    let candidate_va: std::collections::HashSet<u32> =
        E_UNEXPECTED_SITES.iter().map(|&r| base + r).collect();
    trace_ring
        .iter()
        .rev()
        .find(|&&eip| candidate_va.contains(&eip))
        .map(|&eip| eip.wrapping_sub(base))
}

// ─── Phase 1 — disassemble each E_UNEXPECTED-emitting site ───────────

/// Phase 1 — dump 16 bytes before + 16 bytes after each candidate
/// RVA, so a future round can re-decode without re-running the
/// binary scan.  Each candidate is the body of a different vtable
/// method's failure tail.
#[test]
fn phase1_dump_e_unexpected_sites() {
    let Some((sb, img)) = load_msadds32_with_big_trace() else {
        eprintln!("round64 phase1: msadds32.ax missing; skipping");
        return;
    };
    let base = img.image_base;
    eprintln!("round64 phase1: image_base={base:#010x}");
    eprintln!("round64 phase1: candidate E_UNEXPECTED return sites:");
    for &rva in E_UNEXPECTED_SITES {
        let va = base + rva;
        let before = dump_bytes(&sb, va.wrapping_sub(16), 16);
        let here = dump_bytes(&sb, va, 16);
        eprintln!("  rva {rva:#06x}  va {va:#010x}");
        eprintln!("    before: {}", fmt_bytes(&before));
        eprintln!("    here  : {}", fmt_bytes(&here));
    }
}

// ─── Phase 2 — drive Receive + find the live E_UNEXPECTED site ───────

/// Phase 2 — drive the patched `Receive` (which empirically returns
/// `eax = 0x8000ffff`) and scan the trace ring for the last
/// candidate-RVA executed.  That's the site whose `ret` produced
/// the surfaced HRESULT.
#[test]
fn phase2_find_live_e_unexpected_site() {
    if msadds32_path().is_none() {
        eprintln!("round64 phase2: msadds32.ax missing; skipping");
        return;
    }
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    if !fixture.is_file() {
        eprintln!("round64 phase2: WMA2 fixture missing; skipping");
        return;
    }
    // Baseline run with helper_addref patched to 65536 (the value
    // that round-63 confirmed clears the LIFO trap).  Sync-point
    // set; media time and discontinuity not yet set so we observe
    // the bare-minimum input the codec rejects.
    let Some(run) = drive_receive_full(Some(65_536), true, false, false) else {
        eprintln!("round64 phase2: drive_receive_full bootstrap failed");
        return;
    };
    eprintln!(
        "round64 phase2: hr={:?}  trap={:?}  eax={:#x}  edx={:#x}  sample={:#x}",
        run.hr, run.trap_msg, run.eax, run.edx, run.sample
    );
    eprintln!(
        "round64 phase2: trace ring length = {}, unique EIPs = {}",
        run.sb.cpu.trace_ring.len(),
        run.sb.cpu.visited_eips.len()
    );
    let last_site = last_e_unexpected_site(&run.sb.cpu.trace_ring, run.base);
    eprintln!("round64 phase2: last E_UNEXPECTED-emitting RVA = {last_site:?}");
    // Also report ALL sites visited (helps diagnose if a `jmp` to
    // a shared E_UNEXPECTED tail rather than each method having its
    // own).
    let candidate_va: std::collections::HashSet<u32> =
        E_UNEXPECTED_SITES.iter().map(|&r| run.base + r).collect();
    let visited_sites: Vec<u32> = run
        .sb
        .cpu
        .visited_eips
        .iter()
        .filter(|eip| candidate_va.contains(*eip))
        .map(|eip| eip.wrapping_sub(run.base))
        .collect();
    eprintln!("round64 phase2: all E_UNEXPECTED RVAs visited = {visited_sites:?}");
    // EMPIRICAL FINDING: the codec doesn't use any of the 10
    // `mov eax, 0x8000FFFF` (`b8 ff ff 00 80`) sites in `.text`.
    // Instead, the E_UNEXPECTED literal is written to the caller's
    // HRESULT out-slot via `c7 45 08 ff ff 00 80` (`mov [ebp+0x08],
    // 0x8000FFFF`) at RVA 0x172f — phase 3 then loads
    // `eax = [ebp+0x08]` at RVA 0x176c.  Phase 5a pins that path
    // via the bail-out's structural-sentinel set.  This test is
    // therefore expected to report `last_e_unexp_rva = None`; we
    // assert only the negative ("no candidate hit") so the failure
    // mode regression-guards on phase 5a, not here.
    assert!(
        last_site.is_none(),
        "round64 phase2: regressed — a candidate `mov eax, 0x8000FFFF` site \
            IS now reached (RVA {last_site:?}).  Update phase 5a to track \
            the new site too."
    );
}

// ─── Phase 3 — disassemble the function containing the live site ──────

/// Phase 3 — once phase 2 has pinned the live RVA, dump 0x80 bytes
/// of context around it so the next round (or reader of this test's
/// stderr) can decode the failing check without re-running.  Also
/// walk backwards within the trace ring to capture the conditional
/// branch that led to the site.
#[test]
fn phase3_disassemble_live_site_context() {
    if msadds32_path().is_none() {
        eprintln!("round64 phase3: msadds32.ax missing; skipping");
        return;
    }
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    if !fixture.is_file() {
        eprintln!("round64 phase3: WMA2 fixture missing; skipping");
        return;
    }
    let Some(run) = drive_receive_full(Some(65_536), true, false, false) else {
        eprintln!("round64 phase3: drive_receive_full bootstrap failed");
        return;
    };
    let base = run.base;
    let last_site_rva = last_e_unexpected_site(&run.sb.cpu.trace_ring, base);
    eprintln!("round64 phase3: live site RVA = {last_site_rva:?}");
    // ALWAYS dump the tail of the trace ring — that's where the
    // E_UNEXPECTED return came from, even if it's not one of the
    // 10 statically-known sites (the codec may compose the value
    // dynamically, e.g. via `or eax, imm` after `or` / `shl`).
    let ring = &run.sb.cpu.trace_ring;
    let tail_start = ring.len().saturating_sub(64);
    eprintln!("round64 phase3: last 64 entry-EIPs in trace ring:");
    for (i, &eip) in ring[tail_start..].iter().enumerate() {
        let rva = eip.wrapping_sub(base);
        let b0 = run.sb.mmu.load8(eip).unwrap_or(0);
        let b1 = run.sb.mmu.load8(eip + 1).unwrap_or(0);
        let b2 = run.sb.mmu.load8(eip + 2).unwrap_or(0);
        let b3 = run.sb.mmu.load8(eip + 3).unwrap_or(0);
        let b4 = run.sb.mmu.load8(eip + 4).unwrap_or(0);
        eprintln!(
            "  [{:>3}] rva {rva:#06x}  va {eip:#010x}  bytes {b0:02x} {b1:02x} {b2:02x} {b3:02x} {b4:02x}",
            tail_start + i
        );
    }
    let Some(rva) = last_site_rva else {
        eprintln!("round64 phase3: no E_UNEXPECTED RVA in trace ring; skipping disasm");
        return;
    };

    // Dump 0x40 bytes before + 0x40 bytes after the site.  Most
    // failure tails are `b8 ff ff 00 80 (ret encoding)` and the
    // conditional that fed them sits within the preceding 64 bytes.
    let from = (base + rva).wrapping_sub(0x40);
    let body = dump_bytes(&run.sb, from, 0x80);
    eprintln!("round64 phase3: ±0x40 around live site:");
    for (i, row) in body.chunks(16).enumerate() {
        let va = from + (i as u32) * 16;
        let rva = va.wrapping_sub(base);
        eprintln!("  rva {rva:#06x}  va {va:#010x}: {}", fmt_bytes(row));
    }

    // Walk back through the trace ring to find the conditional
    // branch that fed the site.  We want the last branch
    // (`70..7F`, `0F 80..0F 8F`) whose entry-EIP is in the same
    // function as the site and which preceded the site.
    let site_va = base + rva;
    let mut idx_of_site = None;
    for (i, &eip) in run.sb.cpu.trace_ring.iter().enumerate().rev() {
        if eip == site_va {
            idx_of_site = Some(i);
            break;
        }
    }
    let Some(idx) = idx_of_site else {
        eprintln!("round64 phase3: site not found in ring? (skipping back-walk)");
        return;
    };

    // Last 32 instructions before the site.
    let lo = idx.saturating_sub(48);
    eprintln!("round64 phase3: last ~48 instructions before site:");
    for &eip in &run.sb.cpu.trace_ring[lo..=idx] {
        let rva = eip.wrapping_sub(base);
        let first = run.sb.mmu.load8(eip).unwrap_or(0);
        let second = run.sb.mmu.load8(eip + 1).unwrap_or(0);
        let third = run.sb.mmu.load8(eip + 2).unwrap_or(0);
        eprintln!("  rva {rva:#06x}  {first:02x} {second:02x} {third:02x}");
    }
}

// ─── Phase 4 — verify the workaround patch is still applied ───────────

/// Phase 4 — sanity-check that without the helper-addref patch we
/// still trap at the round-62 baseline RVA 0x256a, while with it
/// we still hit `eax = 0x8000ffff`.  This regression-guards the
/// round-63 workaround.
#[test]
fn phase4_workaround_regression_guard() {
    if msadds32_path().is_none() {
        eprintln!("round64 phase4: msadds32.ax missing; skipping");
        return;
    }
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    if !fixture.is_file() {
        eprintln!("round64 phase4: WMA2 fixture missing; skipping");
        return;
    }

    let unpatched = drive_receive_full(None, true, false, false);
    if let Some(o) = &unpatched {
        eprintln!(
            "round64 phase4: unpatched — hr={:?}  trap={:?}",
            o.hr, o.trap_msg
        );
        // Round-63 baseline: trap, not S_OK.
        if let Some(msg) = o.trap_msg.as_deref() {
            assert!(
                msg.contains("memory fault at 0x00000020"),
                "round64 regression: unpatched run no longer hits the round-63 \
                    NULL+0x20 trap (msg = {msg})"
            );
        } else {
            panic!(
                "round64 regression: unpatched run returned HRESULT {:?} \
                    instead of trapping at 0x00000020",
                o.hr
            );
        }
    }

    let patched = drive_receive_full(Some(65_536), true, false, false);
    if let Some(o) = &patched {
        eprintln!(
            "round64 phase4: patched — hr={:?}  trap={:?}  eax={:#x}",
            o.hr, o.trap_msg, o.eax
        );
        // With the patch, Receive runs to completion + returns
        // E_UNEXPECTED.  This is the round-64 investigation surface.
        if let Some(hr) = o.hr {
            assert_eq!(
                hr, 0x8000_ffff,
                "round64 regression: patched run no longer reaches \
                    E_UNEXPECTED (hr was {hr:#010x})"
            );
        }
    }
}

// ─── Phase 5a — directly probe the loop-counter slot [ebp-0x24] ─────────

/// Phase 5a — pin the bail-out site via a sentinel-byte scan that
/// doesn't depend on the trace ring's exact contents:
///
/// 1. Drive Receive (patched).
/// 2. The trace ring's tail MUST contain rva `0x172f` (the
///    `c7 45 08 ff ff 00 80` = `mov dword [ebp+0x08], 0x8000FFFF`
///    site we identified empirically).
/// 3. Walk back through the ring to find the JNZ at `0x165b` that
///    fed `0x172f`.  Confirm it was reached after [ebp-0x24] != 0
///    (i.e., the loop ran ≥2× without output).
///
/// This nails the failing check in a regression-safe way: the
/// failure isn't "any of N candidate E_UNEXPECTED-emitting sites"
/// but specifically the inner-decode-produced-no-output guard.
#[test]
fn phase5a_pin_inner_decode_no_output_bailout() {
    if msadds32_path().is_none() {
        eprintln!("round64 phase5a: msadds32.ax missing; skipping");
        return;
    }
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    if !fixture.is_file() {
        eprintln!("round64 phase5a: WMA2 fixture missing; skipping");
        return;
    }
    let Some(run) = drive_receive_full(Some(65_536), true, false, false) else {
        eprintln!("round64 phase5a: bootstrap failed");
        return;
    };
    let base = run.base;
    let ring = &run.sb.cpu.trace_ring;
    // Required structural sentinels.
    let bailout_site = base + 0x172f; // mov [ebp+0x08], 0x8000FFFF
    let guard_jnz = base + 0x165b; // jnz +0xce → 0x172f
    let guard_cmp = base + 0x1658; // cmp [ebp-0x24], ebx
    let loop_back = base + 0x172a; // jmp -0x1b5 → 0x157a
                                   // Verify the trace ring contains them.
    let saw_bailout = ring.contains(&bailout_site);
    let saw_guard_jnz = ring.contains(&guard_jnz);
    let saw_guard_cmp = ring.contains(&guard_cmp);
    let saw_loop_back = ring.contains(&loop_back);
    eprintln!(
        "round64 phase5a: hr={:?}  saw_bailout={}  saw_guard_jnz={}  saw_guard_cmp={}  saw_loop_back={}",
        run.hr, saw_bailout, saw_guard_jnz, saw_guard_cmp, saw_loop_back
    );
    if run.hr == Some(0x8000_ffff) {
        assert!(
            saw_bailout && saw_guard_jnz && saw_guard_cmp,
            "round64 phase5a: Receive returned E_UNEXPECTED but the trace ring \
                doesn't contain the inner-decode bail-out path \
                (cmp [ebp-0x24], ebx → jnz → mov [ebp+8], 0x8000FFFF) \
                — failure site may have moved"
        );
        // The back-edge proves the loop ran at least twice.
        assert!(
            saw_loop_back,
            "round64 phase5a: Receive returned E_UNEXPECTED but the trace ring \
                doesn't show the inner-decode loop back-edge at 0x172a — \
                interpretation 'inner-decode produced no output twice' may be wrong"
        );
    }
}

// ─── Phase 5 — try the candidate IMediaSample setter fixes ──────────────

/// Phase 5 — sweep a panel of plausible IMediaSample setter
/// combinations; for each, report whether `Receive` still emits
/// `E_UNEXPECTED` from the same RVA or whether it changed.  This is
/// the iterative "narrow the failing check" loop — the output of
/// phase 3 names the failing condition, and we pick setters that
/// satisfy it.
#[test]
fn phase5_imediasample_setter_panel() {
    if msadds32_path().is_none() {
        eprintln!("round64 phase5: msadds32.ax missing; skipping");
        return;
    }
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    if !fixture.is_file() {
        eprintln!("round64 phase5: WMA2 fixture missing; skipping");
        return;
    }

    let panels = [
        ("sync_only", true, false, false),
        ("sync+time", true, true, false),
        ("sync+disc", true, false, true),
        ("sync+time+disc", true, true, true),
        ("nosync", false, false, false),
        ("nosync+time", false, true, false),
    ];
    for (label, sync, time, disc) in panels {
        let Some(run) = drive_receive_full(Some(65_536), sync, time, disc) else {
            eprintln!("round64 phase5 [{label}]: bootstrap failed");
            continue;
        };
        let last_rva = last_e_unexpected_site(&run.sb.cpu.trace_ring, run.base);
        eprintln!(
            "round64 phase5 [{label}]: hr={:?}  eax={:#x}  last_e_unexp_rva={:?}",
            run.hr, run.eax, last_rva
        );
    }
}
