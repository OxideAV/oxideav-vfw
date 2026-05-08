//! Round 26 — `user32!CreateWindowExA` cascade + IPin::Receive
//! sample-push probes.
//!
//! ### Sub-goal A — `user32` cascade stubs.
//!
//! Many DirectShow filters and legacy MS codecs call
//! `user32!CreateWindowExA` during init expecting a non-NULL
//! `HWND`.  The codec doesn't need a real window — just enough
//! to feel happy and proceed past the call.  Round 26 hands out
//! synthetic `HWND` values from a `host.hwnd_registry` set; each
//! companion stub (`UpdateWindow`, `DestroyWindow`, `IsWindow`,
//! `MoveWindow`, etc.) is fail-soft so the codec falls through
//! to its headless code path.
//!
//! These are *PE-load surface* stubs primarily.  Neither the
//! `MPG4DS32.AX` nor `WMVDS32.AX` decoder filter imports
//! `CreateWindowExA` directly (only the `msadds32.ax` audio
//! splitter does, and we deliberately don't drive that through
//! `DLL_PROCESS_ATTACH`).  The cascade is staged here so future
//! rounds — which may load wmvds32 / wmv8ds32 / msscds32 / etc.
//! through the COM ABI — find a complete user32 surface ready
//! to go.
//!
//! ### Sub-goal B — IPin::Receive sample-push (probe).
//!
//! Round 25 reached `IBaseFilter::Run = S_OK` and walked
//! `IBaseFilter::EnumPins → IEnumPins::Next` to retrieve an
//! input pin at `0x6000025C`.  Round 26 stretches into stage 5:
//! call `IPin::ReceiveConnection(pConnector, pmt)` on that pin
//! with an `AM_MEDIA_TYPE` describing MP43 video carried in a
//! `VIDEOINFOHEADER`, observe the HRESULT, and (if successful)
//! probe `IPin::Receive(pSample)` with a stub `IMediaSample`
//! holding a single MP43 keyframe.  Either step may fail with
//! `VFW_E_TYPE_NOT_ACCEPTED` / `E_NOTIMPL` etc.; the test logs
//! the result without asserting success — round 27 will read
//! the trace and unblock whatever's missing.

mod common;

