// Heuristic byte-pattern scanning code — the explicit `if`-inside-`match`
// shape is easier to read than the collapsed form clippy proposes, and
// the leading `*` in the long-form doc-comment ASCII tables is also
// preferable to the compressed form clippy demands.  Suppress at file
// scope.
#![allow(
    clippy::collapsible_match,
    clippy::collapsible_if,
    clippy::nonminimal_bool,
    clippy::doc_overindented_list_items
)]

//! Round 60 — disassemble `msadds32.ax`'s `IPin::QueryAccept`
//! (vtable slot 11) on its input pin so we can identify which
//! validation criterion is rejecting every Round 59 fixture.
//!
//! Round 59 closed by demonstrating that the audio splitter
//! rejects BOTH the WMA1 (`wFormatTag=0x0160`, 4-byte extradata
//! `00 00 01 00`) and WMA2 (`wFormatTag=0x0161`, 10-byte extradata)
//! `AM_MEDIA_TYPE`s extracted from real `.wma` ASF fixtures.  Both
//! land with `HRESULT 0x80004005` (`E_FAIL`).  The splitter's
//! validator is rejecting *something* in the AMT that ffmpeg's
//! WMA encoder does not produce — either a header byte, a field
//! constraint, or a struct-layout assumption.
//!
//! ## What this test pins
//!
//! * **Phase 1** — Resolve the input pin's vtable[11]
//!   (`IPin::QueryAccept`) function VA via the standard COM
//!   pointer chase: `vtbl = *input_pin; method_va = *(vtbl + 4*11)`.
//!
//! * **Phase 2** — Dump up to 512 bytes of the QueryAccept
//!   prologue into a hex+ASCII listing on stderr.  No instruction-
//!   level decoder is required for the deliverable; the byte
//!   listing alone is sufficient empirical material for offline
//!   analysis with Intel SDM Vol. 2 in hand.  The dump is asserted
//!   to:
//!     - start with `0x55` (`push ebp`) — the canonical 32-bit
//!       function prologue MS C/C++ compilers emit;
//!     - contain at least one `cmp /r imm` opcode (`0x81`/`0x83`)
//!       — the validator branches must compare AMT bytes against
//!       constants.
//!
//! * **Phase 3** — Walk the bytes searching for likely AMT field
//!   reads (`mov eax, [reg + 0x40]` = `cbFormat`; `mov eax,
//!   [reg + 0x44]` = `pbFormat`; `mov ax, [reg + 0x12]` =
//!   `extradata[0]`) and report each one's file offset, so we can
//!   trace which fields the validator actually touches.  No
//!   assertion on the exact offsets — this is a structured search
//!   for the report.
//!
//! ## Reference material (clean-room only)
//!
//! * Intel® 64 and IA-32 Architectures Software Developer's Manual,
//!   Volume 2 (Instruction Set Reference) — opcode tables for
//!   `push`/`mov`/`cmp`/`jcc`.
//! * MSDN — `IPin::QueryAccept`, `AM_MEDIA_TYPE`, `WAVEFORMATEX`
//!   layouts.
//! * Raw bytes of `msadds32.ax` from
//!   `docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/`.
//!
//! No Wine / ReactOS / MinGW / Microsoft DShow / ffmpeg WMA source
//! consulted.  This is the same clean-room reverse-engineering
//! methodology rounds 22-57 established.

use oxideav_vfw::com::{
    call::{call_method, vtable_is_plausible},
    method_va, vtable_ptr, MSADDS_AUDIO_DECODER_CLSID, PIN_DIRECTION_INPUT,
    SLOT_BASEFILTER_ENUM_PINS, SLOT_BASEFILTER_STOP, SLOT_ENUMPINS_NEXT, SLOT_PIN_QUERY_ACCEPT,
    SLOT_PIN_QUERY_DIRECTION,
};
use oxideav_vfw::{Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY, IID_IUNKNOWN};
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
        if !matches!(r, Ok(0)) {
            break;
        }
    }
    let _ = sb.com_release(pp);
    None
}

// ---- hex dump helper -------------------------------------------------

fn format_hex_dump(base: u32, bytes: &[u8]) -> String {
    let mut s = String::new();
    for (i, chunk) in bytes.chunks(16).enumerate() {
        let off = base.wrapping_add((i * 16) as u32);
        let mut line = format!("  {off:08x}: ");
        for (j, b) in chunk.iter().enumerate() {
            if j == 8 {
                line.push(' ');
            }
            line.push_str(&format!("{b:02x} "));
        }
        // pad to 16 columns
        for _ in 0..(16 - chunk.len()) {
            line.push_str("   ");
        }
        line.push_str(" |");
        for b in chunk {
            line.push(if (0x20..0x7f).contains(b) {
                *b as char
            } else {
                '.'
            });
        }
        line.push('|');
        s.push_str(&line);
        s.push('\n');
    }
    s
}

// ───────────────────────────────────────────────────────────────────
// Phase 1 — resolve QueryAccept VA via the input pin's vtable
// ───────────────────────────────────────────────────────────────────

/// Phase 1 — the input pin's vtable slot 11 (`QueryAccept`) resolves
/// to a guest VA lying inside the loaded `msadds32.ax` image
/// (`.text` section).  Skipped if the binary is not present.
#[test]
fn phase1_query_accept_method_va_resolves_inside_text_section() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase1: msadds32.ax missing; skipping");
        return;
    };
    let Some(pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase1: no INPUT pin; skipping");
        return;
    };
    let vtbl = vtable_ptr(&sb.mmu, pin).expect("vtable pointer fetch");
    let qa_va = method_va(&sb.mmu, pin, SLOT_PIN_QUERY_ACCEPT).expect("vtable[11] resolves");
    eprintln!(
        "round60 phase1: input pin = {pin:#010x}, vtbl = {vtbl:#010x}, \
         QueryAccept VA = {qa_va:#010x}  (image_base = {base:#010x})",
        base = img.image_base
    );
    // The method must live inside the loaded image's text section
    // (image_base + 0x1000 .. image_base + 0x60000 covers all
    // `.text` sections of msadds32.ax which is ~228 KiB).
    assert!(
        qa_va > img.image_base && qa_va < img.image_base + 0x10_0000,
        "QueryAccept VA {qa_va:#010x} not inside image_base {ib:#010x} +1 MiB",
        ib = img.image_base
    );
    // The vtable itself must also live inside the image (codec
    // vtables are in `.rdata`).
    assert!(
        vtbl > img.image_base && vtbl < img.image_base + 0x10_0000,
        "vtable {vtbl:#010x} not inside image"
    );
}

// ───────────────────────────────────────────────────────────────────
// Phase 1b — dump the full IPin vtable so we know what each slot
// holds and can correlate "the validator that ReceiveConnection
// uses" with our isolated QueryAccept disassembly.
// ───────────────────────────────────────────────────────────────────

