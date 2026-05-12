//! Round 62 — forensics + fix for the `IMemInputPin::Receive`
//! NULL+0x20 trap inside `msadds32.ax`.
//!
//! Round 61 closed by demonstrating that after the full input-pin
//! `IMemAllocator` handshake (`GetAllocator → SetProperties →
//! Commit → NotifyAllocator`) AND the output-pin
//! `IPin::ReceiveConnection` (PCM 44.1 kHz mono 16-bit), pushing a
//! 4 KiB WMA2 payload through `IMemInputPin::Receive` no longer
//! returns `VFW_E_NOT_COMMITTED`.  Instead the codec traps with
//!
//!     memory fault at 0x00000020 (page unmapped)
//!
//! — a NULL pointer deref reading the dword at offset `+0x20`.
//!
//! This round runs the same path under the emulator's
//! `trace_ring` so we can recover the faulting EIP, snapshot the
//! GP register file, and disassemble the instruction byte sequence
//! at the trap site.  Once the source register that held NULL is
//! identified we walk backwards to find what host wiring would
//! have populated it.
//!
//! The diagnostic is sticky-by-design: even if a later round wires
//! the missing field and the trap moves elsewhere, the assertions
//! here log the new trap site (or `S_OK`) for the next round to
//! pick up.  Test failures are reserved for outright regressions
//! (e.g. trap address goes BACK to `VFW_E_NOT_COMMITTED`).
//!
//! ## Reference material (clean-room only)
//!
//! * MSDN — `IMemInputPin`, `IMemAllocator`, `IMediaSample`,
//!   `IPin`, `AM_MEDIA_TYPE`, COM IUnknown ABI.
//! * Intel SDM Vol. 2 — opcode encoding tables, ModR/M + SIB
//!   semantics.
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
    SLOT_PIN_NEW_SEGMENT, SLOT_PIN_QUERY_DIRECTION, SLOT_PIN_RECEIVE_CONNECTION,
};
use oxideav_vfw::emulator::regs::Reg32;
use oxideav_vfw::{Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY, IID_IMEMINPUTPIN, IID_IUNKNOWN};
use std::path::PathBuf;

// ---- fixture helpers (mirror round 61) -------------------------------

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
    // Enable the trace ring so the faulting EIP is recoverable.
    // 2 KiB is enough for the post-Receive-entry tail; deeper
    // visibility uses the visited-EIPs set instead (cheaper than a
    // multi-million-entry ring buffer).
    sb.cpu.enable_trace_ring(2048);
    sb.cpu.track_visited_eips = true;
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

/// Drive the full production path up to (and through)
/// `IMemInputPin::Receive`, including the round-61 phase-5 output
/// connection.  Returns the trap diagnostic on failure or the
/// `HRESULT` on success.
struct ReceiveOutcome {
    hr: Option<u32>,
    err: Option<String>,
    /// Last 16 EIPs from the trace ring (entry-EIP of executed
    /// instructions).  The LAST entry is the faulting instruction.
    trace_tail: Vec<u32>,
    /// Snapshot of GP registers at the point of trap (or after
    /// successful Receive).
    regs: [(&'static str, u32); 8],
    /// Codec's image_base — needed to subtract for RVA reporting.
    image_base: u32,
    /// Set of every unique EIP the codec stepped at during the
    /// Receive body.  Lets us check whether a candidate function
    /// (e.g. the buffer-pool POP at 0x235c) was ever entered.
    visited: std::collections::BTreeSet<u32>,
}

fn drive_to_receive_trap() -> Option<(Sandbox, ReceiveOutcome)> {
    let (mut sb, img, filter) = bootstrap_filter()?;
    let image_base = img.image_base;

    // -- Establish input-pin connection w/ criteria-passing WMA2 AMT.
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
        eprintln!("drive: input-pin RC rejected {r_rc:#010x}");
        return None;
    }
    let mip = sb.query_interface(input_pin, IID_IMEMINPUTPIN).ok()?;
    if mip == 0 {
        return None;
    }

    // -- Connect codec's output pin to host downstream IMemInputPin.
    let out_pin = enum_pin_by_direction(&mut sb, filter, PIN_DIRECTION_OUTPUT)?;
    let (h_pin, _h_mip) = sb.host_iface_r31_mint_input_pin_pair().ok()?;
    let _ = sb.host_iface_r31_mint_base_filter(h_pin).ok()?;
    // Stage PCM downstream AMT (mono 44.1k 16-bit).
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

    // -- Input-pin allocator handshake.
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

    // -- Pause + Run.
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

    // -- Round-62 FIX attempt A: drive `IPin::NewSegment` on the
    // codec's input pin before `Receive`.  Empirically traps
    // immediately (rate-high dword 0x3FF00000 is dereferenced) —
    // gated off by default.
    if std::env::var_os("R62_DRIVE_NEW_SEGMENT").is_some() {
        let rate_lo = 0u32;
        let rate_hi = 0x3FF0_0000u32;
        let start_lo = 0u32;
        let start_hi = 0u32;
        let stop_lo = 10_000_000u32;
        let stop_hi = 0u32;
        let r_ns = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            input_pin,
            SLOT_PIN_NEW_SEGMENT,
            &[start_lo, start_hi, stop_lo, stop_hi, rate_lo, rate_hi],
        );
        match r_ns {
            Ok(hr) => eprintln!("round62 NewSegment → HRESULT {hr:#010x}"),
            Err(e) => eprintln!("round62 NewSegment trapped: {e}"),
        }
    }