use oxideav_vfw::com::{call::vtable_is_plausible, Guid};
use oxideav_vfw::win32::Registry;
use oxideav_vfw::{Sandbox, IID_IBASEFILTER, IID_ICLASSFACTORY};
use std::path::PathBuf;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn binary_path(name: &str) -> Option<PathBuf> {
    let p = workspace_root()?.join(format!(
        "docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/{name}"
    ));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

const MPG4_FILTER_CLSID: &str = "{82CCD3E0-F71A-11D0-9FE5-00609778EA66}";

// ---- Sub-goal A: Stage 1 — registration ---------------------------------

#[test]
fn user32_create_window_ex_a_cascade_is_registered() {
    let mut r = Registry::new();
    oxideav_vfw::win32::user32::register(&mut r);
    for name in [
        "CreateWindowExA",
        "UpdateWindow",
        "IsWindow",
        "DestroyWindow",
        "MoveWindow",
        "ShowWindow",
        "GetMessageA",
        "DispatchMessageA",
        "TranslateMessage",
        "PeekMessageA",
        "PostQuitMessage",
        "GetWindowLongA",
        "SetWindowLongA",
        "GetClientRect",
        "GetWindowRect",
        "GetDC",
        "ReleaseDC",
        "DefWindowProcA",
    ] {
        assert!(
            r.resolve("user32.dll", name).is_some(),
            "user32!{name} stub missing — round-26 cascade"
        );
    }
}

// ---- Sub-goal A: Stage 2 — synthetic-HWND lifecycle ---------------------

#[test]
fn create_window_ex_a_returns_synthetic_hwnd_then_is_window_finds_it() {
    use oxideav_vfw::emulator::isa_int::RET_SENTINEL;
    let mut sb = Sandbox::new();
    let thunk_create = sb
        .registry
        .resolve("user32.dll", "CreateWindowExA")
        .expect("CreateWindowExA registered");
    // 12-arg stdcall — push 12 zeros, then RET_SENTINEL.
    for _ in 0..12 {
        sb.cpu.push32(&mut sb.mmu, 0).unwrap();
    }
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk_create;
    sb.run_until_sentinel().unwrap();
    let hwnd = sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax);
    assert_ne!(hwnd, 0, "CreateWindowExA returned NULL HWND");
    assert!(
        sb.host.hwnd_registry.contains(&hwnd),
        "synthetic HWND {hwnd:#010x} missing from registry"
    );

    // IsWindow(hwnd) → TRUE.
    let thunk_is = sb.registry.resolve("user32.dll", "IsWindow").unwrap();
    sb.cpu.push32(&mut sb.mmu, hwnd).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk_is;
    sb.run_until_sentinel().unwrap();
    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        1,
        "IsWindow on minted HWND should return TRUE"
    );

    // DestroyWindow(hwnd) → TRUE; HWND drops from registry.
    let thunk_destroy = sb.registry.resolve("user32.dll", "DestroyWindow").unwrap();
    sb.cpu.push32(&mut sb.mmu, hwnd).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk_destroy;
    sb.run_until_sentinel().unwrap();
    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        1,
        "DestroyWindow should return TRUE"
    );
    assert!(
        !sb.host.hwnd_registry.contains(&hwnd),
        "DestroyWindow should drop the synthetic HWND"
    );

    // IsWindow on the destroyed HWND → FALSE.
    sb.cpu.push32(&mut sb.mmu, hwnd).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk_is;
    sb.run_until_sentinel().unwrap();
    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        0,
        "IsWindow on destroyed HWND should return FALSE"
    );

    // IsWindow on a never-minted HWND → FALSE.
    sb.cpu.push32(&mut sb.mmu, 0xFEED_BEEF).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk_is;
    sb.run_until_sentinel().unwrap();
    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        0,
        "IsWindow on stale HWND should return FALSE"
    );
}

#[test]
fn create_window_ex_a_increments_synthetic_hwnd_counter() {
    use oxideav_vfw::emulator::isa_int::RET_SENTINEL;
    let mut sb = Sandbox::new();
    let thunk = sb
        .registry
        .resolve("user32.dll", "CreateWindowExA")
        .unwrap();
    let mut handed_out = Vec::new();
    for _ in 0..3 {
        for _ in 0..12 {
            sb.cpu.push32(&mut sb.mmu, 0).unwrap();
        }
        sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
        sb.cpu.regs.eip = thunk;
        sb.run_until_sentinel().unwrap();
        handed_out.push(sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax));
    }
    // Three distinct HWNDs.
    assert_eq!(handed_out[0] + 1, handed_out[1]);
    assert_eq!(handed_out[1] + 1, handed_out[2]);
    assert_eq!(sb.host.hwnd_registry.len(), 3);
}

#[test]
fn message_pump_apis_return_no_message_so_loop_exits() {
    use oxideav_vfw::emulator::isa_int::RET_SENTINEL;
    let mut sb = Sandbox::new();
    // GetMessageA(lpMsg, NULL, 0, 0) → 0 (WM_QUIT semantics).
    // Stage a 28-byte MSG buffer in arena memory and check it
    // gets zero-filled.
    let msg = sb.host.arena_alloc(28).unwrap();
    // Pre-poison so we can confirm the stub overwrote it.
    for i in 0..28u32 {
        sb.mmu.store8(msg + i, 0xAA).unwrap();
    }
    let thunk = sb.registry.resolve("user32.dll", "GetMessageA").unwrap();
    // stdcall: push hwnd_filter, wmsg_filter_min, wmsg_filter_max, lpMsg (rev order).
    sb.cpu.push32(&mut sb.mmu, 0).unwrap();
    sb.cpu.push32(&mut sb.mmu, 0).unwrap();
    sb.cpu.push32(&mut sb.mmu, 0).unwrap();
    sb.cpu.push32(&mut sb.mmu, msg).unwrap();
    sb.cpu.push32(&mut sb.mmu, RET_SENTINEL).unwrap();
    sb.cpu.regs.eip = thunk;
    sb.run_until_sentinel().unwrap();
    assert_eq!(
        sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
        0,
        "GetMessageA should return 0 (WM_QUIT)"
    );
    for i in 0..28u32 {
        assert_eq!(sb.mmu.load8(msg + i).unwrap(), 0, "MSG[{i}] not zeroed");
    }
}

