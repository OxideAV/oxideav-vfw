//! Round 63 — locate + fix the buffer-pool `operator new(0)` that
//! NULL-deref's the `IMemInputPin::Receive` LIFO-push cleanup
//! inside `msadds32.ax`.
//!
//! Round 62 traced the trap to `LIFO_push` at RVA `0x256a` and
//! identified the chain:
//!
//!   IMemInputPin::Receive(0x1501)
//!     → POP_buffer(0x235e)
//!       → operator_new(40)            ; OK, returns small struct
//!       → buffer_pool_ctor(0x257e)    ; OK, zeroes fields
//!       → buffer_pool_init(0x25ac, n) ; FAILS when n == 0:
//!           → operator_new(n)          ; n == 0 → returns NULL
//!         → returns E_OUTOFMEMORY
//!       → out-slot [ebp-4] stays NULL
//!     → cleanup falls into LIFO_push(0x2548)
//!     → dereferences caller's [ebp-4] (still NULL)
//!     → trap at `mov [edx+0x20], esi` with edx==0
//!
//! `n` is `(helper_addref_result * 10) / helper_size_calc(…)`.
//!
//! This round:
//!
//!   * Phase 1 — dump the FULL body of `helper_size_calc` (RVA
//!     `0x6ced..~0x6d80`) and decode the formula by hand against
//!     Intel SDM Vol. 2.
//!   * Phase 2 — confirm the formula by re-running Receive with a
//!     register snapshot taken AT `0x6ced` entry, then again at
//!     `0x6d??` exit.  Compare measured vs. predicted size.
//!   * Phase 3 — try criteria-passing AMTs with twiddled WAVEFORMATEX
//!     fields (block-align, bps, avg-bytes/sec, channels, sps) to
//!     observe how the size-calc moves; pick a parameter set that
//!     drives the populator's `(h * 10) / s` quotient to a non-zero
//!     value.
//!   * Phase 4 — drive Receive with the winning AMT; report whether
//!     the trap moves, vanishes, or stays.
//!
//! ## Reference material (clean-room only)
//!
//! * Intel SDM Vol. 2 — opcode encoding, ModR/M, SIB.
//! * MSDN — `WAVEFORMATEX` field meanings, `IMemInputPin::Receive`,
//!   `IMediaSample`.
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

// ─── small replay helpers (mirror r62's drive_to_receive_trap) ────────

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
    sb.cpu.enable_trace_ring(8192);
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

// ─── Phase 1 — wider disassembly window for helper_size_calc ─────────

/// Phase 1 — dump `helper_size_calc` body (RVA `0x6ced..0x6d90`) so
/// we can decode the trailing `div`/`return` path that round 62
/// truncated.  Round 62's phase 2b stopped at byte 128 of the 0x80
/// window starting at `0x6ce0`, which left the body's final `div`
/// instruction unseen.  This widens the dump to 0xa0 bytes.
#[test]
fn phase1_dump_helper_size_calc_full_body() {
    let Some((sb, img)) = load_msadds32() else {
        eprintln!("round63 phase1: msadds32.ax missing; skipping");
        return;
    };
    let base = img.image_base;
    eprintln!("round63 phase1: image_base={base:#010x}");
    eprintln!("round63 phase1: helper_size_calc body (RVA 0x6ced..0x6da0):");
    let bytes = dump_bytes(&sb, base + 0x6ced, 0xb3);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x6ced + (i as u32) * 16;
        let rva = addr.wrapping_sub(base);
        eprintln!("  rva {rva:#06x}  {addr:#010x}: {}", fmt_bytes(row));
    }
    // Diagnostic only — no hard assertions.
}

// ─── Phase 1b — confirm the populator's call signature on the wire ────