    // -- Build sample from the WMA2 fixture.
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    let asf_bytes = std::fs::read(&fixture_path).ok()?;
    let packet = oxideav_vfw::com::locate_first_data_packet(&asf_bytes).unwrap_or(&[]);
    if packet.is_empty() {
        return None;
    }
    let payload: Vec<u8> = packet.iter().take(4096).copied().collect();
    let sample = sb.mint_host_media_sample(8192, amt).ok()?;
    sb.media_sample_set_payload(sample, &payload, true).ok()?;

    // -- Clear the trace ring just before the Receive so the
    // recovered tail is purely the Receive body.
    sb.cpu.trace_ring.clear();
    sb.cpu.visited_eips.clear();

    // -- Drive Receive — capture the outcome.
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        SLOT_MEMINPUTPIN_RECEIVE,
        &[sample],
    );

    let regs = [
        ("eax", sb.cpu.regs.get32(Reg32::Eax)),
        ("ecx", sb.cpu.regs.get32(Reg32::Ecx)),
        ("edx", sb.cpu.regs.get32(Reg32::Edx)),
        ("ebx", sb.cpu.regs.get32(Reg32::Ebx)),
        ("esp", sb.cpu.regs.esp()),
        ("ebp", sb.cpu.regs.get32(Reg32::Ebp)),
        ("esi", sb.cpu.regs.get32(Reg32::Esi)),
        ("edi", sb.cpu.regs.get32(Reg32::Edi)),
    ];
    let tail_len = sb.cpu.trace_ring.len().min(2048);
    let trace_tail: Vec<u32> = sb
        .cpu
        .trace_ring
        .iter()
        .rev()
        .take(tail_len)
        .rev()
        .copied()
        .collect();

    let visited: std::collections::BTreeSet<u32> = sb.cpu.visited_eips.clone();
    let outcome = match r {
        Ok(hr) => ReceiveOutcome {
            hr: Some(hr),
            err: None,
            trace_tail,
            regs,
            image_base,
            visited,
        },
        Err(e) => ReceiveOutcome {
            hr: None,
            err: Some(format!("{e}")),
            trace_tail,
            regs,
            image_base,
            visited,
        },
    };
    Some((sb, outcome))
}

fn dump_bytes(sb: &Sandbox, va: u32, len: u32) -> Vec<u8> {
    (0..len)
        .map(|i| sb.mmu.load8(va.wrapping_add(i)).unwrap_or(0))
        .collect()
}