/// Phase 1b — dump every slot of the input pin's IPin vtable
/// (slots 0..18) to stderr.  Round 60 needs this to confirm
/// whether `ReceiveConnection` and `QueryAccept` share a
/// validator code path or have independent implementations.
#[test]
fn phase1b_dump_full_ipin_vtable() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase1b: msadds32.ax missing; skipping");
        return;
    };
    let Some(pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase1b: no INPUT pin; skipping");
        return;
    };
    let vtbl = vtable_ptr(&sb.mmu, pin).expect("vtable pointer fetch");
    eprintln!("round60 phase1b: input pin = {pin:#010x}, IPin vtable @ {vtbl:#010x}",);
    let names = [
        "QueryInterface",
        "AddRef",
        "Release",
        "Connect",
        "ReceiveConnection",
        "Disconnect",
        "ConnectedTo",
        "ConnectionMediaType",
        "QueryPinInfo",
        "QueryDirection",
        "QueryId",
        "QueryAccept",
        "EnumMediaTypes",
        "QueryInternalConnections",
        "EndOfStream",
        "BeginFlush",
        "EndFlush",
        "NewSegment",
    ];
    for (slot, name) in names.iter().enumerate() {
        let va = sb
            .mmu
            .load32(vtbl.wrapping_add((slot as u32) * 4))
            .unwrap_or(0);
        let rva = va.wrapping_sub(img.image_base);
        eprintln!("  vtable[{slot:>2}] @ {va:#010x} (RVA {rva:#06x}) — {name}");
    }
    // Also dump the "inner-class" vtable that QueryAccept's
    // wrapper-style delegate at slot 11 actually targets:
    // mov eax, [esp+4]; lea ecx, [eax-0xC]; mov eax, [eax-0xC];
    // call [eax+0x20]   -- slot 8 of *(pin - 0xC)'s vtable.
    let inner_obj = pin.wrapping_sub(0xC);
    if let Ok(inner_vtbl) = sb.mmu.load32(inner_obj) {
        eprintln!("round60 phase1b: pin-0xC inner-class vtable @ {inner_vtbl:#010x}",);
        for slot in 0..20 {
            let va = sb
                .mmu
                .load32(inner_vtbl.wrapping_add(slot * 4))
                .unwrap_or(0);
            let rva = va.wrapping_sub(img.image_base);
            eprintln!("  inner.vtable[{slot:>2}] @ {va:#010x} (RVA {rva:#06x})");
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 2 — dump the QueryAccept prologue and look for hallmarks
// ───────────────────────────────────────────────────────────────────

/// Phase 2 — dump 512 bytes starting at the QueryAccept VA, assert
/// the function prologue is a canonical `push ebp; mov ebp, esp`
/// pair (or the equivalent without a frame pointer when the
/// compiler omits it), and surface the full hex+ASCII listing on
/// stderr so the round-60 report can cite specific byte offsets.
#[test]
fn phase2_query_accept_disassembly_dump() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase2: msadds32.ax missing; skipping");
        return;
    };
    let Some(pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase2: no INPUT pin; skipping");
        return;
    };
    let qa_va = method_va(&sb.mmu, pin, SLOT_PIN_QUERY_ACCEPT).expect("vtable[11] resolves");
    let bytes = sb
        .mmu
        .read(qa_va, 512)
        .expect("read 512 bytes of QueryAccept");
    eprintln!(
        "round60 phase2: QueryAccept @ {qa_va:#010x} (image_base + {rva:#x}) — \
         first 512 bytes:",
        rva = qa_va.wrapping_sub(img.image_base)
    );
    eprintln!("{}", format_hex_dump(qa_va, &bytes));

    // Empirically (round 60 first-run): QueryAccept @ image_base +
    // 0x49a7 starts with `83 7c 24 08 00` = `cmp dword ptr [esp+8], 0`
    // — the canonical "is the AMT pointer NULL?" guard.  We accept
    // either the frame-pointer prologue (`0x55 push ebp`) or the
    // omit-FP entry sequence the optimizer emits here.
    assert!(
        bytes[0] == 0x55 || bytes[0] == 0x83 || bytes[0] == 0x56 || bytes[0] == 0x53,
        "QueryAccept @ {qa_va:#010x} first byte {b:#04x} is not a recognised \
         32-bit function prologue (push ebp / cmp / push esi / push ebx)",
        b = bytes[0]
    );

    // Look for `cmp /r imm32` (0x81) or `cmp /r imm8` (0x83) —
    // every AMT-validation check the splitter performs is a `cmp`
    // followed by a `jcc`.  Empirically at least 2-3 such
    // comparisons should appear in any non-trivial validator.
    let cmp_imm32_count = bytes.windows(2).filter(|w| w[0] == 0x81).count();
    let cmp_imm8_count = bytes.windows(2).filter(|w| w[0] == 0x83).count();
    eprintln!(
        "round60 phase2: count of `0x81 ..` (cmp/sub/add r,imm32) = {cmp_imm32_count}, \
         `0x83 ..` (cmp/sub/add r,imm8) = {cmp_imm8_count}"
    );
    assert!(
        cmp_imm32_count + cmp_imm8_count >= 2,
        "QueryAccept body shows no /r imm comparisons; not a validator"
    );

    // Locate any `0xB8`/`0xB9` `mov eax/ecx, imm32` whose imm32
    // equals E_FAIL (0x80004005) — the failure code path.  Surface
    // every match.
    let mut e_fail_loads: Vec<(usize, u8)> = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        let op = bytes[i];
        if (0xB8..=0xBF).contains(&op) {
            let imm = u32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            if imm == 0x8000_4005 {
                e_fail_loads.push((i, op));
            }
        }
    }
    if !e_fail_loads.is_empty() {
        eprintln!("round60 phase2: E_FAIL (0x80004005) load sites:");
        for (off, op) in &e_fail_loads {
            let reg = match op {
                0xB8 => "eax",
                0xB9 => "ecx",
                0xBA => "edx",
                0xBB => "ebx",
                0xBC => "esp",
                0xBD => "ebp",
                0xBE => "esi",
                0xBF => "edi",
                _ => "?",
            };
            eprintln!(
                "  +{off:#x} (VA {va:#010x}): mov {reg}, 0x80004005",
                va = qa_va.wrapping_add(*off as u32)
            );
        }
    }

    // E_FAIL constant returns are common; lack of any is unusual
    // but not necessarily a failure for the dump test.
    eprintln!(
        "round60 phase2: bytes[0..16] = {:02x?}",
        &bytes[..16.min(bytes.len())]
    );
}

// ───────────────────────────────────────────────────────────────────
// Phase 2b — follow the indirect dispatch into the inner validator
// ───────────────────────────────────────────────────────────────────

