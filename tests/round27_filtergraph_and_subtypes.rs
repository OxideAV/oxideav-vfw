//! Round 27 — IFilterGraph host stub + MEDIASUBTYPE / FORMAT_*
//! probe matrix to push past round-26's `VFW_E_NO_TYPES`
//! (`0x80040208`) HRESULT from `IPin::ReceiveConnection`.
//!
//! Round 26 reached the codec's input pin and called
//! `IPin::ReceiveConnection(self_ptr, AM_MEDIA_TYPE_for_MP43)` —
//! the codec returned `VFW_E_NO_TYPES` because its
//! `CheckMediaType()` wants graph-aware negotiation, not a
//! self-pin handshake.  Two-pronged fix:
//!
//! * **A.1** — try every plausible `(MEDIASUBTYPE, FORMAT_*,
//!   biCompression)` combination against `ReceiveConnection`.
//!   Binary string-table inspection of `MPG4DS32.AX` and
//!   `WMVDS32.AX` shows both binaries enumerate the same set of
//!   FOURCCs (`MP43`/`mp43`, `MP4S`/`mp4s`, `MPG4`/`mpg4`,
//!   `MP42`/`mp42`, `WMV1`/`wmv1`, `MSS1`/`mss1`).  Round 26
//!   tried only `MP43`; this round walks the full list, plus the
//!   ASCII-lower-case variants the codec's `lstrcmpiA`-ish
//!   matcher accepts.
//! * **A.2** — back the codec with a host `IFilterGraph` stub
//!   (every method `E_NOTIMPL` — see
//!   `crate::com::host_iface`) and call
//!   `IBaseFilter::JoinFilterGraph(host_graph, NULL)` BEFORE
//!   `ReceiveConnection`.  DirectShow filters are documented to
//!   refuse pin-level operations until they are part of a graph;
//!   the round-26 self-pin handshake violated that contract.
//!
//! The probe asserts no specific HRESULT — round 27's
//! deliverable is the *log line* showing which combinations get
//! past `CheckMediaType`.  Round 28 then targets whichever
//! combination yields `S_OK` (or a closer failure than
//! `VFW_E_NO_TYPES`).
//!
//! Reference for every layout / IID / HRESULT is MSDN public
//! documentation:
//! * `AM_MEDIA_TYPE` —
//!   <https://learn.microsoft.com/en-us/windows/win32/api/strmif/ns-strmif-am_media_type>
//! * `VIDEOINFOHEADER` —
//!   <https://learn.microsoft.com/en-us/windows/win32/api/amvideo/ns-amvideo-videoinfoheader>
//! * `VIDEOINFOHEADER2` —
//!   <https://learn.microsoft.com/en-us/windows/win32/api/dvdmedia/ns-dvdmedia-videoinfoheader2>
//! * `BITMAPINFOHEADER` —
//!   <https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-bitmapinfoheader>
//! * `IPin::ReceiveConnection` HRESULTs —
//!   <https://learn.microsoft.com/en-us/windows/win32/api/strmif/nf-strmif-ipin-receiveconnection>
//! * `IBaseFilter::JoinFilterGraph` —
//!   <https://learn.microsoft.com/en-us/windows/win32/api/strmif/nf-strmif-ibasefilter-joinfiltergraph>

mod common;

use oxideav_vfw::com::call::vtable_is_plausible;
use oxideav_vfw::com::Guid;
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

/// Build a `MEDIASUBTYPE_<FOURCC>` GUID by splicing the FOURCC
/// (little-endian) into the `{XXXXXXXX-0000-0010-8000-00AA00389B71}`
/// DirectShow-fourcc base GUID.  Source: `uuids.h` from the
/// Windows SDK — `MEDIATYPE_Video` family treats `Data1` as
/// `MAKEFOURCC(c0, c1, c2, c3)` (little-endian — the byte at
/// offset 0 of `Data1` is `c0`).
fn fourcc_subtype(fourcc: &[u8; 4]) -> Guid {
    let d1 = u32::from_le_bytes(*fourcc);
    Guid::new(
        d1,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    )
}