fn fmt_bytes(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// ───────────────────────────────────────────────────────────────────
// Phase 1 — capture the trap precisely
// ───────────────────────────────────────────────────────────────────

/// Phase 1 — drive the full production path; the path is expected
/// to surface either a memory fault at `0x00000020` (r61 baseline)
/// OR a different outcome if r62's fix landed.  We emit the
/// register file + trace tail + 16 bytes at the faulting EIP so
/// the round 63 author can replay the disassembly without re-running
/// the harness.
#[test]
fn phase1_capture_trap_eip_registers_and_instruction_bytes() {
    let Some((sb, out)) = drive_to_receive_trap() else {
        eprintln!("round62 phase1: fixture/dll missing or path failed; skipping");
        return;
    };

    eprintln!("round62 phase1: image_base = {:#010x}", out.image_base);
    if let Some(hr) = out.hr {
        eprintln!("round62 phase1: Receive → HRESULT {hr:#010x} (NO TRAP)");
    }
    if let Some(ref e) = out.err {
        eprintln!("round62 phase1: Receive trapped: {e}");
    }

    eprintln!("round62 phase1: register snapshot (at trap):");
    for (name, v) in &out.regs {
        eprintln!("  {name}={v:#010x}");
    }
    eprintln!("round62 phase1: trace-ring tail (entry-EIPs, last 16):");
    for eip in &out.trace_tail {
        let rva = eip.wrapping_sub(out.image_base);
        eprintln!("  eip={eip:#010x}  rva={rva:#010x}");
    }
    if let Some(&trap_eip) = out.trace_tail.last() {
        let rva = trap_eip.wrapping_sub(out.image_base);
        let bytes = dump_bytes(&sb, trap_eip, 16);
        eprintln!(
            "round62 phase1: TRAP-EIP = {trap_eip:#010x} (rva {rva:#010x})  bytes: {}",
            fmt_bytes(&bytes)
        );
    }

    // The diagnostic is the deliverable — the only hard assertion
    // is that the trap-or-S_OK path actually went somewhere (we
    // recorded SOMETHING on the trace ring).
    assert!(
        !out.trace_tail.is_empty() || out.hr.is_some(),
        "expected at least one instruction to execute inside Receive, \
         or Receive to return synchronously without entering guest code"
    );
}

// ───────────────────────────────────────────────────────────────────
// Phase 2 — dump bytes around the trap site + nearby callers
// ───────────────────────────────────────────────────────────────────

/// Phase 2 — produce a wider hex dump around the faulting EIP and
/// the preceding ~16 trace-ring entries.  The intent is to give
/// the round-63 author enough context to walk back to the
/// instruction that loaded the NULL register without re-running
/// the test.
#[test]
fn phase2_disassembly_region_around_trap_site() {
    let Some((sb, out)) = drive_to_receive_trap() else {
        eprintln!("round62 phase2: fixture/dll missing; skipping");
        return;
    };
    let Some(&trap_eip) = out.trace_tail.last() else {
        eprintln!("round62 phase2: no trap or empty trace ring; skipping");
        return;
    };
    let rva = trap_eip.wrapping_sub(out.image_base);
    eprintln!("round62 phase2: image_base={:#010x}", out.image_base);
    eprintln!("round62 phase2: trap_eip={trap_eip:#010x}  rva={rva:#010x}");

    // Dump 32 bytes before and 32 bytes after the trap EIP.
    let start = trap_eip.wrapping_sub(32);
    let bytes = dump_bytes(&sb, start, 64);
    eprintln!(
        "round62 phase2: 64-byte window around trap EIP ({:#010x}..={:#010x}):",
        start,
        start.wrapping_add(63)
    );
    for chunk in bytes.chunks(16).enumerate() {
        let (i, row) = chunk;
        let addr = start.wrapping_add((i as u32) * 16);
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }

    // Dump each unique trace-ring entry's first 8 bytes so we can
    // see the instructions that led to the trap.
    eprintln!("round62 phase2: instruction sequence (last 16 trace-ring entries):");
    let mut seen = std::collections::BTreeSet::new();
    for eip in &out.trace_tail {
        if !seen.insert(*eip) {
            continue;
        }
        let bytes = dump_bytes(&sb, *eip, 8);
        let rva = eip.wrapping_sub(out.image_base);
        eprintln!(
            "  eip={eip:#010x}  rva={rva:#010x}  bytes={}",
            fmt_bytes(&bytes)
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 2b — dump the bytes of the caller function so we can see
// who passed the NULL-containing struct to the trap-function.
// ───────────────────────────────────────────────────────────────────

/// Phase 2b — the trap function is entered at RVA `0x2548` via a
/// `CALL rel32` at RVA `0x1710`.  Dump the prologue of the caller
/// at RVA `0x16fc..0x1720` so we can identify which struct the
/// caller passes in (= what `ecx = [ebp+8]` is at trap-function
/// entry).
#[test]
fn phase2b_dump_caller_of_trap_function() {
    let Some((mut sb, _img)) = load_msadds32() else {
        eprintln!("round62 phase2b: msadds32.ax missing; skipping");
        return;
    };
    // Walk the image_base in case it shifted.
    let _factory = sb
        .dll_get_class_object(&_img, MSADDS_AUDIO_DECODER_CLSID, IID_ICLASSFACTORY)
        .ok();
    let base = _img.image_base;
    eprintln!("round62 phase2b: image_base={base:#010x}");

    // Caller body: RVA 0x16e0..0x1720.
    eprintln!("round62 phase2b: caller bytes 0x16e0..=0x171f:");
    let bytes = dump_bytes(&sb, base + 0x16e0, 64);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x16e0 + (i as u32) * 16;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    // Trap-function body: RVA 0x2540..0x2590.
    eprintln!("round62 phase2b: trap function 0x2540..=0x258f:");
    let bytes = dump_bytes(&sb, base + 0x2540, 80);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x2540 + (i as u32) * 16;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    // The trap-function calls an imported function via
    // `call dword ptr [0x1c40f03c]` at RVA 0x2555.  Dump that IAT
    // entry + the next 32 to see which imports surround it.
    eprintln!("round62 phase2b: IAT region 0x1c40f030..=0x1c40f067:");
    let bytes = dump_bytes(&sb, 0x1c40_f030, 56);
    for (i, row) in bytes.chunks(8).enumerate() {
        let addr = 0x1c40_f030u32 + (i as u32) * 8;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    // Also call dword ptr [0x1c40f040] (= IAT slot) at RVA 0x2576
    // is the second imported call inside the trap function.  We
    // already have a dump of that region above.

    // The caller (0x16ed) called another function at RVA 0x24cc
    // BEFORE the trap call.  That function presumably writes the
    // pointer into `[ebp-0x4]` that the trap function expects to
    // dereference.  Dump that function's body.
    eprintln!("round62 phase2b: first-call target 0x24c0..=0x2547:");
    let bytes = dump_bytes(&sb, base + 0x24c0, 0x88);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x24c0 + (i as u32) * 16;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    // The full enclosing function that calls 0x24cc + 0x2548:
    // back-walk from the caller's RVA 0x16ed → likely a `Receive`
    // implementation in the codec's input pin.  Dump 0x1500..0x1740.
    eprintln!("round62 phase2b: enclosing function 0x1500..=0x173f:");
    let bytes = dump_bytes(&sb, base + 0x1500, 0x240);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x1500 + (i as u32) * 16;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }

    // The function at 0x235c is the one that POPULATES the caller's
    // `[ebp-0x4]` BEFORE the trap-pair (0x24cc, 0x2548) is called.
    // Dump it so we can identify what host wiring it expects.
    eprintln!("round62 phase2b: populator function 0x2350..=0x24bf:");
    let bytes = dump_bytes(&sb, base + 0x2350, 0x170);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x2350 + (i as u32) * 16;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    // The malloc at the populator (0x23d2 → 0x6ae4) is a candidate
    // for returning NULL.  Dump its body + entry.
    eprintln!("round62 phase2b: malloc target 0x6ad0..=0x6b3f:");
    let bytes = dump_bytes(&sb, base + 0x6ad0, 0x70);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x6ad0 + (i as u32) * 16;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    // The init/ctor at 0x25aa is the other place the populator can
    // return E_OUTOFMEMORY via the [ebp-0x4] = 0x8007000e path.
    // Dump the constructor + init body.
    eprintln!("round62 phase2b: buffer-pool ctor 0x2580..=0x25cf:");
    let bytes = dump_bytes(&sb, base + 0x2580, 0x50);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x2580 + (i as u32) * 16;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    eprintln!("round62 phase2b: buffer-pool init 0x25aa..=0x2649:");
    let bytes = dump_bytes(&sb, base + 0x25aa, 0xa0);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x25aa + (i as u32) * 16;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    // Populator's `helper+0x1c` call target at 0x5ce8.
    eprintln!("round62 phase2b: helper-method target 0x5ce0..=0x5d4f:");
    let bytes = dump_bytes(&sb, base + 0x5ce0, 0x70);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x5ce0 + (i as u32) * 16;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    // Helper computing size at 0x6ceb.
    eprintln!("round62 phase2b: size-calc target 0x6ce0..=0x6d5f:");
    let bytes = dump_bytes(&sb, base + 0x6ce0, 0x80);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x6ce0 + (i as u32) * 16;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    // Wider IAT context.
    eprintln!("round62 phase2b: IAT region 0x1c40f080..=0x1c40f0bf:");
    let bytes = dump_bytes(&sb, 0x1c40_f080, 64);
    for (i, row) in bytes.chunks(8).enumerate() {
        let addr = 0x1c40_f080u32 + (i as u32) * 8;
        eprintln!("  {addr:#010x}: {}", fmt_bytes(row));
    }
    // Resolve the IAT slot at 0x1c40f094 (the malloc-like target
    // the populator function at 0x235e calls via thunk 0x6ae4).
    let malloc_iat_value = sb.mmu.load32(0x1c40_f094).unwrap_or(0);
    eprintln!(
        "round62 phase2b: IAT[0x1c40f094] = {malloc_iat_value:#010x}; is_thunk={}",
        sb.registry.is_thunk(malloc_iat_value)
    );
    if let Some(entry) = sb.registry.entry(malloc_iat_value) {
        eprintln!(
            "round62 phase2b: 0x1c40f094 resolves to {}!{}",
            entry.dll, entry.name
        );
    }
    // Dump precise bytes at 0x6ae0..0x6af0 + decode the call from
    // populator (0x23d2) and the trap function's calls.
    eprintln!("round62 phase2b: bytes 0x6ae0..0x6af0 (byte-by-byte):");
    for off in 0..0x10u32 {
        let addr = 0x1c40_6ae0 + off;
        let b = sb.mmu.load8(addr).unwrap_or(0);
        eprintln!("  [{addr:#010x}] = {b:#04x}");
    }
    // Reconstruct populator call target (the call opcode is at
    // RVA 0x23d4 after `6a 28` push).
    let call_eip = 0x1c40_23d4;
    let rel = sb.mmu.load32(call_eip + 1).unwrap_or(0);
    let target = call_eip.wrapping_add(5).wrapping_add(rel);
    eprintln!(
        "round62 phase2b: call @ 0x23d2 rel={rel:#010x} → target={target:#010x} (rva {:#x})",
        target.wrapping_sub(base)
    );
    // Read the JMP instruction at the target and decode the IAT slot.
    let target_b0 = sb.mmu.load8(target).unwrap_or(0);
    let target_b1 = sb.mmu.load8(target + 1).unwrap_or(0);
    eprintln!(
        "round62 phase2b: target bytes [{target:#010x}..]={:#04x} {:#04x} {:#04x} {:#04x} {:#04x} {:#04x}",
        target_b0,
        target_b1,
        sb.mmu.load8(target + 2).unwrap_or(0),
        sb.mmu.load8(target + 3).unwrap_or(0),
        sb.mmu.load8(target + 4).unwrap_or(0),
        sb.mmu.load8(target + 5).unwrap_or(0),
    );
    if target_b0 == 0xff && target_b1 == 0x25 {
        let slot = sb.mmu.load32(target + 2).unwrap_or(0);
        let v = sb.mmu.load32(slot).unwrap_or(0);
        let name = sb
            .registry
            .entry(v)
            .map(|e| format!("{}!{}", e.dll, e.name))
            .unwrap_or_else(|| "<not a thunk>".to_string());
        eprintln!(
            "round62 phase2b: populator's malloc-like call → jmp [{slot:#010x}]; IAT={v:#010x}  {name}"
        );
    }
    // Also decode the constructor call: at populator RVA 0x23dc
    // there's `e8 9d 01 00 00`.
    let ctor_call_eip = 0x1c40_23dc;
    let ctor_rel = sb.mmu.load32(ctor_call_eip + 1).unwrap_or(0);
    let ctor_target = ctor_call_eip.wrapping_add(5).wrapping_add(ctor_rel);
    eprintln!(
        "round62 phase2b: constructor call @ 0x23dc rel={ctor_rel:#010x} → target={ctor_target:#010x} (rva {:#x})",
        ctor_target.wrapping_sub(base)
    );
    // And the init call: at populator RVA 0x23fd there's `e8 a8 01 00 00`.
    let init_call_eip = 0x1c40_23fd;
    let init_rel = sb.mmu.load32(init_call_eip + 1).unwrap_or(0);
    let init_target = init_call_eip.wrapping_add(5).wrapping_add(init_rel);
    eprintln!(
        "round62 phase2b: init call @ 0x23fd rel={init_rel:#010x} → target={init_target:#010x} (rva {:#x})",
        init_target.wrapping_sub(base)
    );
    // Dump bytes at constructor entry + init entry so we can see
    // whether they would return failure.
    eprintln!(
        "round62 phase2b: bytes at constructor target {ctor_target:#010x}: {}",
        fmt_bytes(&dump_bytes(&sb, ctor_target, 32))
    );
    eprintln!(
        "round62 phase2b: bytes at init target {init_target:#010x}: {}",
        fmt_bytes(&dump_bytes(&sb, init_target, 64))
    );

    // Resolve EVERY IAT slot in the 0x1c40f080..0x1c40f0bf region
    // so we can identify the codec's imported functions in this
    // group.
    eprintln!("round62 phase2b: resolve IAT 0x1c40f080..=0x1c40f0bf:");
    for off in (0u32..0x40).step_by(4) {
        let slot = 0x1c40_f080 + off;
        let v = sb.mmu.load32(slot).unwrap_or(0);
        let name = sb
            .registry
            .entry(v)
            .map(|e| format!("{}!{}", e.dll, e.name))
            .unwrap_or_else(|| "<not a thunk>".to_string());
        eprintln!("  [{slot:#010x}] = {v:#010x}  {name}");
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 2c — function-entry walk: which codec routines were the
// LAST entered before the trap?  Knowing this names the call stack
// without having to disassemble every instruction in the ring.
// ───────────────────────────────────────────────────────────────────

/// Phase 2c — walk the (large) trace ring and flag every EIP that
/// follows a `call rel32` (i.e. function-entry sites).  Print the
/// last 30 such entries — these are the functions the codec
/// stepped into on the way to the trap, in order.  This lets us
/// see what the LAST call before the trap-pair (0x24cc, 0x2548)
/// was, and whether anything between the populator (0x235c) and
/// the trap-function ran that would have reset the out-slot.
#[test]
fn phase2c_walk_function_entries_in_trace_ring() {
    let Some((sb, out)) = drive_to_receive_trap() else {
        eprintln!("round62 phase2c: fixture/dll missing; skipping");
        return;
    };
    let base = out.image_base;
    eprintln!("round62 phase2c: image_base={base:#010x}");

    // A function-entry EIP is one where the byte at that EIP is
    // 0x55 (push ebp) or 0x8B (mov ebp, esp prefix).  Filter the
    // ring to those.
    let mut entries: Vec<u32> = Vec::new();
    for &eip in &out.trace_tail {
        // Only consider in-codec EIPs.
        if eip.wrapping_sub(base) > 0x100_000 {
            continue;
        }
        if let Ok(b) = sb.mmu.load8(eip) {
            if b == 0x55 {
                entries.push(eip);
            }
        }
    }
    eprintln!(
        "round62 phase2c: {} function-entry EIPs (push ebp) in the last \
         {} executed instructions:",
        entries.len(),
        out.trace_tail.len()
    );
    let take = entries.len().min(40);
    let start = entries.len().saturating_sub(take);
    for &eip in &entries[start..] {
        let rva = eip.wrapping_sub(base);
        eprintln!("  entry eip={eip:#010x}  rva={rva:#010x}");
    }

    // Also surface every CALL site (instructions starting with
    // 0xe8 = call rel32 or 0xff /2 = call indirect) in the tail.
    // This is the call sequence that led to the trap.
    let mut calls: Vec<(u32, u32)> = Vec::new();
    for &eip in &out.trace_tail {
        if eip.wrapping_sub(base) > 0x100_000 {
            continue;
        }
        if let Ok(b) = sb.mmu.load8(eip) {
            if b == 0xe8 {
                // call rel32 — decode target.
                if let Ok(rel) = sb.mmu.load32(eip + 1) {
                    let target = eip.wrapping_add(5).wrapping_add(rel);
                    calls.push((eip, target));
                }
            } else if b == 0xff {
                // ff /2 = call indirect.  Decode target = next dword.
                if let Ok(b2) = sb.mmu.load8(eip + 1) {
                    if b2 == 0x15 {
                        // call dword ptr [imm32]
                        if let Ok(slot) = sb.mmu.load32(eip + 2) {
                            calls.push((eip, slot));
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "round62 phase2c: {} call sites in trace ring (last 30 shown):",
        calls.len()
    );
    let take = calls.len().min(30);
    let start = calls.len().saturating_sub(take);
    for &(eip, target) in &calls[start..] {
        let rva = eip.wrapping_sub(base);
        let tgt_rva = target.wrapping_sub(base);
        eprintln!("  call @ eip={eip:#010x} (rva {rva:#010x}) → target={target:#010x} (rva {tgt_rva:#010x})");
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 2d — did the codec EVER step into the buffer-pool
// populator (0x235c) during this Receive call?
// ───────────────────────────────────────────────────────────────────

/// Phase 2d — the populator function at 0x235c is what's supposed
/// to write a fresh buffer-object pointer into the caller's
/// `[ebp-0x4]` slot before the trap-function (0x2548) is called.
/// Surface whether that function's first instruction (RVA 0x235c)
/// shows up in the visited-EIPs set for this Receive call.  If
/// NOT, then the codec entered 0x2548 WITHOUT ever calling the
/// populator — confirming the bug is in the codec's own
/// state-machine, not in our host wiring.
#[test]
fn phase2d_did_populator_run_in_receive_body() {
    let Some((_sb, out)) = drive_to_receive_trap() else {
        eprintln!("round62 phase2d: fixture/dll missing; skipping");
        return;
    };
    let base = out.image_base;
    // Correct function-start RVAs (push-ebp byte):
    //   - enclosing Receive at 0x1501
    //   - populator (POP buffer-pool) at 0x235e
    //   - list-insert (sorted) at 0x24cc → actual `55` is at 0x24ce
    //   - trap function (LIFO push) at 0x2548
    let populator_va = base + 0x235e;
    let trap_func_va = base + 0x2548;
    let pop_va = base + 0x24ce;
    let enclosing_va = base + 0x1501;
    eprintln!(
        "round62 phase2d: visited_eips total entries = {}",
        out.visited.len()
    );
    eprintln!(
        "  populator (0x235c) entered = {}",
        out.visited.contains(&populator_va)
    );
    eprintln!(
        "  list-insert (0x24cc) entered = {}",
        out.visited.contains(&pop_va)
    );
    eprintln!(
        "  trap-function (0x2548) entered = {}",
        out.visited.contains(&trap_func_va)
    );
    eprintln!(
        "  enclosing function (0x1500) entered = {}",
        out.visited.contains(&enclosing_va)
    );
    // Print all unique EIPs at function-prologue offsets in the
    // codec image.
    let prologue_eips: Vec<u32> = out
        .visited
        .iter()
        .filter(|&&eip| eip.wrapping_sub(base) < 0x10_0000)
        .copied()
        .collect();
    eprintln!(
        "round62 phase2d: total in-codec visited EIPs = {}",
        prologue_eips.len()
    );
    // Filter to push-ebp prologues only — count how many distinct
    // functions the codec entered.
    let mut function_entries: Vec<u32> = Vec::new();
    for &eip in &prologue_eips {
        if let Ok(b) = _sb.mmu.load8(eip) {
            if b == 0x55 {
                // also check it's preceded by `ret` or function
                // boundary — best-effort: check the byte before is
                // 0xc3 (ret) or 0xc2 (ret imm16) or 0xcc (int3).
                let prev = _sb.mmu.load8(eip.wrapping_sub(1)).unwrap_or(0);
                let aligned = eip & 0xf == 0;
                if prev == 0xc3 || prev == 0xcc || prev == 0xc2 || aligned {
                    function_entries.push(eip);
                }
            }
        }
    }
    eprintln!(
        "round62 phase2d: distinct function-entry EIPs visited = {}",
        function_entries.len()
    );
    for &eip in &function_entries {
        let rva = eip.wrapping_sub(base);
        eprintln!("  function entry at rva {rva:#06x} (eip {eip:#010x})");
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 4 — fix attempt: drive `IPin::NewSegment` before `Receive`
// ───────────────────────────────────────────────────────────────────

/// Phase 4 — the codec's Receive path falls into a cleanup branch
/// that assumes `[ebp-0x4]` (a local buffer-pointer) has been
/// populated by the buffer-pool POP function at RVA 0x235e.  The
/// POP function's success in turn requires `this[0x160]` to be
/// either non-NULL OR for `operator new` + the in-tree init
/// constructor to both succeed.  On the FIRST Receive call,
/// `this[0x160]` is NULL by codec construction; whether
/// malloc+init succeed depends on a size value derived from the
/// negotiated `WAVEFORMATEX` AND a state field the codec
/// initialises during `NewSegment` (the `start`/`stop`/`rate`
/// segment seed).
///
/// This phase tries calling `IPin::NewSegment(start=0,
/// stop=10000000 (1s), rate=1.0)` on the codec's input pin
/// BEFORE `Receive` and reports whether the trap shifts or
/// clears.  Driven via the `R62_DRIVE_NEW_SEGMENT` env var so the
/// baseline (phase 1-3) is preserved.
#[test]
fn phase4_drive_new_segment_before_receive() {
    std::env::set_var("R62_DRIVE_NEW_SEGMENT", "1");
    let outcome = drive_to_receive_trap();
    std::env::remove_var("R62_DRIVE_NEW_SEGMENT");
    let Some((_sb, out)) = outcome else {
        eprintln!("round62 phase4: fixture/dll missing; skipping");
        return;
    };
    if let Some(hr) = out.hr {
        eprintln!("round62 phase4: Receive (with NewSegment) → HRESULT {hr:#010x}");
        if hr == 0 {
            eprintln!("round62 phase4: BREAKTHROUGH — Receive returned S_OK.");
        }
    }
    if let Some(ref e) = out.err {
        eprintln!("round62 phase4: Receive trap = {e}");
        if let Some(&trap_eip) = out.trace_tail.last() {
            let rva = trap_eip.wrapping_sub(out.image_base);
            eprintln!("round62 phase4: trap_eip = {trap_eip:#010x}  rva = {rva:#010x}");
        }
    }
    eprintln!("round62 phase4: register snapshot:");
    for (name, v) in &out.regs {
        eprintln!("  {name}={v:#010x}");
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 3 — regression guard
// ───────────────────────────────────────────────────────────────────

/// Phase 3 — the regression guard.  The previous-round baseline is
/// a memory fault at exactly `0x00000020`.  Round 62's job is to
/// IMPROVE on that, not regress to `VFW_E_NOT_COMMITTED` or to a
/// different NULL-at-low-offset trap.
///
/// Acceptable outcomes (each will keep this test green):
///   (a) HRESULT S_OK (or anything > 0 but high bit clear) — PCM
///       bytes likely available downstream.
///   (b) A different memory-fault address (e.g. `0x00000024`,
///       `0x0000_0040`, etc.) — round 63 picks up the next blocker.
///   (c) `VFW_E_NOT_COMMITTED` (`0x80040209`) — UNACCEPTABLE; r61
///       got past this and r62 must not undo that progress.
///   (d) `0x00000020` exactly — still the r61 baseline; informational.
#[test]
fn phase3_regression_guard_no_return_to_not_committed() {
    let Some((_sb, out)) = drive_to_receive_trap() else {
        eprintln!("round62 phase3: fixture/dll missing; skipping");
        return;
    };
    if let Some(hr) = out.hr {
        assert_ne!(
            hr, 0x8004_0209,
            "round62 regression: Receive surfaced VFW_E_NOT_COMMITTED again"
        );
        eprintln!("round62 phase3: Receive returned HRESULT {hr:#010x} (no trap)");
    }
    if let Some(ref e) = out.err {
        eprintln!("round62 phase3: Receive trap = {e}");
        // The only regression we guard against is going BACK to
        // VFW_E_NOT_COMMITTED.  Any other trap (including the
        // existing 0x20 baseline) is acceptable round-by-round.
    }
}