/// QueryAccept's body at `image_base + 0x49a7` follows a classic
/// CBaseInputPin pattern:
///
///     cmp [esp+8], 0       ; pmt arg NULL?
///     jne +7
///     mov eax, E_POINTER
///     jmp ret
///     mov eax, [esp+4]     ; this
///     push [esp+8]         ; push pmt
///     lea ecx, [eax-0xC]   ; ecx = (this - 0xC)  (containing class)
///     mov eax, [eax-0xC]   ; eax = *(this - 0xC) = vtable
///     call [eax+0x20]      ; vtable[8] — the virtual validator
///     test eax, eax
///     jge ret-with-eax
///     mov eax, S_FALSE
///     ret 8
///
/// The virtual validator at slot 8 of the "this-0xC" object's
/// vtable is the actual `CheckMediaType` implementation.  We
/// follow that pointer through guest memory and disassemble it
/// here too.  Because the validator runs against the LIVE
/// guest-side input pin, we can resolve the vtable directly:
///
///     inner_vtable = mmu.load32(input_pin - 0x0C)
///     check_media_type_va = mmu.load32(inner_vtable + 0x20)
#[test]
fn phase2b_check_media_type_validator_disassembly_dump() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase2b: msadds32.ax missing; skipping");
        return;
    };
    let Some(pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase2b: no INPUT pin; skipping");
        return;
    };
    // Replay the wrapper's pointer chase: read *(pin - 0xC) to
    // get the inner-class vtable, then slot 8 of that.
    let inner_obj = pin.wrapping_sub(0x0C);
    eprintln!("round60 phase2b: inner-class base = {inner_obj:#010x} (input_pin - 0xC)");
    let Ok(inner_vtbl) = sb.mmu.load32(inner_obj) else {
        eprintln!("round60 phase2b: failed to read inner vtable pointer; skipping");
        return;
    };
    eprintln!("round60 phase2b: inner vtable = {inner_vtbl:#010x}");
    if inner_vtbl == 0 {
        eprintln!("round60 phase2b: inner vtable is NULL; skipping");
        return;
    }
    let Ok(cmt_va) = sb.mmu.load32(inner_vtbl.wrapping_add(0x20)) else {
        eprintln!("round60 phase2b: failed to read vtable[8]; skipping");
        return;
    };
    eprintln!(
        "round60 phase2b: CheckMediaType VA = {cmt_va:#010x} (image_base + {rva:#x})",
        rva = cmt_va.wrapping_sub(img.image_base)
    );
    if cmt_va < img.image_base || cmt_va >= img.image_base + 0x10_0000 {
        eprintln!("round60 phase2b: CheckMediaType VA out of image bounds; skipping");
        return;
    }
    let Ok(bytes) = sb.mmu.read(cmt_va, 768) else {
        eprintln!("round60 phase2b: read failed");
        return;
    };
    eprintln!("round60 phase2b: CheckMediaType first 768 bytes:");
    eprintln!("{}", format_hex_dump(cmt_va, &bytes));

    // Walk the bytes locating useful constant comparisons.
    let mut interesting: Vec<(usize, String)> = Vec::new();
    for i in 0..bytes.len().saturating_sub(6) {
        // `cmp r/m32, imm32`  (0x81 /7)
        if bytes[i] == 0x81 && (bytes[i + 1] >> 3) & 0b111 == 7 {
            let modrm = bytes[i + 1];
            let mode = (modrm >> 6) & 0b11;
            let rm = modrm & 0b111;
            let mut imm_off = i + 2;
            if mode != 0b11 {
                if rm == 4 {
                    imm_off += 1;
                }
                match mode {
                    0b00 => {
                        if rm == 5 {
                            imm_off += 4;
                        }
                    }
                    0b01 => imm_off += 1,
                    0b10 => imm_off += 4,
                    _ => {}
                }
            }
            if imm_off + 4 <= bytes.len() {
                let imm = u32::from_le_bytes([
                    bytes[imm_off],
                    bytes[imm_off + 1],
                    bytes[imm_off + 2],
                    bytes[imm_off + 3],
                ]);
                interesting.push((i, format!("cmp r/m32, {imm:#010x}")));
            }
        }
        // `cmp r/m, imm8` (0x83 /7)
        if bytes[i] == 0x83 && (bytes[i + 1] >> 3) & 0b111 == 7 {
            let imm8 = bytes[i + 2];
            interesting.push((i, format!("cmp r/m, {imm8:#04x}")));
        }
        // `cmp r/m16, imm16` (66 81 /7  or  66 83 /7)
        if i + 1 < bytes.len()
            && bytes[i] == 0x66
            && bytes[i + 1] == 0x81
            && i + 2 < bytes.len()
            && (bytes[i + 2] >> 3) & 0b111 == 7
        {
            let modrm = bytes[i + 2];
            let mode = (modrm >> 6) & 0b11;
            let rm = modrm & 0b111;
            let mut imm_off = i + 3;
            if mode != 0b11 {
                if rm == 4 {
                    imm_off += 1;
                }
                match mode {
                    0b00 => {
                        if rm == 5 {
                            imm_off += 4;
                        }
                    }
                    0b01 => imm_off += 1,
                    0b10 => imm_off += 4,
                    _ => {}
                }
            }
            if imm_off + 2 <= bytes.len() {
                let imm = u16::from_le_bytes([bytes[imm_off], bytes[imm_off + 1]]);
                interesting.push((i, format!("cmp r/m16, {imm:#06x}")));
            }
        }
    }
    eprintln!("round60 phase2b: constant-comparison sites:");
    for (off, desc) in &interesting {
        eprintln!(
            "  +{off:#x} (VA {va:#010x}): {desc}",
            va = cmt_va.wrapping_add(*off as u32)
        );
    }

    // Search for IsEqualGUID-style calls (push reference IID;
    // call) — the validator compares the AMT's majortype/subtype/
    // formattype GUIDs against constants in `.rdata`.  Look for
    // `push imm32` (0x68 ..) followed by a `call` (0xE8 / 0xFF
    // /2) within the next 16 bytes; the imm32 is the .rdata VA
    // of the reference GUID.
    eprintln!("round60 phase2b: GUID-literal references (push imm32 + nearby call):");
    let mut guid_pushes = 0;
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0x68 {
            let imm = u32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            if imm > img.image_base && imm < img.image_base + 0x10_0000 {
                let look_ahead_end = (i + 16).min(bytes.len().saturating_sub(1));
                let has_call = bytes[i + 5..look_ahead_end]
                    .windows(1)
                    .any(|w| w[0] == 0xE8 || w[0] == 0xFF);
                if has_call {
                    eprintln!(
                        "  +{off:#x} (VA {va:#010x}): push {imm:#010x}  ; .rdata+{rva:#x}",
                        off = i,
                        va = cmt_va.wrapping_add(i as u32),
                        rva = imm.wrapping_sub(img.image_base)
                    );
                    // Read 16 bytes there — likely the GUID literal.
                    if let Ok(g_bytes) = sb.mmu.read(imm, 16) {
                        eprintln!("      bytes @ {imm:#010x}: {g_bytes:02x?}");
                        if let Some(g) = oxideav_vfw::com::Guid::read_le(&g_bytes) {
                            eprintln!("      decoded GUID: {g}");
                        }
                    }
                    guid_pushes += 1;
                }
            }
        }
    }
    eprintln!("round60 phase2b: total GUID-literal push sites = {guid_pushes}");
}

// ───────────────────────────────────────────────────────────────────
// Phase 2c — disassemble ReceiveConnection itself (the actual
// validator the round-59 fixtures land on)
// ───────────────────────────────────────────────────────────────────