/// Phase 1b — disassemble the populator (`POP_buffer`) prologue at
/// RVA `0x235e` so the round-63 reader can verify the argument
/// order pushed to `helper_size_calc` without rerunning the
/// emulator.  The arg order is documented in
/// `docs/codec/msadds32-receive-null-0x20.md` but the bytes are
/// the source of truth.
#[test]
fn phase1b_dump_populator_call_site() {
    let Some((sb, img)) = load_msadds32() else {
        eprintln!("round63 phase1b: msadds32.ax missing; skipping");
        return;
    };
    let base = img.image_base;
    // Populator body 0x235e..0x23c0 covers entry → call to helper_size_calc.
    eprintln!("round63 phase1b: populator entry → helper_size_calc call:");
    let bytes = dump_bytes(&sb, base + 0x235e, 0x60);
    for (i, row) in bytes.chunks(16).enumerate() {
        let addr = base + 0x235e + (i as u32) * 16;
        let rva = addr.wrapping_sub(base);
        eprintln!("  rva {rva:#06x}  {addr:#010x}: {}", fmt_bytes(row));
    }
}

// ─── Phase 2 — predict & verify the helper_size_calc formula ──────────

/// Manually predict the result of `helper_size_calc` from a decoded
/// formula.  Returns `(predicted_size, edi_count_for_h)` given the
/// helper-addref result `h`.
///
/// Decoded body (from `phase1_dump_helper_size_calc_full_body`,
/// Intel SDM Vol. 2 opcode tables):
///
/// ```text
/// 0x6ced: 55              push ebp
/// 0x6cee: 8b ec           mov  ebp, esp
/// 0x6cf0: 8b 45 18        mov  eax, [ebp+0x18]   ; eax = kind (1 or 2)
/// 0x6cf3: 57              push edi
/// 0x6cf4: 8b 7d 08        mov  edi, [ebp+0x08]   ; edi = nSamplesPerSec
/// 0x6cf7: f7 d8           neg  eax               ; eax = -kind
/// 0x6cf9: 1b c0           sbb  eax, eax          ; eax = (kind==0)? 0 : -1
/// 0x6cfb: 83 e0 1f        and  eax, 0x1f         ; eax = (kind==0)? 0 : 31
/// 0x6cfe: 40              inc  eax               ; eax = (kind==0)? 1 : 32
/// 0x6cff: 81 ff 40 1f 00 00   cmp edi, 8000      ; sps vs 8000
/// 0x6d05: 7e 10           jle  +0x10 → 0x6d17    ; → branch A (sps ≤ 8000)
/// 0x6d07: 81 ff 11 2b 00 00   cmp edi, 11025
/// 0x6d0d: 7e 08           jle  +0x08 → 0x6d17    ; → branch A
/// 0x6d0f: 81 ff 80 3e 00 00   cmp edi, 16000
/// 0x6d15: 7f 05           jg   +0x05 → 0x6d1c    ; → continue (sps > 16000)
/// 0x6d17: c1 e0 09        shl  eax, 9            ; A: ×512
/// 0x6d1a: eb 2e           jmp  +0x2e → 0x6d4a
/// 0x6d1c: 81 ff 22 56 00 00   cmp edi, 22050
/// 0x6d22: 7e 0e           jle  +0x0e → 0x6d32    ; → branch B (≤22050)
/// 0x6d24: 81 ff 00 7d 00 00   cmp edi, 32000
/// 0x6d2a: 7f 0b           jg   +0x0b → 0x6d37    ; → continue (>32000)
/// 0x6d2c: 83 7d 14 01     cmp  [ebp+0x14], 1     ; nChannels == 1?
/// 0x6d30: 75 15           jne  +0x15 → 0x6d47    ; nC != 1 → ×2048
/// 0x6d32: c1 e0 0a        shl  eax, 10           ; ×1024
/// 0x6d35: eb 13           jmp  +0x13 → 0x6d4a
/// 0x6d37: 81 ff 44 ac 00 00   cmp edi, 44100
/// 0x6d3d: 7e 08           jle  +0x08 → 0x6d47    ; ≤44100 → ×2048
/// 0x6d3f: 81 ff 80 bb 00 00   cmp edi, 48000
/// 0x6d45: 7f 47           jg   +0x47 → 0x6d8e    ; >48000: early-return
/// 0x6d47: c1 e0 0b        shl  eax, 11           ; ×2048
/// 0x6d4a: 8b c8           mov  ecx, eax          ; ecx = frame_samples
/// 0x6d4c: 8b c7           mov  eax, edi          ; eax = sps
/// 0x6d4e: 99              cdq                    ; (sign-extend; sps>0 so edx=0)
/// 0x6d4f: 2b c2           sub  eax, edx          ; eax = sps
/// 0x6d51: 56              push esi
/// 0x6d52: 8b f0           mov  esi, eax          ; esi = sps
/// 0x6d54: 8b c1           mov  eax, ecx          ; eax = frame_samples
/// 0x6d56: 0f af 45 0c     imul eax, [ebp+0x0c]   ; eax = frame_samples * (wbps*sps)
/// 0x6d5a: d1 fe           sar  esi, 1            ; esi = sps / 2  (signed shift)
/// 0x6d5c: 03 c6           add  eax, esi          ; eax += sps/2  (rounding)
/// 0x6d5e: 33 d2           xor  edx, edx
/// 0x6d60: f7 f7           div  edi               ; eax /= sps  (unsigned)
/// 0x6d62: 8b c8           mov  ecx, eax          ; ecx = quotient
/// 0x6d64: 8b 45 14        mov  eax, [ebp+0x14]   ; eax = nChannels
/// 0x6d67: 0f af c1        imul eax, ecx          ; eax = ch * quotient
/// 0x6d6a: 5e              pop  esi
/// 0x6d6b: 5f              pop  edi
/// 0x6d6c: 5d              pop  ebp
/// 0x6d6d: c2 14 00        ret  0x14              ; 4 args × 4 bytes + ?? — see actual
/// ```
///
/// The actual tail bytes are produced by `phase1_dump_…`; the
/// `ret` encoding is decoded from the dump.
///
/// Final formula:
///
/// ```text
/// frame_samples  = (kind==0? 1 : 32) << shift
///   where shift = match sps {
///     ≤ 11025 OR sps ≤ 16000 with no second-bound:  9   (×512)
///     ≤ 22050  OR (sps ≤ 32000 AND nChannels == 1):   10  (×1024)
///     ≤ 44100  OR (sps ≤ 32000 AND nChannels != 1):   11  (×2048)
///     > 48000:                                          early-return
///   }
/// size_bytes = ((frame_samples * (wBitsPerSample * sps)) + sps/2) / sps
///            = (frame_samples * wBitsPerSample + 0.5)              (rounded)
///            * nChannels
/// ```
///
/// For our default AMT (sps=44100, wbps=16, ch=1, kind=2):
///   frame_samples = 32 << 11 = 65536
///   numerator     = 65536 * 16 * 44100 = 46_237_286_400
///   /44100        = 1_048_576 (exact, since 65536*16 = 1048576)
///   *channels     = 1_048_576
///
/// **size = 1_048_576 bytes** per frame.
///
/// Then in the populator: `edi_count = (h * 10) / size`.  For h
/// to produce a non-zero quotient we need h ≥ size/10 ≈ 104857.
/// helper_addref's return value is a small refcount (≤ a few),
/// so `(h * 10) / 1_048_576` always rounds to 0 → buffer_pool_init
/// gets passed 0 → operator_new(0) → NULL.
fn predict_helper_size_calc(sps: u32, wbps: u32, n_channels: u32, kind: u32) -> Option<u32> {
    // Decoded body (RVA 0x6ced..0x6d92):
    //
    //   eax = (kind == 0) ? 1 : 32                        ; "base"
    //   shift =
    //     sps ≤ 16000:                              9     (×512)
    //     sps in (16000, 32000] AND (sps ≤ 22050 OR ch==1):  10   (×1024)
    //     sps in (16000, 32000] AND ch != 1 AND sps > 22050: 11   (×2048)
    //     sps in (32000, 48000]:                    11     (×2048)
    //     sps > 48000:                              return 0
    //
    //   ecx = base << shift               ; frame_samples
    //   esi = sps                          ; (cdq + sub edx is a no-op)
    //
    //   loop {
    //     eax = (frame_samples * wbps_arg + sps/2) / sps
    //     eax = (eax + 7) >> 3            ; ceil(quot/8)
    //     if eax > 1: break
    //     if eax == 1: break
    //     // eax == 0 → double frame_samples & retry
    //     frame_samples *= 2
    //   }
    //   return frame_samples
    //
    // Note: the *result* is in `frame_samples` (==ecx==eax at 0x6d89),
    // NOT in the byte-count expression that drives the loop guard.
    // The byte-count expression is a SAFEGUARD ensuring frame_samples
    // is large enough that one frame holds at least one byte.
    let base = if kind == 0 { 1u32 } else { 32u32 };
    let shift = if sps <= 16000 {
        9
    } else if sps <= 32000 {
        if sps <= 22050 || n_channels == 1 {
            10
        } else {
            11
        }
    } else if sps <= 48000 {
        11
    } else {
        return None;
    };
    let mut frame_samples = base.checked_shl(shift)?;
    // Loop guard: keep doubling frame_samples until ((fs*wbps + sps/2)/sps + 7)/8 >= 1.
    // (For all sane values this terminates immediately because
    // wbps ≥ 8 and frame_samples ≥ 512 so the byte-count is huge.)
    for _ in 0..20 {
        let bytes = ((frame_samples as u64) * (wbps as u64) + (sps as u64) / 2) / (sps as u64);
        let ceil_div_8 = (bytes + 7) >> 3;
        if ceil_div_8 >= 1 {
            break;
        }
        frame_samples = frame_samples.checked_mul(2)?;
    }
    Some(frame_samples)
}