/// `MEDIATYPE_Video = {73646976-0000-0010-8000-00AA00389B71}`.
fn iid_mediatype_video() -> Guid {
    Guid::parse("{73646976-0000-0010-8000-00AA00389B71}").unwrap()
}
/// `FORMAT_VideoInfo = {05589F80-C356-11CE-BF01-00AA0055595A}`.
fn iid_format_video_info() -> Guid {
    Guid::parse("{05589F80-C356-11CE-BF01-00AA0055595A}").unwrap()
}
/// `FORMAT_VideoInfo2 = {F72A76A0-EB0A-11D0-ACE4-0000C0CC16BA}`.
fn iid_format_video_info2() -> Guid {
    Guid::parse("{F72A76A0-EB0A-11D0-ACE4-0000C0CC16BA}").unwrap()
}

/// Stage an `AM_MEDIA_TYPE` (72 bytes) describing a video media
/// type at `addr`, plus a format-specific descriptor at `addr+72`.
///
/// `format_kind`:
/// * `VIH1` — emit a 88-byte `VIDEOINFOHEADER`
///   (`FORMAT_VideoInfo`).
/// * `VIH2` — emit a 112-byte `VIDEOINFOHEADER2`
///   (`FORMAT_VideoInfo2`).  The two structs share the same
///   leading 16-byte rcSource/rcTarget RECTs and the same trailing
///   `BITMAPINFOHEADER`, but VIH2 inserts 24 bytes of new fields
///   (`dwInterlaceFlags`/`dwCopyProtectFlags`/`dwPictAspectRatioX`
///   /`dwPictAspectRatioY`/`dwControlFlags`/`dwReserved2`) before
///   the BITMAPINFOHEADER.
fn stage_am_media_type(
    sb: &mut Sandbox,
    subtype: Guid,
    fourcc: [u8; 4],
    width: i32,
    height: i32,
    format_kind: FormatKind,
) -> Result<u32, oxideav_vfw::Error> {
    use oxideav_vfw::Error;
    let format_size = format_kind.size();
    // 72 (AM_MEDIA_TYPE) + format + alignment.
    let total = 72 + format_size + 16;
    let blob = sb.host.arena_alloc(total).map_err(Error::Win32)?;
    let amt = blob;
    let fmt = blob + 72;

    let format_guid = match format_kind {
        FormatKind::VIH1 => iid_format_video_info(),
        FormatKind::VIH2 => iid_format_video_info2(),
    };

    // AM_MEDIA_TYPE @ amt (72 bytes):
    iid_mediatype_video()
        .stage(&mut sb.mmu, amt)
        .map_err(Error::Trap)?;
    subtype.stage(&mut sb.mmu, amt + 16).map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 32, &1u32.to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 36, &1u32.to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 40, &0u32.to_le_bytes())
        .map_err(Error::Trap)?;
    format_guid
        .stage(&mut sb.mmu, amt + 44)
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 60, &0u32.to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 64, &(format_size).to_le_bytes())
        .map_err(Error::Trap)?;
    sb.mmu
        .write_initializer(amt + 68, &fmt.to_le_bytes())
        .map_err(Error::Trap)?;

    // Format payload @ fmt: VIH1 (88 B) or VIH2 (112 B).  Both
    // start with two RECTs (rcSource / rcTarget — zeroed).
    for i in 0..32u32 {
        sb.mmu.store8(fmt + i, 0).map_err(Error::Trap)?;
    }
    // dwBitRate / dwBitErrorRate / AvgTimePerFrame (8 bytes) =
    // zero across both VIH1 and VIH2.
    for i in 32..48u32 {
        sb.mmu.store8(fmt + i, 0).map_err(Error::Trap)?;
    }
    let bih = match format_kind {
        FormatKind::VIH1 => fmt + 48,
        FormatKind::VIH2 => {
            // Six new DWORDs at offset 48..72 in VIH2:
            //   dwInterlaceFlags (= 0)
            //   dwCopyProtectFlags (= 0)
            //   dwPictAspectRatioX (= width)
            //   dwPictAspectRatioY (= height)
            //   dwControlFlags (= 0)
            //   dwReserved2 (= 0)
            sb.mmu
                .write_initializer(fmt + 48, &0u32.to_le_bytes())
                .map_err(Error::Trap)?;
            sb.mmu
                .write_initializer(fmt + 52, &0u32.to_le_bytes())
                .map_err(Error::Trap)?;
            sb.mmu
                .write_initializer(fmt + 56, &(width as u32).to_le_bytes())
                .map_err(Error::Trap)?;
            sb.mmu
                .write_initializer(fmt + 60, &(height as u32).to_le_bytes())
                .map_err(Error::Trap)?;
            sb.mmu
                .write_initializer(fmt + 64, &0u32.to_le_bytes())
                .map_err(Error::Trap)?;
            sb.mmu
                .write_initializer(fmt + 68, &0u32.to_le_bytes())
                .map_err(Error::Trap)?;
            fmt + 72
        }
    };

    // BITMAPINFOHEADER (40 bytes):
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
        .write_initializer(bih + 16, &fourcc)
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

    Ok(amt)
}