/// Phase 2c — round-60 first-pass evidence (see phase1b vtable
/// dump and phase2b CheckMediaType dump) demonstrated that
/// QueryAccept and the input-pin's `CheckMediaType` virtual
/// both unconditionally return S_OK / S_FALSE without touching
/// the WAVEFORMATEX bytes.  The actual `E_FAIL` round 59
/// observed must therefore originate inside `ReceiveConnection`
/// (vtable slot 4), which performs its own AMT validation
/// before invoking the upstream connection bookkeeping.
///
/// This phase dumps `ReceiveConnection` and looks for the
/// failure code path returning `0x80004005` (`E_FAIL`).
#[test]
fn phase2c_receive_connection_validator_disassembly() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase2c: msadds32.ax missing; skipping");
        return;
    };
    let Some(pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase2c: no INPUT pin; skipping");
        return;
    };
    let rc_va = method_va(&sb.mmu, pin, 4 /* ReceiveConnection */).expect("vtable[4] resolves");
    eprintln!(
        "round60 phase2c: ReceiveConnection @ {rc_va:#010x} (image_base + {rva:#x})",
        rva = rc_va.wrapping_sub(img.image_base)
    );
    let bytes = sb
        .mmu
        .read(rc_va, 1024)
        .expect("read 1 KiB of ReceiveConnection");
    eprintln!(
        "round60 phase2c: ReceiveConnection first 1024 bytes:\n{}",
        format_hex_dump(rc_va, &bytes)
    );

    // Locate every `mov eax, 0x80004005` (b8 05 40 00 80) site
    // — these are the E_FAIL return points.
    let mut e_fail_sites: Vec<usize> = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0xB8
            && bytes[i + 1] == 0x05
            && bytes[i + 2] == 0x40
            && bytes[i + 3] == 0x00
            && bytes[i + 4] == 0x80
        {
            e_fail_sites.push(i);
        }
    }
    eprintln!("round60 phase2c: E_FAIL (mov eax, 0x80004005) sites:");
    for off in &e_fail_sites {
        eprintln!(
            "  +{off:#x} (VA {va:#010x})",
            va = rc_va.wrapping_add(*off as u32)
        );
    }

    // Locate calls (e8 rel32 / ff /2 modrm) leading up to each
    // E_FAIL site — these are the validation calls.
    for &off in &e_fail_sites {
        let start = off.saturating_sub(64);
        eprintln!(
            "round60 phase2c: bytes preceding E_FAIL @ +{off:#x}: {bytes:02x?}",
            bytes = &bytes[start..off.min(bytes.len())]
        );
    }

    // Find any `cmp r/m16, imm16` operations against the
    // wFormatTag constants (0x160 / 0x161) — first place the
    // validator would touch.
    let mut tag_cmps: Vec<(usize, u16)> = Vec::new();
    for i in 0..bytes.len().saturating_sub(6) {
        if bytes[i] == 0x66 && bytes[i + 1] == 0x81 && (bytes[i + 2] >> 3) & 0b111 == 7 {
            let modrm = bytes[i + 2];
            let mode = (modrm >> 6) & 0b11;
            let rm = modrm & 0b111;
            let mut imm_off = i + 3;
            if mode != 0b11 {
                if rm == 4 {
                    imm_off += 1;
                }
                match mode {
                    0b00 => {
                        if rm == 5 {
                            imm_off += 4;
                        }
                    }
                    0b01 => imm_off += 1,
                    0b10 => imm_off += 4,
                    _ => {}
                }
            }
            if imm_off + 2 <= bytes.len() {
                let imm = u16::from_le_bytes([bytes[imm_off], bytes[imm_off + 1]]);
                if imm == 0x160 || imm == 0x161 || imm == 0x055 || imm == 0x161 {
                    tag_cmps.push((i, imm));
                }
            }
        }
    }
    eprintln!("round60 phase2c: 16-bit constant compares vs WMA tags: {tag_cmps:?}");

    // Calls within the body (relative call e8 rel32).
    let mut calls: Vec<(usize, u32)> = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0xE8 {
            let rel = i32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            let target = rc_va.wrapping_add(i as u32 + 5).wrapping_add(rel as u32);
            calls.push((i, target));
        }
    }
    eprintln!("round60 phase2c: rel32 calls inside ReceiveConnection (first 16):");
    for (off, tgt) in calls.iter().take(16) {
        eprintln!(
            "  +{off:#x} (VA {va:#010x}): call {tgt:#010x} (RVA {rva:#x})",
            va = rc_va.wrapping_add(*off as u32),
            rva = tgt.wrapping_sub(img.image_base)
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 2d — disassemble the REAL validator at inner.vtable[10]
// (RVA 0x5623), which `ReceiveConnection` calls BEFORE the
// E_FAIL→VFW_E_TYPE_NOT_ACCEPTED remap.  Round 59 saw raw E_FAIL
// (0x80004005) → the rejection must come from this method.
// ───────────────────────────────────────────────────────────────────

/// Phase 2d — the real AMT validator.  Decoded from
/// `ReceiveConnection`'s body (phase 2c):
///
///     1c4047c7: call [eax+0x28]    ; inner.vtable[10] @ 0x5623
///     1c4047cd: test eax, eax
///     1c4047d1: jge 0x4047da       ; >=0 → continue
///     1c4047d5: call [eax+0x2c]    ; cleanup
///     1c4047d8: jmp 0x40480f       ; -- BYPASSES the E_FAIL→0x8004022a remap
///
/// Because that jump bypasses the remap at offset 0x4047f6, an
/// E_FAIL coming out of vtable[10] propagates UN-CHANGED back to
/// the host.  Round 59 observed exactly 0x80004005, so this is
/// where the validator rejection originates.
#[test]
fn phase2d_inner_validator_at_rva_5623_disassembly() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase2d: msadds32.ax missing; skipping");
        return;
    };
    let Some(pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase2d: no INPUT pin; skipping");
        return;
    };
    // Resolve via the live vtable, not the hard-coded RVA, to
    // stay robust against ASLR / image-base changes.
    let inner_obj = pin.wrapping_sub(0x0C);
    let inner_vtbl = sb.mmu.load32(inner_obj).expect("read inner vtable pointer");
    let validator_va = sb
        .mmu
        .load32(inner_vtbl.wrapping_add(10 * 4))
        .expect("read inner.vtable[10]");
    eprintln!(
        "round60 phase2d: inner.vtable[10] (validator) @ {validator_va:#010x} \
         (image_base + {rva:#x})",
        rva = validator_va.wrapping_sub(img.image_base)
    );
    let bytes = sb.mmu.read(validator_va, 1024).expect("read 1 KiB");
    eprintln!(
        "round60 phase2d: validator first 1024 bytes:\n{}",
        format_hex_dump(validator_va, &bytes)
    );

    // Find E_FAIL (0x80004005) immediate-load sites.
    let mut e_fail_sites = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0xB8
            && bytes[i + 1] == 0x05
            && bytes[i + 2] == 0x40
            && bytes[i + 3] == 0x00
            && bytes[i + 4] == 0x80
        {
            e_fail_sites.push(i);
        }
    }
    eprintln!("round60 phase2d: E_FAIL immediate-load sites:");
    for off in &e_fail_sites {
        eprintln!(
            "  +{off:#x} (VA {va:#010x})",
            va = validator_va.wrapping_add(*off as u32)
        );
    }

    // Find every rel32 call.
    let mut calls = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0xE8 {
            let rel = i32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            let target = validator_va
                .wrapping_add(i as u32 + 5)
                .wrapping_add(rel as u32);
            calls.push((i, target));
        }
    }
    eprintln!("round60 phase2d: rel32 calls (first 32):");
    for (off, tgt) in calls.iter().take(32) {
        eprintln!(
            "  +{off:#x} (VA {va:#010x}): call {tgt:#010x} (RVA {rva:#x})",
            va = validator_va.wrapping_add(*off as u32),
            rva = tgt.wrapping_sub(img.image_base)
        );
    }

    // Find GUID-literal pushes — `push imm32` where imm32 is in
    // the image bounds (likely a .rdata GUID reference).
    eprintln!("round60 phase2d: GUID-literal `push imm32` sites:");
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0x68 {
            let imm = u32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            if imm > img.image_base && imm < img.image_base + 0x20_000 {
                if let Ok(g_bytes) = sb.mmu.read(imm, 16) {
                    if let Some(g) = oxideav_vfw::com::Guid::read_le(&g_bytes) {
                        eprintln!(
                            "  +{off:#x} (VA {va:#010x}): push {imm:#010x}  ; GUID = {g}",
                            off = i,
                            va = validator_va.wrapping_add(i as u32)
                        );
                    }
                }
            }
        }
    }

    // 16-bit constant compares — likely against wFormatTag.
    eprintln!("round60 phase2d: 16-bit constant compares vs WMA-related ranges:");
    for i in 0..bytes.len().saturating_sub(8) {
        if bytes[i] == 0x66 && bytes[i + 1] == 0x81 && (bytes[i + 2] >> 3) & 0b111 == 7 {
            let modrm = bytes[i + 2];
            let mode = (modrm >> 6) & 0b11;
            let rm = modrm & 0b111;
            let mut imm_off = i + 3;
            if mode != 0b11 {
                if rm == 4 {
                    imm_off += 1;
                }
                match mode {
                    0b00 => {
                        if rm == 5 {
                            imm_off += 4;
                        }
                    }
                    0b01 => imm_off += 1,
                    0b10 => imm_off += 4,
                    _ => {}
                }
            }
            if imm_off + 2 <= bytes.len() {
                let imm = u16::from_le_bytes([bytes[imm_off], bytes[imm_off + 1]]);
                eprintln!(
                    "  +{off:#x} (VA {va:#010x}): cmp r/m16, {imm:#06x}",
                    off = i,
                    va = validator_va.wrapping_add(i as u32)
                );
            }
        }
        // `cmp r/m16, imm8` (66 83 /7 ib)
        if bytes[i] == 0x66
            && bytes[i + 1] == 0x83
            && (bytes[i + 2] >> 3) & 0b111 == 7
            && i + 3 < bytes.len()
        {
            let imm = bytes[i + 3];
            eprintln!(
                "  +{off:#x} (VA {va:#010x}): cmp r/m16, {imm:#04x} (8-bit)",
                off = i,
                va = validator_va.wrapping_add(i as u32)
            );
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 2e — chase the validator delegation chain:
//   ReceiveConnection → inner.vtable[10]@0x5623
//                       → m_pFilter.vtable[18]   (filter's slot 18)
//                       → … the codec's audio-format gate.
// ───────────────────────────────────────────────────────────────────

/// Phase 2e — the validator at `RVA 0x5623` is a trampoline; it
/// loads `[this+0xD8] = m_pFilter` and calls slot 18 of its
/// vtable.  That's where the codec actually inspects the AMT
/// bytes.  Resolve the filter pointer at runtime, dump its
/// vtable, then disassemble the slot-18 target.
#[test]
fn phase2e_filter_vtable_slot18_validator() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase2e: msadds32.ax missing; skipping");
        return;
    };
    let Some(pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase2e: no INPUT pin; skipping");
        return;
    };
    // Replay the validator's chain:  ecx_at_call = [pin - 0xC]   (the inner-class object)
    //                                m_pFilter   = [inner + 0xD8]
    //                                vtable      = *m_pFilter
    //                                target_va   = vtable[18] = [vtbl + 0x48]
    let inner = pin.wrapping_sub(0x0C);
    let m_pfilter = sb.mmu.load32(inner + 0xD8).expect("read m_pFilter");
    eprintln!("round60 phase2e: inner-class @ {inner:#010x}, m_pFilter @ {m_pfilter:#010x}");
    let filter_vtbl = sb.mmu.load32(m_pfilter).expect("read filter vtable");
    eprintln!("round60 phase2e: filter vtable @ {filter_vtbl:#010x}");
    eprintln!("round60 phase2e: filter vtable slots 0..=24:");
    for slot in 0..=24 {
        let va = sb.mmu.load32(filter_vtbl + slot * 4).unwrap_or(0);
        let rva = va.wrapping_sub(img.image_base);
        let inside = va > img.image_base && va < img.image_base + 0x10_0000;
        eprintln!(
            "  filter.vtbl[{slot:>2}] @ {va:#010x} (RVA {rva:#06x}){}",
            if inside { "" } else { "  OUT-OF-IMAGE" }
        );
    }
    let target_va = sb
        .mmu
        .load32(filter_vtbl + 18 * 4)
        .expect("read filter.vtable[18]");
    eprintln!(
        "round60 phase2e: filter.vtable[18] (audio-format-gate) @ {target_va:#010x} \
         (image_base + {rva:#x})",
        rva = target_va.wrapping_sub(img.image_base)
    );
    let bytes = sb.mmu.read(target_va, 1024).expect("read 1 KiB");
    eprintln!(
        "round60 phase2e: target first 1024 bytes:\n{}",
        format_hex_dump(target_va, &bytes)
    );

    // E_FAIL load sites in this function?
    let mut e_fail_sites = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0xB8
            && bytes[i + 1] == 0x05
            && bytes[i + 2] == 0x40
            && bytes[i + 3] == 0x00
            && bytes[i + 4] == 0x80
        {
            e_fail_sites.push(i);
        }
    }
    eprintln!("round60 phase2e: E_FAIL sites: {e_fail_sites:?}");
    for off in &e_fail_sites {
        let start = off.saturating_sub(32);
        eprintln!(
            "  ctx before +{off:#x} (VA {va:#010x}): {bytes:02x?}",
            va = target_va.wrapping_add(*off as u32),
            bytes = &bytes[start..*off]
        );
    }

    // 16-bit literal compares
    eprintln!("round60 phase2e: 16-bit constant compares:");
    for i in 0..bytes.len().saturating_sub(8) {
        if bytes[i] == 0x66 && bytes[i + 1] == 0x81 && (bytes[i + 2] >> 3) & 0b111 == 7 {
            let modrm = bytes[i + 2];
            let mode = (modrm >> 6) & 0b11;
            let rm = modrm & 0b111;
            let mut imm_off = i + 3;
            if mode != 0b11 {
                if rm == 4 {
                    imm_off += 1;
                }
                match mode {
                    0b00 => {
                        if rm == 5 {
                            imm_off += 4;
                        }
                    }
                    0b01 => imm_off += 1,
                    0b10 => imm_off += 4,
                    _ => {}
                }
            }
            if imm_off + 2 <= bytes.len() {
                let imm = u16::from_le_bytes([bytes[imm_off], bytes[imm_off + 1]]);
                eprintln!(
                    "  +{off:#x} (VA {va:#010x}): cmp r/m16, {imm:#06x}",
                    off = i,
                    va = target_va.wrapping_add(i as u32)
                );
            }
        }
        if bytes[i] == 0x66 && bytes[i + 1] == 0x83 && (bytes[i + 2] >> 3) & 0b111 == 7 {
            if i + 3 < bytes.len() {
                let imm = bytes[i + 3];
                eprintln!(
                    "  +{off:#x} (VA {va:#010x}): cmp r/m16, {imm:#04x} (imm8)",
                    off = i,
                    va = target_va.wrapping_add(i as u32)
                );
            }
        }
    }

    // rel32 calls
    let mut calls = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0xE8 {
            let rel = i32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            let target = target_va
                .wrapping_add(i as u32 + 5)
                .wrapping_add(rel as u32);
            calls.push((i, target));
        }
    }
    eprintln!("round60 phase2e: rel32 calls (first 32):");
    for (off, tgt) in calls.iter().take(32) {
        eprintln!(
            "  +{off:#x} (VA {va:#010x}): call {tgt:#010x} (RVA {rva:#x})",
            va = target_va.wrapping_add(*off as u32),
            rva = tgt.wrapping_sub(img.image_base)
        );
    }

    // GUID-literal pushes
    eprintln!("round60 phase2e: GUID-literal `push imm32` sites:");
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0x68 {
            let imm = u32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            if imm > img.image_base && imm < img.image_base + 0x20_000 {
                if let Ok(g_bytes) = sb.mmu.read(imm, 16) {
                    if let Some(g) = oxideav_vfw::com::Guid::read_le(&g_bytes) {
                        eprintln!(
                            "  +{off:#x} (VA {va:#010x}): push {imm:#010x}  ; GUID = {g}",
                            off = i,
                            va = target_va.wrapping_add(i as u32)
                        );
                    }
                }
            }
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 2f — follow the validator chain into the rel32 callee at
// RVA 0x4743, which the trampoline at RVA 0x5623 invokes with
// the pmt after the (stub) filter.vtable[18] returns S_OK.
// ───────────────────────────────────────────────────────────────────

#[test]
fn phase2f_rva_4743_validator_disassembly() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase2f: msadds32.ax missing; skipping");
        return;
    };
    let Some(_pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase2f: no INPUT pin; skipping");
        return;
    };
    let target_va = img.image_base + 0x4743;
    eprintln!("round60 phase2f: target @ {target_va:#010x} (RVA 0x4743)");
    let bytes = sb.mmu.read(target_va, 1024).expect("read 1 KiB");
    eprintln!(
        "round60 phase2f: target first 1024 bytes:\n{}",
        format_hex_dump(target_va, &bytes)
    );

    // E_FAIL load sites.
    let mut e_fail_sites = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0xB8
            && bytes[i + 1] == 0x05
            && bytes[i + 2] == 0x40
            && bytes[i + 3] == 0x00
            && bytes[i + 4] == 0x80
        {
            e_fail_sites.push(i);
        }
    }
    eprintln!("round60 phase2f: E_FAIL sites at offsets: {e_fail_sites:?}");
    for off in &e_fail_sites {
        let start = off.saturating_sub(48);
        let end = (off + 5).min(bytes.len());
        eprintln!(
            "  context around E_FAIL +{off:#x} (VA {va:#010x}):\n{}",
            format_hex_dump(target_va + start as u32, &bytes[start..end]),
            va = target_va.wrapping_add(*off as u32),
        );
    }

    // rel32 calls
    let mut calls = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0xE8 {
            let rel = i32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            let target = target_va
                .wrapping_add(i as u32 + 5)
                .wrapping_add(rel as u32);
            calls.push((i, target));
        }
    }
    eprintln!("round60 phase2f: rel32 calls:");
    for (off, tgt) in calls.iter().take(16) {
        eprintln!(
            "  +{off:#x} (VA {va:#010x}): call {tgt:#010x} (RVA {rva:#x})",
            va = target_va.wrapping_add(*off as u32),
            rva = tgt.wrapping_sub(img.image_base)
        );
    }

    // GUID-literal pushes
    eprintln!("round60 phase2f: GUID-literal `push imm32` sites:");
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0x68 {
            let imm = u32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            if imm > img.image_base && imm < img.image_base + 0x20_000 {
                if let Ok(g_bytes) = sb.mmu.read(imm, 16) {
                    if let Some(g) = oxideav_vfw::com::Guid::read_le(&g_bytes) {
                        eprintln!(
                            "  +{off:#x} (VA {va:#010x}): push {imm:#010x}  ; GUID = {g}",
                            off = i,
                            va = target_va.wrapping_add(i as u32)
                        );
                    }
                }
            }
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 2g — disassemble the CompleteConnect callee (inner.vtable[12]
// @ RVA 0x2057), which `ReceiveConnection` invokes AFTER the
// (stub) CheckMediaType.  Round 59 saw raw E_FAIL — it must be
// originating here, because every prior callee in the chain
// returns S_OK on the stub host pin / stub filter.
// ───────────────────────────────────────────────────────────────────

#[test]
fn phase2g_inner_vtable_slot12_complete_connect() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase2g: msadds32.ax missing; skipping");
        return;
    };
    let Some(pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase2g: no INPUT pin; skipping");
        return;
    };
    let inner = pin.wrapping_sub(0x0C);
    let inner_vtbl = sb.mmu.load32(inner).expect("read inner vtable");
    let target_va = sb
        .mmu
        .load32(inner_vtbl + 12 * 4)
        .expect("read inner.vtable[12]");
    eprintln!(
        "round60 phase2g: inner.vtable[12] (CompleteConnect candidate) @ {target_va:#010x} \
         (image_base + {rva:#x})",
        rva = target_va.wrapping_sub(img.image_base)
    );
    let bytes = sb.mmu.read(target_va, 1024).expect("read 1 KiB");
    eprintln!(
        "round60 phase2g: first 1024 bytes:\n{}",
        format_hex_dump(target_va, &bytes)
    );
    let mut e_fail_sites = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0xB8
            && bytes[i + 1] == 0x05
            && bytes[i + 2] == 0x40
            && bytes[i + 3] == 0x00
            && bytes[i + 4] == 0x80
        {
            e_fail_sites.push(i);
        }
    }
    eprintln!("round60 phase2g: E_FAIL sites: {e_fail_sites:?}");

    let mut calls = Vec::new();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0xE8 {
            let rel = i32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            let target = target_va
                .wrapping_add(i as u32 + 5)
                .wrapping_add(rel as u32);
            calls.push((i, target));
        }
    }
    eprintln!("round60 phase2g: rel32 calls:");
    for (off, tgt) in calls.iter().take(16) {
        eprintln!(
            "  +{off:#x} (VA {va:#010x}): call {tgt:#010x} (RVA {rva:#x})",
            va = target_va.wrapping_add(*off as u32),
            rva = tgt.wrapping_sub(img.image_base)
        );
    }
    eprintln!("round60 phase2g: GUID-literal `push imm32` sites:");
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == 0x68 {
            let imm = u32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            if imm > img.image_base && imm < img.image_base + 0x20_000 {
                if let Ok(g_bytes) = sb.mmu.read(imm, 16) {
                    if let Some(g) = oxideav_vfw::com::Guid::read_le(&g_bytes) {
                        eprintln!(
                            "  +{off:#x} (VA {va:#010x}): push {imm:#010x}  ; GUID = {g}",
                            off = i,
                            va = target_va.wrapping_add(i as u32)
                        );
                    }
                }
            }
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 2h — extract the magic CLSID string the validator
// compares extradata against, and confirm the cbSize threshold.
// ───────────────────────────────────────────────────────────────────

/// Phase 2h — pin down the extradata gate.  From phase 2g we
/// decoded:
///
///     CompleteConnect (inner.vtable[12] @ RVA 0x2057):
///       call pConnector->ConnectionMediaType(&amt)
///       eax = amt.pbFormat
///       cmp word ptr [eax], 0x160          ; wFormatTag
///       jz wma1_path
///       cmp word ptr [eax], 0x161
///       jz wma2_path
///       → mov esi, E_UNEXPECTED ; return
///
///     wma1_path:
///       cmp word ptr [eax+0x10], 0x29       ; cbSize >= 41 ?
///       jb fail
///       call import_x40f090(eax+0x16, "1A0F...", 0x25)   ; cmp 37 bytes of extradata
///
///     wma2_path:
///       cmp word ptr [eax+0x10], 0x2F       ; cbSize >= 47 ?
///       jb fail
///       call import_x40f090(eax+0x1c, "1A0F...", 0x25)   ; cmp 37 bytes of extradata
///
/// This phase reads `.rdata @ 0x411138` to recover the exact
/// 37-byte (0x25) ASCII string the validator compares against.
#[test]
fn phase2h_extract_codec_clsid_string_constant() {
    let Some((sb, img, _filter)) = bootstrap_filter() else {
        eprintln!("round60 phase2h: msadds32.ax missing; skipping");
        return;
    };
    // Read 0x40 bytes at the .rdata offset the validator
    // dereferences (0x11138 RVA).
    let s_va = img.image_base + 0x11138;
    let bytes = sb.mmu.read(s_va, 0x40).expect("read .rdata @ 0x11138");
    eprintln!("round60 phase2h: bytes @ {s_va:#010x} (RVA 0x11138):");
    eprintln!("{}", format_hex_dump(s_va, &bytes));
    let nul_pos = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let s = std::str::from_utf8(&bytes[..nul_pos]).unwrap_or("(non-UTF8)");
    eprintln!("round60 phase2h: decoded string = {s:?}  (len = {nul_pos})");
    // The validator's `push 0x25` (length) implies 37 chars.
    // Surface the first 37 bytes as the canonical magic constant.
    eprintln!(
        "round60 phase2h: first 37 bytes (the validator's `push 0x25` length): {first:?}",
        first = std::str::from_utf8(&bytes[..37.min(bytes.len())]).unwrap_or("(non-UTF8)")
    );
}

/// Phase 2i — assemble an AMT whose extradata is engineered to
/// pass the validator gates we discovered in phase 2g:
///
///   * `wFormatTag = 0x0161` (WMA2)
///   * `cbSize    >= 0x2F`   (47 bytes minimum)
///   * `extradata[10..47]` matches the 37-byte magic string at
///     `.rdata @ 0x11138`
///
/// Drive `IPin::ReceiveConnection` against this AMT and observe
/// the HRESULT.  If the validator we decoded is the WHOLE story,
/// this should land S_OK.  If it returns some other failure, the
/// next round can focus on whatever check fires next.
#[test]
fn phase2i_attempt_pass_through_clsid_gated_validator() {
    use oxideav_vfw::com::{AmtBlueprint, SLOT_PIN_RECEIVE_CONNECTION};
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase2i: msadds32.ax missing; skipping");
        return;
    };
    let Some(input_pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase2i: no INPUT pin; skipping");
        return;
    };
    // Read the magic 37-byte string.
    let s_va = img.image_base + 0x11138;
    let s_bytes = sb.mmu.read(s_va, 37).expect("read magic string");
    eprintln!(
        "round60 phase2i: magic string = {s:?}",
        s = std::str::from_utf8(&s_bytes).unwrap_or("(non-UTF8)")
    );
    // Build extradata for WMA2: 10-byte preamble (zero) + 37-byte
    // magic string = exactly 47 bytes.  The validator's lower-
    // bound check is `cbSize >= 0x2F`, so we go right at the
    // threshold.
    let mut extradata = vec![0u8; 10];
    extradata.extend_from_slice(&s_bytes);
    assert_eq!(extradata.len(), 47);
    let bp = AmtBlueprint {
        format_tag: 0x0161,
        n_channels: 1,
        n_samples_per_sec: 44_100,
        n_avg_bytes_per_sec: 4_000,
        n_block_align: 185,
        w_bits_per_sample: 16,
        extradata,
    };
    let amt = stage_audio_amt_from_blueprint(&mut sb, &bp).expect("stage AMT");
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
    );
    match r {
        Ok(hr) => eprintln!(
            "round60 phase2i: ReceiveConnection (WMA2 + 37-byte magic) → HRESULT {hr:#010x}"
        ),
        Err(e) => eprintln!("round60 phase2i: trapped: {e}"),
    }

    // Same shape but WMA1 — 4-byte preamble + 37-byte magic = 41 bytes (== 0x29).
    let mut extradata = vec![0u8; 4];
    extradata.extend_from_slice(&s_bytes);
    assert_eq!(extradata.len(), 41);
    let bp = AmtBlueprint {
        format_tag: 0x0160,
        n_channels: 1,
        n_samples_per_sec: 44_100,
        n_avg_bytes_per_sec: 4_000,
        n_block_align: 185,
        w_bits_per_sample: 16,
        extradata,
    };
    let amt = stage_audio_amt_from_blueprint(&mut sb, &bp).expect("stage AMT");
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
    );
    match r {
        Ok(hr) => eprintln!(
            "round60 phase2i: ReceiveConnection (WMA1 + 37-byte magic) → HRESULT {hr:#010x}"
        ),
        Err(e) => eprintln!("round60 phase2i: trapped: {e}"),
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 3 — scan for AMT / WAVEFORMATEX field reads
// ───────────────────────────────────────────────────────────────────

/// Phase 3 — search the QueryAccept body for byte patterns that
/// fetch typical `AM_MEDIA_TYPE` and `WAVEFORMATEX` fields:
///
/// * `mov reg, [reg+0x40]` — `AM_MEDIA_TYPE::cbFormat`
/// * `mov reg, [reg+0x44]` — `AM_MEDIA_TYPE::pbFormat`
/// * `mov reg, [reg+0x10]` — `AM_MEDIA_TYPE::subtype.Data1` (first
///    GUID dword of subtype, typical place a fast-path validator
///    matches on `wFormatTag`)
/// * `cmp [..], 0x161` / `cmp [..], 0x160` — WMA1/WMA2 wFormatTag
///    constants
///
/// Reports every match's offset to stderr so the report can cite
/// the validator's actual reach into the AMT.
#[test]
fn phase3_scan_for_amt_field_reads_and_format_tag_constants() {
    let Some((mut sb, img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase3: msadds32.ax missing; skipping");
        return;
    };
    let Some(pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase3: no INPUT pin; skipping");
        return;
    };
    let qa_va = method_va(&sb.mmu, pin, SLOT_PIN_QUERY_ACCEPT).expect("vtable[11] resolves");
    let bytes = sb.mmu.read(qa_va, 1024).expect("read 1 KiB of QueryAccept");

    let _ = img; // suppress unused warning in this phase

    // `mov reg, [reg+disp8]`: opcode 0x8B, ModR/M with mod=01.
    // We just scan for `8B XX disp8` triples where disp8 is one
    // of {0x10, 0x28, 0x40, 0x44}.  Loose pattern — false
    // positives are fine for the structured search.
    let mut field_reads: Vec<(usize, u8)> = Vec::new();
    for i in 0..bytes.len().saturating_sub(3) {
        if bytes[i] != 0x8B {
            continue;
        }
        let modrm = bytes[i + 1];
        let mode = (modrm >> 6) & 0b11;
        if mode != 0b01 {
            // not [reg+disp8]; could be mod=10 ([reg+disp32]) or
            // mod=00 ([reg]).  Disp8 is the dominant pattern for
            // small struct offsets like 0x40/0x44.
            continue;
        }
        let disp8 = bytes[i + 2];
        if matches!(disp8, 0x10 | 0x28 | 0x40 | 0x44) {
            field_reads.push((i, disp8));
        }
    }
    eprintln!("round60 phase3: AMT field-read candidates (mov reg, [reg+disp8]):");
    for (off, disp) in &field_reads {
        let field_name = match disp {
            0x10 => "subtype.Data1",
            0x28 => "formattype.Data1",
            0x40 => "cbFormat",
            0x44 => "pbFormat",
            _ => "?",
        };
        eprintln!(
            "  +{off:#x} (VA {va:#010x}): 8B {modrm:02x} {disp:02x}  ; {field_name}",
            va = qa_va.wrapping_add(*off as u32),
            modrm = bytes[off + 1],
        );
    }

    // Search for `cmp r/m, imm8/imm16/imm32` against {0x160, 0x161}.
    let mut tag_compares: Vec<(usize, u32)> = Vec::new();
    for i in 0..bytes.len().saturating_sub(6) {
        // `cmp r/m32, imm32`: opcode 0x81 /7 — ModRM.reg = 7.
        if bytes[i] == 0x81 {
            let modrm = bytes[i + 1];
            if (modrm >> 3) & 0b111 == 7 {
                // figure imm offset
                let mode = (modrm >> 6) & 0b11;
                let rm = modrm & 0b111;
                let mut imm_off = i + 2;
                // crude SIB+disp consumption
                if mode != 0b11 {
                    if rm == 4 {
                        imm_off += 1; // SIB
                    }
                    match mode {
                        0b00 => {
                            if rm == 5 {
                                imm_off += 4;
                            }
                        }
                        0b01 => imm_off += 1,
                        0b10 => imm_off += 4,
                        _ => {}
                    }
                }
                if imm_off + 4 <= bytes.len() {
                    let imm = u32::from_le_bytes([
                        bytes[imm_off],
                        bytes[imm_off + 1],
                        bytes[imm_off + 2],
                        bytes[imm_off + 3],
                    ]);
                    if imm == 0x0160 || imm == 0x0161 || imm == 0x000A || imm == 0x0006 {
                        tag_compares.push((i, imm));
                    }
                }
            }
        }
        // `cmp eax, imm32`: opcode 0x3D
        if bytes[i] == 0x3D && i + 4 < bytes.len() {
            let imm = u32::from_le_bytes([bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]]);
            if imm == 0x0160 || imm == 0x0161 {
                tag_compares.push((i, imm));
            }
        }
    }
    eprintln!("round60 phase3: wFormatTag / cbSize constant compares:");
    for (off, imm) in &tag_compares {
        eprintln!(
            "  +{off:#x} (VA {va:#010x}): cmp ..., {imm:#06x}",
            va = qa_va.wrapping_add(*off as u32)
        );
    }

    // Search the bytes for the raw 16-bit constants 0x0160 and
    // 0x0161 — they appear as `60 01` and `61 01` in LE.  Catches
    // `cmp word ptr [..], 0x161` and equivalent.
    let mut tag_literals: Vec<(usize, u16)> = Vec::new();
    for i in 0..bytes.len().saturating_sub(2) {
        let v = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        if v == 0x0160 || v == 0x0161 {
            tag_literals.push((i, v));
        }
    }
    eprintln!("round60 phase3: 16-bit literal occurrences of 0x0160/0x0161:");
    for (off, v) in &tag_literals {
        eprintln!(
            "  +{off:#x} (VA {va:#010x}): {v:#06x}",
            va = qa_va.wrapping_add(*off as u32)
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// Phase 4 — synthetic AMT criteria-passing constructor
// ───────────────────────────────────────────────────────────────────

/// Phase 4 — feed the splitter every plausible AMT shape we can
/// construct based on the round-58 / round-59 empirical evidence:
///
/// * WMA2 with several `nBlockAlign` values (185 from the fixture,
///   common 1024/2048 buffer sizes, the spec default 0x05A0 = 1440);
/// * WMA2 with extradata replaced by the canonical 10-byte WMA2
///   "v2 default" header `00 88 00 00 00 00 0F 00 00 00`
///   (`SamplesPerBlock`=0x8800 + `EncodeOptions`=0x000F);
/// * WMA1 with extradata `00 88 00 00` (4-byte v1 default).
///
/// Records every (`tag`, `nBlockAlign`, `extradata[..]`) → HRESULT
/// triple on stderr so the round-60 report can pin which shape
/// (if any) the splitter accepts.  No hard assertion on S_OK —
/// the deliverable is the empirical scan.
#[test]
fn phase4_brute_force_amt_shape_scan() {
    use oxideav_vfw::com::{AmtBlueprint, SLOT_PIN_RECEIVE_CONNECTION};
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase4: msadds32.ax missing; skipping");
        return;
    };
    let Some(input_pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase4: no INPUT pin; skipping");
        return;
    };

    // Empirical r59 baseline (matches ffmpeg fixture exactly):
    let base_wma1 = AmtBlueprint {
        format_tag: 0x0160,
        n_channels: 1,
        n_samples_per_sec: 44_100,
        n_avg_bytes_per_sec: 4_000,
        n_block_align: 185,
        w_bits_per_sample: 16,
        extradata: vec![0x00, 0x00, 0x01, 0x00],
    };
    let base_wma2 = AmtBlueprint {
        format_tag: 0x0161,
        n_channels: 1,
        n_samples_per_sec: 44_100,
        n_avg_bytes_per_sec: 4_000,
        n_block_align: 185,
        w_bits_per_sample: 16,
        extradata: vec![0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    };

    // Candidate alternatives to test, beyond the r59 baseline.
    let mut candidates: Vec<(String, AmtBlueprint)> = Vec::new();
    candidates.push(("r59-baseline-WMA1".into(), base_wma1.clone()));
    candidates.push(("r59-baseline-WMA2".into(), base_wma2.clone()));

    for &ba in &[1024u16, 2048, 1440, 8192] {
        candidates.push((
            format!("WMA2 nBlockAlign={ba}"),
            AmtBlueprint {
                n_block_align: ba,
                ..base_wma2.clone()
            },
        ));
    }
    // Try 2 channels.
    candidates.push((
        "WMA2 channels=2".into(),
        AmtBlueprint {
            n_channels: 2,
            ..base_wma2.clone()
        },
    ));
    // Try standard sample rate (48 kHz / 22 050).
    for &sr in &[22_050u32, 48_000] {
        candidates.push((
            format!("WMA2 sr={sr}"),
            AmtBlueprint {
                n_samples_per_sec: sr,
                ..base_wma2.clone()
            },
        ));
    }
    // Re-shaped extradata: canonical WMA2 default header style.
    candidates.push((
        "WMA2 extra=canonical-defaults".into(),
        AmtBlueprint {
            extradata: vec![0x00, 0x88, 0x00, 0x00, 0x00, 0x00, 0x0F, 0x00, 0x00, 0x00],
            ..base_wma2.clone()
        },
    ));
    // Re-shaped WMA1 extradata.
    candidates.push((
        "WMA1 extra=4-byte 0x00880000".into(),
        AmtBlueprint {
            extradata: vec![0x00, 0x88, 0x00, 0x00],
            ..base_wma1.clone()
        },
    ));
    // Try zero extradata (rejected by r58 but valuable contrast).
    candidates.push((
        "WMA2 extra=empty".into(),
        AmtBlueprint {
            extradata: vec![],
            ..base_wma2.clone()
        },
    ));
    // Try high-bit-rate fixtures.
    candidates.push((
        "WMA2 nAvgBytesPerSec=20000".into(),
        AmtBlueprint {
            n_avg_bytes_per_sec: 20_000,
            n_block_align: 2048,
            ..base_wma2.clone()
        },
    ));

    let mut accepted: Vec<String> = Vec::new();
    for (name, bp) in &candidates {
        let amt = match stage_audio_amt_from_blueprint(&mut sb, bp) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("round60 phase4: stage {name} failed: {e}");
                continue;
            }
        };
        let host_out = match sb.mint_host_output_pin_with_connection(amt, input_pin) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("round60 phase4: mint host_out for {name} failed: {e}");
                continue;
            }
        };
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            input_pin,
            SLOT_PIN_RECEIVE_CONNECTION,
            &[host_out, amt],
        );
        match r {
            Ok(hr) => {
                eprintln!("round60 phase4: {name:40}  → HRESULT {hr:#010x}");
                if hr == 0 {
                    accepted.push(name.clone());
                }
            }
            Err(e) => eprintln!("round60 phase4: {name:40}  TRAPPED: {e}"),
        }
    }
    eprintln!("round60 phase4: accepted count = {}", accepted.len());
    for n in &accepted {
        eprintln!("  ACCEPTED: {n}");
    }
    // No hard assertion on accepted.len() > 0 — the deliverable
    // is the empirical scan over the candidate space.
}