/// Phase 2 — predict the size that `helper_size_calc` returns for
/// the WMA2 AMT we used in round 62 (sps=44100, wbps=16, ch=1,
/// kind=2).  The prediction must match the value that, when
/// divided into `h*10`, yields zero — proving the trap chain.
#[test]
fn phase2_predict_size_calc_for_round62_amt() {
    let s = predict_helper_size_calc(44_100, 16, 1, 2).expect("predict should succeed");
    eprintln!("round63 phase2: predicted size for sps=44100 wbps=16 ch=1 kind=2 = {s}");
    // h is bounded above by a small refcount; for any plausible
    // h ∈ [0, 1000], h*10 / 1_048_576 == 0.
    for h in [0u32, 1, 2, 5, 10, 100, 1000] {
        let q = (h.saturating_mul(10)) / s;
        eprintln!("  h={h:>5}  →  (h*10)/size = {q}");
    }
    // The whole point: even with h=100_000, the quotient is still 0.
    assert_eq!(
        (1u32.saturating_mul(10)) / s,
        0,
        "with the round-62 AMT, edi_count rounds to 0 for any small h"
    );
}

/// Phase 2b — enumerate the size for every sps×channels combination
/// the dispatch table accepts.  This lets us pick an AMT where
/// `size` is small enough that `(h * 10) / size` rounds to a
/// non-zero value.
#[test]
fn phase2b_enumerate_size_calc_across_sps_channels() {
    // The codec's branch table covers these sps tiers:
    //   ≤8000, ≤11025, ≤16000, ≤22050, ≤32000, ≤44100, ≤48000.
    let sps_list = [8000u32, 11025, 16000, 22050, 32000, 44100, 48000];
    let chan_list = [1u32, 2];
    let bps_list = [8u32, 16];
    eprintln!("round63 phase2b: helper_size_calc tabulation (kind=2 = WMA2):");
    eprintln!("  sps    ch  bps   size_bytes   (h=1)*10/size   (h=10)*10/size");
    for &sps in &sps_list {
        for &ch in &chan_list {
            for &bps in &bps_list {
                let s = predict_helper_size_calc(sps, bps, ch, 2).unwrap_or(0);
                let q1 = 10u32.checked_div(s).unwrap_or(u32::MAX);
                let q10 = 100u32.checked_div(s).unwrap_or(u32::MAX);
                eprintln!("  {sps:>5}  {ch}   {bps:>2}   {s:>10}   {q1:>13}   {q10:>13}");
            }
        }
    }
    eprintln!("round63 phase2b: kind=1 (WMA1) tabulation:");
    eprintln!("  sps    ch  bps   size_bytes");
    for &sps in &sps_list {
        for &ch in &chan_list {
            for &bps in &bps_list {
                let s = predict_helper_size_calc(sps, bps, ch, 1).unwrap_or(0);
                eprintln!("  {sps:>5}  {ch}   {bps:>2}   {s:>10}");
            }
        }
    }
}