#[derive(Clone, Copy, Debug)]
enum FormatKind {
    VIH1,
    VIH2,
}
impl FormatKind {
    fn size(self) -> u32 {
        match self {
            FormatKind::VIH1 => 88,
            FormatKind::VIH2 => 112,
        }
    }
    #[allow(dead_code)]
    fn name(self) -> &'static str {
        match self {
            FormatKind::VIH1 => "VIH1",
            FormatKind::VIH2 => "VIH2",
        }
    }
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

/// Walk EnumPins → Next → first input pin.  Returns the pin
/// pointer or `None` if anything failed (with diagnostic on
/// stderr).  Mirrors round 26's helper so this test can stage
/// all combinations from a freshly-bootstrapped sandbox.
fn first_input_pin(sb: &mut Sandbox, filter: u32) -> Option<u32> {
    use oxideav_vfw::com::call::call_method;
    // Stop the filter so ReceiveConnection is legal.
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_STOP,
        &[],
    );
    // EnumPins(filter, &ppEnum).
    let scratch = sb.host.arena_alloc(8).ok()?;
    sb.mmu.write_initializer(scratch, &[0u8; 8]).ok()?;
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_ENUM_PINS,
        &[scratch],
    );
    let pp = sb.mmu.load32(scratch).unwrap_or(0);
    if !matches!(r, Ok(0)) || pp == 0 {
        eprintln!("first_input_pin: EnumPins failed: {r:?}");
        return None;
    }
    sb.host.com.intern(pp, None);
    let pin_slot = sb.host.arena_alloc(8).ok()?;
    sb.mmu.write_initializer(pin_slot, &[0u8; 8]).ok()?;
    let _ = call_method(
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
        eprintln!("first_input_pin: pin0 NULL after IEnumPins::Next");
        let _ = sb.com_release(pp);
        return None;
    }
    sb.host.com.intern(pin, None);
    let _ = sb.com_release(pp);
    Some(pin)
}

// ---- A.1 — MEDIASUBTYPE × FOURCC × FORMAT × VIH probe ----------------

#[test]
fn fourcc_subtype_helper_round_trips_mp43() {
    let g = fourcc_subtype(b"MP43");
    // Should equal MEDIASUBTYPE_MP43 = {3334504D-0000-0010-8000-00AA00389B71}.
    assert_eq!(g.data1, 0x3334_504D);
    assert_eq!(g.data2, 0);
    assert_eq!(g.data3, 0x0010);
    assert_eq!(g.data4, [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]);
}

/// One row of the probe matrix: `(label, fourcc_bytes,
/// format_kind)`.
struct ProbeRow {
    label: &'static str,
    fourcc: [u8; 4],
    format: FormatKind,
}

const PROBE_ROWS: &[ProbeRow] = &[
    ProbeRow {
        label: "MP43+VIH1",
        fourcc: *b"MP43",
        format: FormatKind::VIH1,
    },
    ProbeRow {
        label: "mp43+VIH1",
        fourcc: *b"mp43",
        format: FormatKind::VIH1,
    },
    ProbeRow {
        label: "MP4S+VIH1",
        fourcc: *b"MP4S",
        format: FormatKind::VIH1,
    },
    ProbeRow {
        label: "mp4s+VIH1",
        fourcc: *b"mp4s",
        format: FormatKind::VIH1,
    },
    ProbeRow {
        label: "MPG4+VIH1",
        fourcc: *b"MPG4",
        format: FormatKind::VIH1,
    },
    ProbeRow {
        label: "MP42+VIH1",
        fourcc: *b"MP42",
        format: FormatKind::VIH1,
    },
    ProbeRow {
        label: "DIV3+VIH1",
        fourcc: *b"DIV3",
        format: FormatKind::VIH1,
    },
    ProbeRow {
        label: "DIVX+VIH1",
        fourcc: *b"DIVX",
        format: FormatKind::VIH1,
    },
    ProbeRow {
        label: "DX50+VIH1",
        fourcc: *b"DX50",
        format: FormatKind::VIH1,
    },
    ProbeRow {
        label: "MP43+VIH2",
        fourcc: *b"MP43",
        format: FormatKind::VIH2,
    },
    ProbeRow {
        label: "MP4S+VIH2",
        fourcc: *b"MP4S",
        format: FormatKind::VIH2,
    },
    ProbeRow {
        label: "MPG4+VIH2",
        fourcc: *b"MPG4",
        format: FormatKind::VIH2,
    },
];