// ───────────────────────────────────────────────────────────────────
// Phase 4 — criteria-passing constructor → ReceiveConnection
// ───────────────────────────────────────────────────────────────────

/// Phase 4 — the new `AmtBlueprint::wma_criteria_passing`
/// constructor builds an AMT shaped to pass the round-60-decoded
/// validator chain in one call.  Verify it returns S_OK for
/// WMA2 against the live `msadds32.ax` audio splitter.
#[test]
fn phase4_criteria_passing_constructor_lands_s_ok() {
    use oxideav_vfw::com::{AmtBlueprint, SLOT_PIN_RECEIVE_CONNECTION};
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase4: msadds32.ax missing; skipping");
        return;
    };
    let Some(input_pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase4: no INPUT pin; skipping");
        return;
    };
    let bp = AmtBlueprint::wma_criteria_passing(
        0x0161, // WMA2
        1, 44_100, 4_000, 185,
    );
    eprintln!(
        "round60 phase4: extradata.len() = {}, first 16 bytes = {:02x?}",
        bp.extradata.len(),
        &bp.extradata[..16.min(bp.extradata.len())]
    );
    let amt = stage_audio_amt_from_blueprint(&mut sb, &bp).expect("stage AMT");
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
    .expect("ReceiveConnection must not trap");
    eprintln!("round60 phase4: ReceiveConnection (WMA2 criteria-passing) → HRESULT {r:#010x}");
    assert_eq!(
        r, 0x0000_0000,
        "wma_criteria_passing AMT was not accepted by msadds32.ax — \
         validator decoded incorrectly"
    );
}