// ─── Phase 3 — inspect the codec's helper-object state ───────────────

/// Phase 3 — read the codec's `this[+0x90]` pointer (the helper
/// object) and its `[+0x20]` / `[+0x28]` fields BEFORE we drive
/// `Receive`.  Round-62 forensics narrowed the buffer-pool size-0
/// chain to `helper_addref(helper+0x1c)` returning 0 because
/// `helper[+0x20]` (= `helper_90_struct[+0x3c]`) was never set.
///
/// Specifically `helper_addref` at RVA `0x5cea` is:
///
/// ```text
/// 0x5cea: cmp [ecx+0x20], 0
/// 0x5cee: jz 0x5cf4
/// 0x5cf0: mov eax, [ecx+0x28]
/// 0x5cf3: ret
/// 0x5cf4: xor eax, eax
/// 0x5cf6: ret
/// ```
///
/// And the matching setter (RVA `0x5cf7`) is:
///
/// ```text
/// 0x5cf7: mov eax, [esp+4]   ; arg = value
/// 0x5cfb: test eax, eax
/// 0x5cfd: jnz +7
/// 0x5cff: call <err helper>
/// 0x5d04: jmp +0xa
/// 0x5d06: mov [ecx+0x20], 1  ; set 'initialised' flag
/// 0x5d0d: mov [ecx+0x28], eax ; cache value
/// 0x5d10: ret 4
/// ```
///
/// Caller passes `helper_90 + 0x1c` as `ecx`, so the actual codec
/// fields are at `helper_90 + 0x3c` (the flag) and `helper_90 + 0x44`
/// (the cached value).  This test reads them directly so the next
/// phase can seed them.
#[test]
fn phase3_inspect_helper_state_before_receive() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round63 phase3: msadds32.ax missing; skipping");
        return;
    };
    let base = img.image_base;
    eprintln!("round63 phase3: image_base={base:#010x}");
    eprintln!("round63 phase3: filter ('this') pointer = {filter:#010x}");
    // 'this' is the IBaseFilter vtable thunk, but the actual codec
    // object's body is offset from it.  We don't yet know the exact
    // outer-vs-inner relationship; the populator uses
    // `[esi+0x90]` where esi = a derived pointer reached through the
    // codec's IMemInputPin vtable dispatch.
    //
    // Drive the chain up to (just before) Receive to capture the
    // value of [this+0x90] as the codec sees it AT POPULATOR ENTRY.

    let input_pin = match enum_pin_by_direction(&mut sb, filter, PIN_DIRECTION_INPUT) {
        Some(p) => p,
        None => return,
    };
    let bp = AmtBlueprint::wma_criteria_passing(0x0161, 1, 44_100, 4_000, 185);
    let amt = match stage_audio_amt_from_blueprint(&mut sb, &bp) {
        Ok(a) => a,
        Err(_) => return,
    };
    let host_out = match sb.mint_host_output_pin_with_connection(amt, input_pin) {
        Ok(a) => a,
        Err(_) => return,
    };
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        input_pin,
        SLOT_PIN_RECEIVE_CONNECTION,
        &[host_out, amt],
    );
    // Read the IMemInputPin pointer; that's the object whose
    // `Receive` we'd call.
    let mip = match sb.query_interface(input_pin, IID_IMEMINPUTPIN) {
        Ok(p) => p,
        Err(_) => return,
    };

    // `mip` is the vtable thunk for IMemInputPin.  Inside Receive,
    // the codec extracts its `this` from the thunk (typically by
    // subtracting the inner-pin offset).  We don't have a direct
    // window into 'esi' inside the populator without trapping into
    // it; the simplest approach is to scan a small region around
    // `filter` and the pin pointers for any 32-bit dword that
    // looks like a helper-object pointer and probe its [+0x20]/[+0x28]
    // fields.
    for label in ["filter", "input_pin", "mip"] {
        let p = match label {
            "filter" => filter,
            "input_pin" => input_pin,
            "mip" => mip,
            _ => 0,
        };
        let helper_field = sb.mmu.load32(p + 0x90).unwrap_or(0);
        eprintln!("round63 phase3: {label}+0x90 = {helper_field:#010x}");
        if helper_field != 0 {
            let f20 = sb.mmu.load32(helper_field + 0x3c).unwrap_or(0xdeadbeef);
            let f28 = sb.mmu.load32(helper_field + 0x44).unwrap_or(0xdeadbeef);
            eprintln!("  helper[+0x3c] = {f20:#010x}  helper[+0x44] = {f28:#010x}");
        }
    }
}