#[test]
fn round27_probe_matrix_against_mpg4ds32() {
    let (mut sb, _img, _factory, clsid) = match drive_dll_get_class_object("MPG4DS32.AX") {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round27 A.1 skipped: {e}");
            return;
        }
    };
    let filter = match sb.co_create_instance(clsid, IID_IBASEFILTER) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("round27 A.1 skipped: CreateInstance: {e}");
            return;
        }
    };
    assert!(vtable_is_plausible(&sb.mmu, filter));

    let pin = match first_input_pin(&mut sb, filter) {
        Some(p) => p,
        None => {
            let _ = sb.com_release(filter);
            eprintln!("round27 A.1 skipped: no input pin");
            return;
        }
    };
    eprintln!("round27 A.1: IPin = {pin:#010x}");

    use oxideav_vfw::com::call::call_method;
    let mut results = Vec::new();
    for row in PROBE_ROWS {
        let subtype = fourcc_subtype(&row.fourcc);
        let amt = match stage_am_media_type(&mut sb, subtype, row.fourcc, 320, 240, row.format) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("round27 A.1: stage {} failed: {e}", row.label);
                continue;
            }
        };
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            pin,
            4, // SLOT_PIN_RECEIVE_CONNECTION
            &[pin, amt],
        );
        results.push((row.label, r.clone()));
        eprintln!("round27 A.1: ReceiveConnection({}) → {r:?}", row.label);
    }
    eprintln!("--- round27 A.1 probe matrix summary ---");
    for (label, r) in &results {
        eprintln!("  {label:<14} → {r:?}");
    }
    // Tear down.
    let _ = sb.com_release(pin);
    let _ = sb.com_release(filter);
}

// ---- A.2 — IFilterGraph host stub + JoinFilterGraph ------------------

#[test]
fn host_filter_graph_mints_with_plausible_vtable() {
    let mut sb = Sandbox::new();
    let g = sb.mint_host_filter_graph().expect("mint host graph");
    // Sanity: vtable looks plausible (3 readable function pointers
    // at slots 0..3).
    assert!(vtable_is_plausible(&sb.mmu, g));
}

#[test]
fn round27_join_filter_graph_then_probe_against_mpg4ds32() {
    let (mut sb, _img, _factory, clsid) = match drive_dll_get_class_object("MPG4DS32.AX") {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round27 A.2 skipped: {e}");
            return;
        }
    };
    let filter = match sb.co_create_instance(clsid, IID_IBASEFILTER) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("round27 A.2 skipped: CreateInstance: {e}");
            return;
        }
    };
    assert!(vtable_is_plausible(&sb.mmu, filter));

    // Mint host IFilterGraph + drive IBaseFilter::JoinFilterGraph.
    let host_graph = sb.mint_host_filter_graph().expect("mint host graph");
    eprintln!("round27 A.2: host_graph at {host_graph:#010x}");

    use oxideav_vfw::com::call::call_method;
    let join_r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_JOIN_FILTER_GRAPH,
        &[host_graph, 0],
    );
    eprintln!("round27 A.2: JoinFilterGraph(host_graph, NULL) → {join_r:?}");

    let pin = match first_input_pin(&mut sb, filter) {
        Some(p) => p,
        None => {
            let _ = sb.com_release(filter);
            return;
        }
    };
    eprintln!("round27 A.2: IPin = {pin:#010x}");

    // Replay the round-26 default attempt: MP43 + VIH1.
    let subtype = fourcc_subtype(b"MP43");
    let amt = stage_am_media_type(&mut sb, subtype, *b"MP43", 320, 240, FormatKind::VIH1)
        .expect("stage AM_MEDIA_TYPE");

    // First with NULL pConnector (graph-aware path may now
    // accept).
    let r_null = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        4,
        &[0, amt],
    );
    eprintln!("round27 A.2: ReceiveConnection(NULL, MP43 VIH1) → {r_null:?}");

    // Then with self-pin (legal under graph-aware contract iff
    // codec doesn't probe pConnector's pin direction).
    let r_self = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        4,
        &[pin, amt],
    );
    eprintln!("round27 A.2: ReceiveConnection(self, MP43 VIH1) → {r_self:?}");

    // Walk the full probe matrix again under graph-aware mode so
    // the round-27 trace covers every combo.
    let mut a2_results = Vec::new();
    for row in PROBE_ROWS {
        let subtype = fourcc_subtype(&row.fourcc);
        let amt = match stage_am_media_type(&mut sb, subtype, row.fourcc, 320, 240, row.format) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("round27 A.2: stage {} failed: {e}", row.label);
                continue;
            }
        };
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            pin,
            4,
            &[pin, amt],
        );
        a2_results.push((row.label, r.clone()));
        eprintln!("round27 A.2: ReceiveConnection({}) → {r:?}", row.label);
    }
    eprintln!("--- round27 A.2 probe matrix summary (post-JoinFilterGraph) ---");
    for (label, r) in &a2_results {
        eprintln!("  {label:<14} → {r:?}");
    }

    let _ = sb.com_release(pin);
    let _ = sb.com_release(filter);
}