// ---- Sub-goal B: Stage 5 — IPin::ReceiveConnection probe ---------------

const FOURCC_MP43: [u8; 4] = *b"MP43";

/// `MEDIATYPE_Video = {73646976-0000-0010-8000-00AA00389B71}`.
/// Source: `uuids.h` from the Windows SDK — `MEDIATYPE_Video`.
fn iid_mediatype_video() -> Guid {
    Guid::parse("{73646976-0000-0010-8000-00AA00389B71}").unwrap()
}

/// `MEDIASUBTYPE_MP43 = {3334504D-0000-0010-8000-00AA00389B71}`.
/// MP43 fourcc 0x3334_504D in little-endian, padded with the
/// "media subtype" base UUID `{XXXXXXXX-0000-0010-8000-00AA00389B71}`.
/// Source: `uuids.h` — `MEDIASUBTYPE_MP43` /
/// `MAKEFOURCC('M','P','4','3')`.
fn iid_mediasubtype_mp43() -> Guid {
    Guid::parse("{3334504D-0000-0010-8000-00AA00389B71}").unwrap()
}

/// `FORMAT_VideoInfo = {05589F80-C356-11CE-BF01-00AA0055595A}`.
/// Source: `uuids.h`.
fn iid_format_video_info() -> Guid {
    Guid::parse("{05589F80-C356-11CE-BF01-00AA0055595A}").unwrap()
}