/// Patch the codec's `helper_addref` thunk (RVA `0x5cea`) so it
/// unconditionally returns `value`.  This lets us drive the
/// populator's `(h * 10) / size` computation with a known `h` and
/// observe what happens downstream — including whether the
/// buffer-pool init succeeds and Receive trap shifts past `0x256a`.
///
/// The original function (10 bytes):
///
/// ```text
/// 0x5cea: 83 79 20 00     cmp [ecx+0x20], 0
/// 0x5cee: 74 04           jz +4
/// 0x5cf0: 8b 41 28        mov eax, [ecx+0x28]
/// 0x5cf3: c3              ret
/// 0x5cf4: 33 c0           xor eax, eax
/// 0x5cf6: c3              ret
/// ```
///
/// We overwrite the first 6 bytes with:
///
/// ```text
/// b8 XX XX XX XX  mov eax, imm32
/// c3              ret
/// ```
///
/// (Total 6 bytes.)  This is safely contained in the original
/// function's footprint — the displaced original tail (`mov eax,
/// [ecx+0x28]; ret`) is still callable starting at 0x5cf0 from
/// other paths, but those paths re-enter `0x5cea` not `0x5cf0` so
/// no other caller is affected.
fn patch_helper_addref(sb: &mut Sandbox, base: u32, value: u32) -> Result<(), String> {
    sb.msadds32_patch_helper_addref(base, value)
        .map_err(|e| format!("msadds32_patch_helper_addref: {e}"))
}