// ---- A.2/B — HostIPin output-pin handshake ---------------------------

#[test]
fn round27_receive_connection_with_host_output_pin() {
    let (mut sb, _img, _factory, clsid) = match drive_dll_get_class_object("MPG4DS32.AX") {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round27 host-pin skipped: {e}");
            return;
        }
    };
    let filter = match sb.co_create_instance(clsid, IID_IBASEFILTER) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("round27 host-pin skipped: CreateInstance: {e}");
            return;
        }
    };

    // JoinFilterGraph first.
    let host_graph = sb.mint_host_filter_graph().expect("mint host graph");
    use oxideav_vfw::com::call::call_method;
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_JOIN_FILTER_GRAPH,
        &[host_graph, 0],
    );

    let pin = match first_input_pin(&mut sb, filter) {
        Some(p) => p,
        None => {
            let _ = sb.com_release(filter);
            return;
        }
    };

    let subtype = fourcc_subtype(b"MP43");
    let amt = stage_am_media_type(&mut sb, subtype, *b"MP43", 320, 240, FormatKind::VIH1)
        .expect("stage AM_MEDIA_TYPE");
    let host_pin = sb.mint_host_output_pin(amt).expect("mint host output pin");
    eprintln!("round27 host-pin: host output pin at {host_pin:#010x}");
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        4,
        &[host_pin, amt],
    );
    eprintln!("round27 host-pin: ReceiveConnection(host_pin, MP43 VIH1) → {r:?}");

    // Sweep all FOURCCs once more, this time with a host output
    // pin advertising each in turn.
    let mut sweep = Vec::new();
    for row in PROBE_ROWS {
        let subtype = fourcc_subtype(&row.fourcc);
        let amt = match stage_am_media_type(&mut sb, subtype, row.fourcc, 320, 240, row.format) {
            Ok(a) => a,
            Err(_) => continue,
        };
        let host_pin = match sb.mint_host_output_pin(amt) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            pin,
            4,
            &[host_pin, amt],
        );
        sweep.push((row.label, r.clone()));
        eprintln!("round27 host-pin: ReceiveConnection({}) → {r:?}", row.label);
    }
    eprintln!("--- round27 A.2/B host-pin sweep summary ---");
    for (label, r) in &sweep {
        eprintln!("  {label:<14} → {r:?}");
    }

    let _ = sb.com_release(pin);
    let _ = sb.com_release(filter);
}

// ---- B (stretch) — IMemInputPin handshake after ReceiveConnection S_OK