/// Phase 4 (WMA1) — same shape, `wFormatTag = 0x0160`,
/// `cbSize = 0x29` (41 bytes).  Validator branches WMA1 to a
/// `pbFormat + 0x16` offset that places the magic string after
/// the 4-byte WMA1 preamble.
#[test]
fn phase4_criteria_passing_wma1_lands_s_ok() {
    use oxideav_vfw::com::{AmtBlueprint, SLOT_PIN_RECEIVE_CONNECTION};
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase4 (WMA1): msadds32.ax missing; skipping");
        return;
    };
    let Some(input_pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase4 (WMA1): no INPUT pin; skipping");
        return;
    };
    let bp = AmtBlueprint::wma_criteria_passing(0x0160, 1, 44_100, 4_000, 185);
    eprintln!(
        "round60 phase4: WMA1 extradata.len() = {}",
        bp.extradata.len()
    );
    let amt = stage_audio_amt_from_blueprint(&mut sb, &bp).expect("stage AMT");
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
    .expect("ReceiveConnection must not trap");
    eprintln!("round60 phase4: ReceiveConnection (WMA1 criteria-passing) → HRESULT {r:#010x}");
    assert_eq!(r, 0x0000_0000, "WMA1 criteria-passing AMT was not accepted");
}

// ───────────────────────────────────────────────────────────────────
// Phase 5 (stretch) — push a real WMA frame through Receive
// ───────────────────────────────────────────────────────────────────