/// Drive the bootstrapped filter all the way to `IMemInputPin::Receive`
/// with an optional patch applied to `helper_addref` (RVA 0x5cea).
/// Returns the trap message + the trap EIP RVA.
struct DriveOutcome {
    hr: Option<u32>,
    trap_msg: Option<String>,
    trap_rva: Option<u32>,
    eax: u32,
    ecx: u32,
    edx: u32,
    esi: u32,
}

fn drive_receive_with_patch(
    sps: u32,
    n_channels: u16,
    n_avg: u32,
    n_block: u16,
    patch_value: Option<u32>,
) -> Option<DriveOutcome> {
    let (mut sb, img, filter) = bootstrap_filter()?;
    let base = img.image_base;
    if let Some(v) = patch_value {
        if let Err(e) = patch_helper_addref(&mut sb, base, v) {
            eprintln!("drive_receive_with_patch: patch failed: {e}");
            return None;
        }
    }

    let input_pin = enum_pin_by_direction(&mut sb, filter, PIN_DIRECTION_INPUT)?;
    let bp = AmtBlueprint::wma_criteria_passing(0x0161, n_channels, sps, n_avg, n_block);
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

    // Output-pin connection.
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

    let trap_eip = sb.cpu.trace_ring.iter().last().copied();
    let trap_rva = trap_eip.map(|e| e.wrapping_sub(base));
    let outcome = match r {
        Ok(hr) => DriveOutcome {
            hr: Some(hr),
            trap_msg: None,
            trap_rva: None,
            eax: sb.cpu.regs.get32(Reg32::Eax),
            ecx: sb.cpu.regs.get32(Reg32::Ecx),
            edx: sb.cpu.regs.get32(Reg32::Edx),
            esi: sb.cpu.regs.get32(Reg32::Esi),
        },
        Err(e) => DriveOutcome {
            hr: None,
            trap_msg: Some(format!("{e}")),
            trap_rva,
            eax: sb.cpu.regs.get32(Reg32::Eax),
            ecx: sb.cpu.regs.get32(Reg32::Ecx),
            edx: sb.cpu.regs.get32(Reg32::Edx),
            esi: sb.cpu.regs.get32(Reg32::Esi),
        },
    };
    Some(outcome)
}