#[test]
fn round27_meminputpin_receive_after_host_pin_connection() {
    let (mut sb, _img, _factory, clsid) = match drive_dll_get_class_object("MPG4DS32.AX") {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round27 B skipped: {e}");
            return;
        }
    };
    let filter = match sb.co_create_instance(clsid, IID_IBASEFILTER) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("round27 B skipped: CreateInstance: {e}");
            return;
        }
    };

    let host_graph = sb.mint_host_filter_graph().expect("mint host graph");
    use oxideav_vfw::com::call::call_method;
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        oxideav_vfw::com::SLOT_BASEFILTER_JOIN_FILTER_GRAPH,
        &[host_graph, 0],
    );

    let pin = match first_input_pin(&mut sb, filter) {
        Some(p) => p,
        None => {
            let _ = sb.com_release(filter);
            return;
        }
    };

    let subtype = fourcc_subtype(b"MP43");
    let amt = stage_am_media_type(&mut sb, subtype, *b"MP43", 320, 240, FormatKind::VIH1)
        .expect("stage AM_MEDIA_TYPE");
    let host_pin = sb.mint_host_output_pin(amt).expect("mint host output pin");
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        4,
        &[host_pin, amt],
    );
    eprintln!("round27 B: ReceiveConnection → {r:?}");
    if !matches!(r, Ok(0)) {
        eprintln!("round27 B: connection did not bind; skipping IMemInputPin probe");
        let _ = sb.com_release(pin);
        let _ = sb.com_release(filter);
        return;
    }
    eprintln!("round27 B: input pin connected via host output pin");

    // Probe IMemInputPin via QueryInterface(IID_IMemInputPin).
    use oxideav_vfw::IID_IMEMINPUTPIN;
    let mip = match sb.query_interface(pin, IID_IMEMINPUTPIN) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("round27 B: QI IMemInputPin failed: {e}");
            let _ = sb.com_release(pin);
            let _ = sb.com_release(filter);
            return;
        }
    };
    eprintln!("round27 B: IMemInputPin at {mip:#010x}");

    // Snapshot trace_ring before any guest activity that might
    // advance the codec — sub-goal B's "internal state" probe.
    sb.cpu.enable_trace_ring(64);

    // GetAllocator(IMemAllocator** ppAllocator) — slot 3.
    let alloc_slot = sb.host.arena_alloc(8).unwrap();
    sb.mmu.write_initializer(alloc_slot, &[0u8; 8]).unwrap();
    let r_ga = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        3,
        &[alloc_slot],
    );
    let p_alloc = sb.mmu.load32(alloc_slot).unwrap_or(0);
    eprintln!("round27 B: IMemInputPin::GetAllocator → {r_ga:?}, ppAllocator = {p_alloc:#010x}");

    // GetAllocatorRequirements — slot 5; tell us what allocator
    // properties the codec wants.
    let props = sb.host.arena_alloc(16).unwrap();
    sb.mmu.write_initializer(props, &[0u8; 16]).unwrap();
    let r_gar = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        5,
        &[props],
    );
    let cbuffers = sb.mmu.load32(props).unwrap_or(0);
    let cb_buffer = sb.mmu.load32(props + 4).unwrap_or(0);
    let cb_align = sb.mmu.load32(props + 8).unwrap_or(0);
    let cb_prefix = sb.mmu.load32(props + 12).unwrap_or(0);
    eprintln!(
        "round27 B: GetAllocatorRequirements → {r_gar:?}, cBuffers={cbuffers}, \
         cbBuffer={cb_buffer}, cbAlign={cb_align}, cbPrefix={cb_prefix}"
    );

    // ReceiveCanBlock — slot 8.
    let r_rcb = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        mip,
        8,
        &[],
    );
    eprintln!("round27 B: IMemInputPin::ReceiveCanBlock → {r_rcb:?}");

    // Trace ring snapshot — proxy for "codec internal state
    // advance".  Round 28 mines this for the next probe target.
    let ring = sb.cpu.trace_ring.clone();
    eprintln!(
        "round27 B: trace_ring captured {} EIPs while exercising IMemInputPin",
        ring.len()
    );
    if !ring.is_empty() {
        let head = ring.iter().take(8).collect::<Vec<_>>();
        let tail = ring.iter().rev().take(8).collect::<Vec<_>>();
        eprintln!("  head: {head:#010x?}");
        eprintln!("  tail: {tail:#010x?}");
    }

    let _ = sb.com_release(mip);
    let _ = sb.com_release(pin);
    let _ = sb.com_release(filter);
}

// ---- Probe: codec's own EnumMediaTypes — what does it offer? --------