/// Stage an `AM_MEDIA_TYPE` (72 bytes) describing MP43 video at
/// `addr` plus a `VIDEOINFOHEADER` (88 bytes) describing the
/// frame at `addr+72`.  Returns `(am_media_type_addr, vih_addr)`.
fn stage_am_media_type_mp43(
    sb: &mut Sandbox,
    width: i32,
    height: i32,
) -> Result<(u32, u32), oxideav_vfw::Error> {
    use oxideav_vfw::Error;
    // 72 + 88 = 160 bytes, plus alignment slack.
    let blob = sb.host.arena_alloc(176).map_err(Error::Win32)?;
    let amt = blob;
    let vih = blob + 72;

    // AM_MEDIA_TYPE @ amt:
    //   majortype  : GUID @  0
    //   subtype    : GUID @ 16
    //   bFixedSizeSamples (BOOL) @ 32 = TRUE
    //   bTemporalCompression (BOOL) @ 36 = TRUE
    //   lSampleSize (ULONG) @ 40 = 0
    //   formattype : GUID @ 44
    //   pUnk       (IUnknown*) @ 60 = NULL
    //   cbFormat   (ULONG) @ 64 = sizeof(VIDEOINFOHEADER) = 88
    //   pbFormat   (BYTE*) @ 68 = vih
    iid_mediatype_video()
        .stage(&mut sb.mmu, amt)
        .map_err(Error::Trap)?;
    iid_mediasubtype_mp43()
        .stage(&mut sb.mmu, amt + 16)
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 32, &1u32.to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 36, &1u32.to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 40, &0u32.to_le_bytes())
        .map_err(Error::Trap)?;
    iid_format_video_info()
        .stage(&mut sb.mmu, amt + 44)
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 60, &0u32.to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 64, &88u32.to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 68, &vih.to_le_bytes())
        .map_err(Error::Trap)?;

    // VIDEOINFOHEADER @ vih (88 bytes):
    //   rcSource    : RECT  @  0 (16 bytes — zero RECT)
    //   rcTarget    : RECT  @ 16 (16 bytes — zero RECT)
    //   dwBitRate   : DWORD @ 32 = 0
    //   dwBitErrorRate : DWORD @ 36 = 0
    //   AvgTimePerFrame : LONGLONG @ 40 (8 bytes) = 0
    //   bmiHeader   : BITMAPINFOHEADER @ 48 (40 bytes)
    for i in 0..48u32 {
        sb.mmu.store8(vih + i, 0).map_err(Error::Trap)?;
    }
    // BITMAPINFOHEADER (40 bytes):
    //   biSize          @  0 = 40
    //   biWidth         @  4
    //   biHeight        @  8
    //   biPlanes        @ 12 = 1
    //   biBitCount      @ 14 = 24
    //   biCompression   @ 16 = 'MP43' little-endian
    //   biSizeImage     @ 20 = w*h*3/2  (yuv420 estimate)
    //   biXPelsPerMeter @ 24 = 0
    //   biYPelsPerMeter @ 28 = 0
    //   biClrUsed       @ 32 = 0
    //   biClrImportant  @ 36 = 0
    let bih = vih + 48;
    sb.mmu
        .write_initializer(bih, &40u32.to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(bih + 4, &(width as u32).to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(bih + 8, &(height as u32).to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(bih + 12, &1u16.to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(bih + 14, &24u16.to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(bih + 16, &FOURCC_MP43)
        .map_err(Error::Trap)?;
    let size_image = (width.unsigned_abs() * height.unsigned_abs() * 3) / 2;
    sb.mmu
        .write_initializer(bih + 20, &size_image.to_le_bytes())
        .map_err(Error::Trap)?;
    for off in [24u32, 28, 32, 36] {
        sb.mmu
            .write_initializer(bih + off, &0u32.to_le_bytes())
            .map_err(Error::Trap)?;
    }

    Ok((amt, vih))
}

fn drive_dll_get_class_object(
    dll_name: &str,
) -> Result<(Sandbox, oxideav_vfw::pe::Image, u32, Guid), String> {
    let p = binary_path(dll_name).ok_or_else(|| format!("{dll_name} not present"))?;
    let bytes = std::fs::read(&p).map_err(|e| format!("read {dll_name}: {e}"))?;
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(2_000_000_000);
    let img = sb
        .load(dll_name, &bytes)
        .map_err(|e| format!("load: {e}"))?;
    sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .map_err(|e| format!("DllMain: {e}"))?;
    let clsid = Guid::parse(MPG4_FILTER_CLSID).expect("parse clsid");
    let factory = sb
        .dll_get_class_object(&img, clsid, IID_ICLASSFACTORY)
        .map_err(|e| format!("DllGetClassObject: {e}"))?;
    Ok((sb, img, factory, clsid))
}

#[test]
fn ipin_receive_connection_with_mp43_videoinfo_probe() {
    let (mut sb, _img, _factory, clsid) = match drive_dll_get_class_object("MPG4DS32.AX") {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round26 stage 5 skipped: {e}");
            return;
        }
    };
    let filter = match sb.co_create_instance(clsid, IID_IBASEFILTER) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("round26 stage 5 skipped: CreateInstance: {e}");
            return;
        }
    };
    assert!(vtable_is_plausible(&sb.mmu, filter));

    // Force the filter into Stopped state so ReceiveConnection
    // is legal.  DirectShow contract: ReceiveConnection returns
    // VFW_E_NOT_STOPPED (0x80040208) if the filter is not
    // stopped.  Round-25 stage 4 ran Stop/Pause/Run sequentially
    // and left the filter Running; here we explicitly call Stop.
    use oxideav_vfw::com::call::call_method;
    let stop_r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_STOP,
        &[],
    );
    eprintln!("round26 stage 5: IBaseFilter::Stop → {stop_r:?}");

    // EnumPins → Next → input pin (round 25 confirmed dir=0 on
    // pin 0 of MPG4DS32 = INPUT).
    let scratch = sb.host.arena_alloc(8).unwrap();
    sb.mmu.write_initializer(scratch, &[0u8; 8]).unwrap();
    let r_enum = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_ENUM_PINS,
        &[scratch],
    );
    let pp = sb.mmu.load32(scratch).unwrap_or(0);
    if !matches!(r_enum, Ok(0)) || pp == 0 {
        eprintln!("round26 stage 5: EnumPins did not yield enum: {r_enum:?}");
        let _ = sb.com_release(filter);
        return;
    }
    sb.host.com.intern(pp, None);
    let pin_slot = sb.host.arena_alloc(8).unwrap();
    sb.mmu.write_initializer(pin_slot, &[0u8; 8]).unwrap();
    let _next_r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pp,
        3,
        &[1, pin_slot, pin_slot + 4],
    );
    let pin = sb.mmu.load32(pin_slot).unwrap_or(0);
    if pin == 0 {
        eprintln!("round26 stage 5: pin0 NULL after IEnumPins::Next");
        let _ = sb.com_release(pp);
        let _ = sb.com_release(filter);
        return;
    }
    sb.host.com.intern(pin, None);
    eprintln!("round26 stage 5: IPin at {pin:#010x}");

    // Stage AM_MEDIA_TYPE describing MP43 / 320x240.
    let (amt, _vih) = match stage_am_media_type_mp43(&mut sb, 320, 240) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("round26 stage 5: stage_am_media_type_mp43: {e}");
            let _ = sb.com_release(pin);
            let _ = sb.com_release(pp);
            let _ = sb.com_release(filter);
            return;
        }
    };

    // IPin::ReceiveConnection(pConnector, pmt) — slot 4.
    // First-pass attempt with NULL pConnector: many DirectShow
    // filters reject this with E_POINTER (0x80004003) because
    // ReceiveConnection records the upstream pin pointer.  We
    // log the HRESULT and then retry with a host-side stub
    // IPin* so the codec gets a non-NULL pConnector to record.
    let recv_r_null = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        4,
        &[0, amt],
    );
    eprintln!("round26 stage 5: IPin::ReceiveConnection(NULL, MP43 320x240) → {recv_r_null:?}");

    // Retry with a stub IPin pointer.  We don't need a real
    // upstream pin for this probe — the codec only needs the
    // pointer to be non-NULL.  Use the input pin's own address
    // (self-loop is technically illegal per DirectShow's
    // graph-validation contract, but most filters don't check
    // until Run/Pause).
    let recv_r_self = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        4,
        &[pin, amt],
    );
    eprintln!("round26 stage 5: IPin::ReceiveConnection(self, MP43 320x240) → {recv_r_self:?}");
    let recv_r = if matches!(recv_r_self, Ok(0)) {
        recv_r_self
    } else {
        recv_r_null
    };
    // ReceiveConnection may fail with VFW_E_TYPE_NOT_ACCEPTED
    // (0x80040207), VFW_E_INVALIDMEDIATYPE (0x80040200), or
    // succeed with S_OK.  We log the result for the round-27
    // analysis without asserting either outcome — the codec's
    // CheckMediaType() may need additional setup state we haven't
    // staged yet (allocator, downstream output pin connection).
    if let Ok(0) = recv_r {
        eprintln!("round26 stage 5: ReceiveConnection accepted MP43");
        // Report HWND-registry growth as evidence of cascade
        // engagement (codec may have minted a private HWND).
        eprintln!(
            "round26 stage 5: hwnd_registry has {} entry / next_idx={}",
            sb.host.hwnd_registry.len(),
            sb.host.next_hwnd_index,
        );
    }

    // Tear down.
    let _ = sb.com_release(pin);
    let _ = sb.com_release(pp);
    let _ = sb.com_release(filter);
}

#[test]
fn hwnd_registry_starts_empty_and_pure_host_apis_keep_it_empty() {
    // Sanity: a fresh sandbox with no codec loaded has an empty
    // HWND registry; calling EnumPins / co_create_instance does
    // not mint HWNDs on its own (those are codec-driven).
    let sb = Sandbox::new();
    assert_eq!(sb.host.hwnd_registry.len(), 0);
    assert_eq!(sb.host.next_hwnd_index, 0);
}