/// Phase 4 — Receive with `helper_addref` patched to return a non-
/// zero value.  Sweep a panel of patch values; for each, report the
/// trap RVA + register file.  If any value drives Receive to a
/// different trap address (i.e. past `0x256a`) we've confirmed the
/// chain and have a working workaround for the missing
/// JoinFilterGraph initialisation.
#[test]
fn phase4_patch_helper_addref_panel() {
    if msadds32_path().is_none() {
        eprintln!("round63 phase4: msadds32.ax missing; skipping");
        return;
    }
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    if !fixture.is_file() {
        eprintln!("round63 phase4: WMA2 fixture missing; skipping");
        return;
    }

    // First: baseline (no patch) — must trap at 0x256a as before.
    eprintln!("round63 phase4: baseline (unpatched):");
    if let Some(o) = drive_receive_with_patch(44_100, 1, 4_000, 185, None) {
        eprintln!(
            "  hr={:?}  trap={:?}  trap_rva={:?}  eax={:#x} ecx={:#x} edx={:#x} esi={:#x}",
            o.hr, o.trap_msg, o.trap_rva, o.eax, o.ecx, o.edx, o.esi
        );
    }

    // Sweep patch values.
    //
    // The populator computes:
    //   edi_count = (helper_addref_result * 10) / helper_size_calc_result
    // For sps=44100 wbps=16 kind=2 → helper_size_calc = 65536.
    // For non-zero edi_count we need patch_value ≥ 6554.
    //
    // Crucially: patch_value=0 should reproduce the r62 baseline
    // trap (helper_addref always returning 0 is the natural state
    // when no JoinFilterGraph/Pause has set helper+0x3c).
    let values = [
        0u32,      // → matches baseline (uninitialised helper state)
        1,         // → edi_count = 0 but flag-bit changes nothing in helper_addref
        65536u32,  // → edi_count = 10
        100_000,   // → edi_count = 15
        655_360,   // → edi_count = 100
        4_000_000, // → edi_count = 610 (roughly half a second's allocations)
        44_100,    // → edi_count = 6 (sps as h)
        4_000,     // → edi_count = 0  (avg bytes / sec)
    ];
    for &v in &values {
        eprintln!("round63 phase4: patched helper_addref → {v}:");
        let predicted_size = predict_helper_size_calc(44_100, 16, 1, 2).unwrap_or(0);
        let predicted_edi = v
            .saturating_mul(10)
            .checked_div(predicted_size)
            .unwrap_or(0);
        eprintln!("  predicted size_calc={predicted_size}, edi_count={predicted_edi}");
        if let Some(o) = drive_receive_with_patch(44_100, 1, 4_000, 185, Some(v)) {
            eprintln!(
                "  hr={:?}  trap={:?}  trap_rva={:?}  eax={:#x} ecx={:#x} edx={:#x} esi={:#x}",
                o.hr, o.trap_msg, o.trap_rva, o.eax, o.ecx, o.edx, o.esi
            );
        } else {
            eprintln!("  drive returned None (bootstrap failed)");
        }
    }
}

/// Phase 5 — regression guard.  Round 62 left a baseline at trap
/// RVA `0x256a`.  Round 63 must not regress that to
/// `VFW_E_NOT_COMMITTED`.  This test is a no-op if the fixture is
/// missing.
#[test]
fn phase5_regression_guard() {
    if msadds32_path().is_none() {
        eprintln!("round63 phase5: msadds32.ax missing; skipping");
        return;
    }
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    if !fixture.is_file() {
        eprintln!("round63 phase5: WMA2 fixture missing; skipping");
        return;
    }
    let o = match drive_receive_with_patch(44_100, 1, 4_000, 185, None) {
        Some(o) => o,
        None => {
            eprintln!("round63 phase5: drive failed (bootstrap); skipping");
            return;
        }
    };
    if let Some(hr) = o.hr {
        assert_ne!(
            hr, 0x8004_0209,
            "round63 regression: Receive surfaced VFW_E_NOT_COMMITTED again"
        );
        eprintln!("round63 phase5: Receive returned HRESULT {hr:#010x}");
    }
    if let Some(msg) = o.trap_msg.as_deref() {
        eprintln!("round63 phase5: Receive trap = {msg}");
    }
}