#[test]
fn round27_codec_enum_media_types_and_query_accept() {
    let (mut sb, _img, _factory, clsid) = match drive_dll_get_class_object("MPG4DS32.AX") {
        Ok(o) => o,
        Err(e) => {
            eprintln!("round27 enum-media skipped: {e}");
            return;
        }
    };
    let filter = match sb.co_create_instance(clsid, IID_IBASEFILTER) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("round27 enum-media skipped: CreateInstance: {e}");
            return;
        }
    };
    let pin = match first_input_pin(&mut sb, filter) {
        Some(p) => p,
        None => {
            let _ = sb.com_release(filter);
            return;
        }
    };
    eprintln!("round27 enum-media: IPin = {pin:#010x}");

    use oxideav_vfw::com::call::call_method;
    // IPin::EnumMediaTypes(IEnumMediaTypes** ppEnum) — slot 12.
    let scratch = sb.host.arena_alloc(8).unwrap();
    sb.mmu.write_initializer(scratch, &[0u8; 8]).unwrap();
    let r_enum = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        12,
        &[scratch],
    );
    let p_enum = sb.mmu.load32(scratch).unwrap_or(0);
    eprintln!("round27 enum-media: EnumMediaTypes → {r_enum:?}, ppEnum = {p_enum:#010x}");
    if let Ok(0) = r_enum {
        if p_enum != 0 {
            sb.host.com.intern(p_enum, None);
            // Walk IEnumMediaTypes::Next(1, &out_amt, &fetched).
            let amt_slot = sb.host.arena_alloc(8).unwrap();
            for i in 0..6 {
                sb.mmu.write_initializer(amt_slot, &[0u8; 8]).unwrap();
                let r_next = call_method(
                    &mut sb.cpu,
                    &mut sb.mmu,
                    &sb.registry,
                    &mut sb.host,
                    p_enum,
                    3,
                    &[1, amt_slot, amt_slot + 4],
                );
                let amt = sb.mmu.load32(amt_slot).unwrap_or(0);
                let fetched = sb.mmu.load32(amt_slot + 4).unwrap_or(0);
                eprintln!(
                    "round27 enum-media[{i}]: Next → {r_next:?}, amt = {amt:#010x}, fetched = {fetched}"
                );
                if !matches!(r_next, Ok(0)) || amt == 0 {
                    break;
                }
                // Decode the AMT it gave us.
                if let Ok(major) = Guid::load(&sb.mmu, amt) {
                    if let Ok(sub) = Guid::load(&sb.mmu, amt + 16) {
                        eprintln!(
                            "  major={major}, subtype={sub} \
                             (FOURCC = {:?})",
                            String::from_utf8_lossy(&sub.data1.to_le_bytes())
                        );
                    }
                }
                let cb_format = sb.mmu.load32(amt + 64).unwrap_or(0);
                let pb_format = sb.mmu.load32(amt + 68).unwrap_or(0);
                eprintln!("  cbFormat={cb_format} pbFormat={pb_format:#010x}");
                if pb_format != 0 && cb_format >= 88 {
                    // Read biCompression at offset 48+16 = 64 from VIH.
                    let bicompression = sb.mmu.load32(pb_format + 48 + 16).unwrap_or(0);
                    eprintln!(
                        "  biCompression={bicompression:#010x} ({:?})",
                        String::from_utf8_lossy(&bicompression.to_le_bytes())
                    );
                }
            }
            let _ = sb.com_release(p_enum);
        }
    }

    // Try IPin::QueryAccept(amt) — slot 11. Faster than
    // ReceiveConnection because it doesn't actually try to bind.
    let subtype = fourcc_subtype(b"MP43");
    let amt = stage_am_media_type(&mut sb, subtype, *b"MP43", 320, 240, FormatKind::VIH1)
        .expect("stage AM_MEDIA_TYPE");
    let r_qa = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pin,
        11,
        &[amt],
    );
    eprintln!("round27 enum-media: QueryAccept(MP43 VIH1) → {r_qa:?}");

    let _ = sb.com_release(pin);
    let _ = sb.com_release(filter);
}

// ---- Sanity test: registry has the host-COM thunks. ------------------

#[test]
fn host_filter_graph_thunks_registered_under_register_all() {
    use oxideav_vfw::win32::Registry;
    let mut r = Registry::new();
    r.register_all();
    for name in [
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
    ] {
        assert!(
            r.resolve("host-com.host", name).is_some(),
            "host-com {name} thunk missing — register_all gap"
        );
    }
}