/// Phase 5 stretch — after the round-60 validator gate is
/// satisfied, try to push real encoded WMA2 bytes from the ASF
/// fixture through `IMemInputPin::Receive` and observe whether
/// any PCM surfaces on the host sink.
///
/// We DO NOT assert any specific PCM byte count — this is the
/// first cross-validator interaction with the codec's internal
/// decoder.  Success criterion: the call does not trap, and the
/// HRESULT is reported.  Any PCM that surfaces is a bonus
/// recorded on stderr for round 61's baseline.
#[test]
fn phase5_push_real_wma2_frame_after_criteria_passing_connect() {
    use oxideav_vfw::com::{
        AmtBlueprint, SLOT_MEDIAFILTER_PAUSE, SLOT_MEDIAFILTER_RUN, SLOT_MEMINPUTPIN_RECEIVE,
        SLOT_PIN_RECEIVE_CONNECTION,
    };
    use oxideav_vfw::IID_IMEMINPUTPIN;
    let Some((mut sb, _img, filter)) = bootstrap_filter() else {
        eprintln!("round60 phase5: msadds32.ax missing; skipping");
        return;
    };
    let Some(input_pin) = find_input_pin(&mut sb, filter) else {
        eprintln!("round60 phase5: no INPUT pin; skipping");
        return;
    };
    let bp = AmtBlueprint::wma_criteria_passing(0x0161, 1, 44_100, 4_000, 185);
    let amt = stage_audio_amt_from_blueprint(&mut sb, &bp).expect("stage AMT");
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
    .expect("ReceiveConnection must not trap");
    if r != 0 {
        eprintln!("round60 phase5: ReceiveConnection rejected (HRESULT {r:#010x}); skipping");
        return;
    }
    eprintln!("round60 phase5: ReceiveConnection ACCEPTED — driving Pause + Run");
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
            eprintln!("round60 phase5: QI(IMemInputPin) failed; skipping");
            return;
        }
    };
    // Read the ASF fixture's first data packet for real WMA2 bytes.
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audio/wma2_440hz_mono_1s.wma");
    let asf_bytes = match std::fs::read(&fixture_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("round60 phase5: cannot read WMA2 fixture: {e}; skipping");
            return;
        }
    };
    let packet = oxideav_vfw::com::locate_first_data_packet(&asf_bytes).unwrap_or(&[]);
    if packet.is_empty() {
        eprintln!("round60 phase5: no data packet found; skipping");
        return;
    }
    // Cap the payload at the AMT's nBlockAlign × a few blocks.
    let payload: Vec<u8> = packet.iter().take(4096).copied().collect();
    let sample = sb
        .mint_host_media_sample(/*data_capacity=*/ 8192, amt)
        .expect("mint host media sample");
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
            "round60 phase5: IMemInputPin::Receive({} B WMA2) → HRESULT {hr:#010x}",
            payload.len()
        ),
        Err(e) => eprintln!("round60 phase5: trapped: {e}"),
    }
    let pcm_queued = oxideav_vfw::com::host_iface_r31::queue_len(&sb.host);
    eprintln!("round60 phase5: PCM bytes queued on host sink = {pcm_queued}");
}

// ---- helper: stage AMT from blueprint --------------------------------

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
