//! Round-2 + round-3 milestone integration tests for the
//! `vfw32` IC* surface.
//!
//! Round 2 shipped the synthetic-codec pipeline: a hand-rolled
//! PE32 DLL whose `DriverProc` returns `mov eax, imm32 ; ret 20`
//! across the full `IC*` walkthrough. That coverage is preserved
//! below in [`synth_codec_walks_full_ic_pipeline`] — it confirms
//! buffer marshalling, HIC lifecycle, and re-entrant
//! `call_guest` plumbing without depending on a real codec.
//!
//! Round 3 adds a real-codec smoke test against Intel's Indeo 3
//! redistributable (`IR32_32.DLL`, fcc_handler `IV31`), using the
//! [`common::fetch_or_load`] helper to locate the bytes. The test
//! walks `DllMain → ICOpen → ICGetInfo → ICClose` and asserts
//! the codec name read back from `ICGetInfo` is non-empty +
//! ASCII-printable. NO frame decode — the IV5 bundle has DLLs
//! but no `.avi` payloads; encoded-frame coverage waits for
//! round 4.
//!
//! If the Indeo 3 walkthrough trips a trap during `ICOpen` /
//! `ICGetInfo` / `ICClose`, the trap variant + the last EIP point
//! at exactly which `vfw32` / `kernel32` stub or ISA opcode round
//! 4 needs to add. The failure-mode message in the test panic is
//! the round-4 todo list.

mod common;

use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE, ICDECOMPRESS_SIZE};
use oxideav_vfw::Sandbox;

/// Build a synthetic codec DLL whose `DriverProc` is just
/// `mov eax, ret_value ; ret 20`. We hand-roll a full PE32 with
/// `DriverProc` exported — based on the round-1
/// `pe::test_image::build_minimal_dll` layout.
fn build_canned_codec_dll(driver_proc_ret: u32) -> Vec<u8> {
    use oxideav_vfw::pe::header::{
        IMAGE_DIRECTORY_ENTRY_BASERELOC, IMAGE_DIRECTORY_ENTRY_EXPORT,
        IMAGE_DIRECTORY_ENTRY_IMPORT, IMAGE_DOS_SIGNATURE, IMAGE_FILE_MACHINE_I386,
        IMAGE_NT_OPTIONAL_HDR32_MAGIC, IMAGE_NT_SIGNATURE, IMAGE_SCN_MEM_EXECUTE,
        IMAGE_SCN_MEM_READ, IMAGE_SCN_MEM_WRITE,
    };
    const FILE_ALIGN: usize = 0x200;
    const SECTION_ALIGN: u32 = 0x1000;
    const IMAGE_BASE: u32 = 0x1000_0000;

    let mut bytes = vec![0u8; FILE_ALIGN];
    bytes[0..2].copy_from_slice(&IMAGE_DOS_SIGNATURE.to_le_bytes());
    let pe_off: u32 = 0x40;
    bytes[0x3C..0x40].copy_from_slice(&pe_off.to_le_bytes());
    let pe = pe_off as usize;
    bytes[pe..pe + 4].copy_from_slice(&IMAGE_NT_SIGNATURE.to_le_bytes());

    let fh = pe + 4;
    bytes[fh..fh + 2].copy_from_slice(&IMAGE_FILE_MACHINE_I386.to_le_bytes());
    bytes[fh + 2..fh + 4].copy_from_slice(&3u16.to_le_bytes());
    bytes[fh + 16..fh + 18].copy_from_slice(&224u16.to_le_bytes());
    bytes[fh + 18..fh + 20].copy_from_slice(&0x2000u16.to_le_bytes());

    let oh = fh + 20;
    bytes[oh..oh + 2].copy_from_slice(&IMAGE_NT_OPTIONAL_HDR32_MAGIC.to_le_bytes());
    bytes[oh + 4..oh + 8].copy_from_slice(&(FILE_ALIGN as u32).to_le_bytes());
    bytes[oh + 8..oh + 12].copy_from_slice(&(FILE_ALIGN as u32).to_le_bytes());
    bytes[oh + 16..oh + 20].copy_from_slice(&0x1000u32.to_le_bytes()); // entry RVA
    bytes[oh + 20..oh + 24].copy_from_slice(&0x1000u32.to_le_bytes());
    bytes[oh + 24..oh + 28].copy_from_slice(&0x2000u32.to_le_bytes());
    bytes[oh + 28..oh + 32].copy_from_slice(&IMAGE_BASE.to_le_bytes());
    bytes[oh + 32..oh + 36].copy_from_slice(&SECTION_ALIGN.to_le_bytes());
    bytes[oh + 36..oh + 40].copy_from_slice(&(FILE_ALIGN as u32).to_le_bytes());
    bytes[oh + 40..oh + 42].copy_from_slice(&4u16.to_le_bytes());
    bytes[oh + 48..oh + 50].copy_from_slice(&4u16.to_le_bytes());
    bytes[oh + 56..oh + 60].copy_from_slice(&0x5000u32.to_le_bytes());
    bytes[oh + 60..oh + 64].copy_from_slice(&(FILE_ALIGN as u32).to_le_bytes());
    bytes[oh + 68..oh + 70].copy_from_slice(&3u16.to_le_bytes());
    bytes[oh + 72..oh + 76].copy_from_slice(&0x10_0000u32.to_le_bytes());
    bytes[oh + 76..oh + 80].copy_from_slice(&0x1000u32.to_le_bytes());
    bytes[oh + 80..oh + 84].copy_from_slice(&0x10_0000u32.to_le_bytes());
    bytes[oh + 84..oh + 88].copy_from_slice(&0x1000u32.to_le_bytes());
    bytes[oh + 92..oh + 96].copy_from_slice(&16u32.to_le_bytes());

    let dirs = oh + 96;
    write_dir(
        &mut bytes,
        dirs,
        IMAGE_DIRECTORY_ENTRY_EXPORT,
        0x2000,
        0x100,
    );
    write_dir(&mut bytes, dirs, IMAGE_DIRECTORY_ENTRY_IMPORT, 0x2100, 40);
    write_dir(
        &mut bytes,
        dirs,
        IMAGE_DIRECTORY_ENTRY_BASERELOC,
        0x4000,
        12,
    );

    let st = dirs + 16 * 8;
    write_section(
        &mut bytes,
        st,
        b".text",
        0x10,
        0x1000,
        FILE_ALIGN as u32,
        FILE_ALIGN as u32,
        IMAGE_SCN_MEM_READ | IMAGE_SCN_MEM_EXECUTE,
    );
    write_section(
        &mut bytes,
        st + 40,
        b".rdata",
        0x600,
        0x2000,
        (FILE_ALIGN * 3) as u32,
        (FILE_ALIGN * 2) as u32,
        IMAGE_SCN_MEM_READ | IMAGE_SCN_MEM_WRITE,
    );
    write_section(
        &mut bytes,
        st + 80,
        b".reloc",
        0x10,
        0x4000,
        FILE_ALIGN as u32,
        (FILE_ALIGN * 5) as u32,
        IMAGE_SCN_MEM_READ,
    );

    bytes.resize(FILE_ALIGN * 6, 0);

    // .text — DriverProc body: mov eax, imm32 ; ret 20.
    bytes[FILE_ALIGN] = 0xB8;
    bytes[FILE_ALIGN + 1..FILE_ALIGN + 5].copy_from_slice(&driver_proc_ret.to_le_bytes());
    bytes[FILE_ALIGN + 5] = 0xC2;
    bytes[FILE_ALIGN + 6..FILE_ALIGN + 8].copy_from_slice(&20u16.to_le_bytes());

    // .rdata
    let rdata = FILE_ALIGN * 2;
    let off = |rva: u32| rdata + (rva - 0x2000) as usize;

    // Export directory at RVA 0x2000.
    //   AddressOfFunctions     @ 0x2080 (4 bytes)
    //   AddressOfNames         @ 0x2084 (4 bytes)
    //   AddressOfNameOrdinals  @ 0x2088 (2 bytes)
    //   "DriverProc\0"         @ 0x208C (11 bytes; ends at 0x2097)
    //   "synth-codec.dll\0"    @ 0x20A0 (16 bytes; spaced)
    let edir = off(0x2000);
    bytes[edir + 12..edir + 16].copy_from_slice(&0x20A0u32.to_le_bytes());
    bytes[edir + 16..edir + 20].copy_from_slice(&1u32.to_le_bytes());
    bytes[edir + 20..edir + 24].copy_from_slice(&1u32.to_le_bytes());
    bytes[edir + 24..edir + 28].copy_from_slice(&1u32.to_le_bytes());
    bytes[edir + 28..edir + 32].copy_from_slice(&0x2080u32.to_le_bytes());
    bytes[edir + 32..edir + 36].copy_from_slice(&0x2084u32.to_le_bytes());
    bytes[edir + 36..edir + 40].copy_from_slice(&0x2088u32.to_le_bytes());

    let aof = off(0x2080);
    bytes[aof..aof + 4].copy_from_slice(&0x1000u32.to_le_bytes());
    let aon = off(0x2084);
    bytes[aon..aon + 4].copy_from_slice(&0x208Cu32.to_le_bytes());
    let aoo = off(0x2088);
    bytes[aoo..aoo + 2].copy_from_slice(&0u16.to_le_bytes());
    let dp_name = off(0x208C);
    let s = b"DriverProc\0";
    bytes[dp_name..dp_name + s.len()].copy_from_slice(s);
    let dll = off(0x20A0);
    let s = b"synth-codec.dll\0";
    bytes[dll..dll + s.len()].copy_from_slice(s);

    // Import descriptor: kernel32 only (so the IAT slot pattern
    // matches what a real codec exhibits).
    let imp0 = off(0x2100);
    bytes[imp0..imp0 + 4].copy_from_slice(&0x2150u32.to_le_bytes());
    bytes[imp0 + 12..imp0 + 16].copy_from_slice(&0x2400u32.to_le_bytes());
    bytes[imp0 + 16..imp0 + 20].copy_from_slice(&0x2200u32.to_le_bytes());

    let ilt = off(0x2150);
    bytes[ilt..ilt + 4].copy_from_slice(&0x2300u32.to_le_bytes());
    let iat = off(0x2200);
    bytes[iat..iat + 4].copy_from_slice(&0x2300u32.to_le_bytes());
    let ibn = off(0x2300);
    bytes[ibn + 2..ibn + 2 + b"GetProcessHeap\0".len()].copy_from_slice(b"GetProcessHeap\0");
    let kn = off(0x2400);
    bytes[kn..kn + b"kernel32.dll\0".len()].copy_from_slice(b"kernel32.dll\0");

    // .reloc — single empty block.
    let reloc = FILE_ALIGN * 5;
    bytes[reloc..reloc + 4].copy_from_slice(&0u32.to_le_bytes());
    bytes[reloc + 4..reloc + 8].copy_from_slice(&8u32.to_le_bytes());

    bytes
}

fn write_dir(bytes: &mut [u8], base: usize, idx: usize, rva: u32, size: u32) {
    let off = base + idx * 8;
    bytes[off..off + 4].copy_from_slice(&rva.to_le_bytes());
    bytes[off + 4..off + 8].copy_from_slice(&size.to_le_bytes());
}

#[allow(clippy::too_many_arguments)]
fn write_section(
    bytes: &mut [u8],
    off: usize,
    name: &[u8],
    virtual_size: u32,
    virtual_address: u32,
    size_of_raw_data: u32,
    pointer_to_raw_data: u32,
    characteristics: u32,
) {
    for (i, b) in name.iter().take(8).enumerate() {
        bytes[off + i] = *b;
    }
    bytes[off + 8..off + 12].copy_from_slice(&virtual_size.to_le_bytes());
    bytes[off + 12..off + 16].copy_from_slice(&virtual_address.to_le_bytes());
    bytes[off + 16..off + 20].copy_from_slice(&size_of_raw_data.to_le_bytes());
    bytes[off + 20..off + 24].copy_from_slice(&pointer_to_raw_data.to_le_bytes());
    bytes[off + 36..off + 40].copy_from_slice(&characteristics.to_le_bytes());
}

#[test]
fn synth_codec_walks_full_ic_pipeline() {
    let bytes = build_canned_codec_dll(0x0000_C0DE);
    let mut sb = Sandbox::new();
    let img = sb.load("synth-codec.dll", &bytes).expect("PE32 load");
    sb.install_codec(&img).expect("DriverProc found");

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"cvid");
    let hic = sb.ic_open(fcc_video, fcc_handler, 1).expect("ic_open");
    assert_ne!(hic, 0, "open should mint a HIC");

    let bih_in = Bih {
        bi_size: BIH_SIZE,
        width: 32,
        height: 32,
        planes: 1,
        bit_count: 24,
        compression: *b"cvid",
        size_image: 32 * 32 * 3 / 2,
        ..Default::default()
    };
    let bih_out = Bih {
        bi_size: BIH_SIZE,
        width: 32,
        height: 32,
        planes: 1,
        bit_count: 24,
        ..Default::default()
    };

    let r = sb
        .ic_decompress_query(hic, &bih_in, Some(&bih_out))
        .expect("query");
    assert_eq!(r, 0x0000_C0DE);

    let r = sb
        .ic_decompress_begin(hic, &bih_in, &bih_out)
        .expect("begin");
    assert_eq!(r, 0x0000_C0DE);

    let payload = vec![0u8; 64];
    let (lr, out) = sb
        .ic_decompress(hic, 0, &bih_in, &payload, &bih_out, 32 * 32 * 3)
        .expect("decompress");
    assert_eq!(lr, 0x0000_C0DE);
    assert_eq!(out.len(), 32 * 32 * 3);

    let r = sb.ic_decompress_end(hic).expect("end");
    assert_eq!(r, 0x0000_C0DE);

    let r = sb.ic_close(hic).expect("close");
    assert_eq!(r, 0x0000_C0DE);
}

#[test]
fn icdecompress_struct_size_matches_marshalling() {
    // Sanity check — guard against accidental changes to the
    // ICDECOMPRESS marshalling layout.
    assert_eq!(ICDECOMPRESS_SIZE, 24);
    assert_eq!(BIH_SIZE, 40);
}

/// Round-3 real-codec walkthrough framework. Loads Intel's Indeo
/// 3 redistributable + (when the loader can satisfy the imports)
/// runs `DllMain → ICOpen('VIDC','IV31',ICMODE_DECOMPRESS) →
/// ICGetInfo → ICClose`. The codec name read out of `szName` in
/// `ICINFO` is asserted ASCII-printable + non-empty.
///
/// **End of round 3**: `Sandbox::load` rejects the import
/// resolution step because gdi32 / user32 / winmm + 22 extra
/// kernel32 stubs are not yet implemented (see
/// `tests/m1_load_dll_main.rs::round_4_todo_imports`). The test
/// asserts on the rejection with a clear diagnostic so the
/// failure is the round-4 work plan, not a CI surprise.
///
/// **Once round 4 lands the missing stubs**, the load will
/// succeed, the `else` branch fires, and the IC* walkthrough
/// runs end-to-end. The first trap then encountered (likely an
/// ISA opcode the round-1 integer interpreter doesn't yet
/// model) becomes round 5's todo list — same bootstrap pattern.
///
/// `ICMODE_DECOMPRESS = 1` (vfw.h). The fcc_handler `IV31` is
/// Indeo 3.2's canonical 4cc.
#[test]
fn indeo3_driverproc_open_getinfo_close_smoke() {
    const ICMODE_DECOMPRESS: u32 = 1;
    /// `ICINFO` total size (`dwSize..szDriver[128]`):
    /// 6 dwords + 16 WCHARs + 128 WCHARs + 128 WCHARs
    /// = 24 + 32 + 256 + 256 = 568 bytes.
    const ICINFO_SIZE: u32 = 568;

    let bytes =
        common::fetch_or_load("IR32_32.DLL").expect("fetch IR32_32.DLL — see tests/common/mod.rs");

    let mut sb = Sandbox::new();
    match sb.load("IR32_32.DLL", &bytes) {
        Err(oxideav_vfw::Error::PeLoader(oxideav_vfw::pe::PeError::UnknownImportFunction {
            dll,
            name,
        })) => {
            // Round 3 expectation: import resolution rejects the
            // load. The first miss surfaced is one of the
            // documented round-4 todo entries.
            eprintln!(
                "round 3: IR32_32.DLL load rejected at first missing import \
                 {dll}!{name} — round-4 stub work needed (see m1_load_dll_main \
                 round_4_todo_imports for the full list)."
            );
            // Don't assert on the *specific* (dll, name) — sort
            // order in BTreeMap iteration in `imports::resolve`
            // can pick any of the missing imports first. The
            // "load failed for the right family of reason"
            // assertion is what we want.
        }
        Err(other) => {
            panic!(
                "IR32_32.DLL load failed with unexpected error \
                 (expected UnknownImportFunction at end of round 3): {other}"
            );
        }
        Ok(img) => {
            // Round 4+: imports resolved, walk the full pipeline.
            indeo3_walk_ic_pipeline(&mut sb, &img, ICMODE_DECOMPRESS, ICINFO_SIZE);
        }
    }
}

/// Round 4+ post-load walkthrough — extracted into a free
/// function so the round-3 test stays a clean "load failed for
/// the documented reason" assertion + a forward-compatible
/// post-load arm.
fn indeo3_walk_ic_pipeline(
    sb: &mut Sandbox,
    img: &oxideav_vfw::pe::Image,
    icmode_decompress: u32,
    icinfo_size: u32,
) {
    // 1. DllMain.
    if let Err(e) = sb.call_dll_main(img, oxideav_vfw::DLL_PROCESS_ATTACH) {
        panic!(
            "IR32_32.DLL DllMain trap — next-round todo:\n  {e}\n\
             (last EIP + trap variant identify which ISA opcode \
             or stub is missing)"
        );
    }

    // 2. install_codec → ICOpen('VIDC', 'IV31', ICMODE_DECOMPRESS).
    sb.install_codec(img).expect("DriverProc not exported");
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_iv31 = u32::from_le_bytes(*b"IV31");
    let hic = sb
        .ic_open(fcc_video, fcc_iv31, icmode_decompress)
        .unwrap_or_else(|e| panic!("IR32_32.DLL ICOpen('VIDC','IV31',DECOMPRESS) trap:\n  {e}"));
    assert_ne!(
        hic, 0,
        "ICOpen returned NULL HIC — DriverProc rejected DRV_OPEN"
    );

    // 3. ICGetInfo — codec writes its identity card.
    let info = sb
        .ic_get_info(hic, icinfo_size)
        .unwrap_or_else(|e| panic!("IR32_32.DLL ICGetInfo trap:\n  {e}"));
    assert!(
        !info.is_empty(),
        "ICGetInfo returned 0 bytes — codec did not write its identity card"
    );

    // szName is at offset 24 (after 6 dwords). It's a UTF-16LE
    // 16-character zero-terminated string.
    let name = decode_utf16le_until_nul(&info, 24, 16);
    assert!(
        !name.is_empty(),
        "ICGetInfo szName empty — Indeo 3 should report a codec name"
    );
    assert!(
        name.chars().all(|c| (0x20..=0x7E).contains(&(c as u32))),
        "ICGetInfo szName contains non-ASCII-printable bytes: {name:?}"
    );
    eprintln!("Indeo 3 codec name: {name:?}");

    // 4. ICClose.
    if let Err(e) = sb.ic_close(hic) {
        panic!("IR32_32.DLL ICClose trap:\n  {e}");
    }
}

/// Decode a fixed-length UTF-16LE field (`field_chars` 16-bit
/// units starting at byte `off`) into a Rust `String`, stopping
/// at the first NUL char or invalid surrogate. Used for the
/// `szName` / `szDescription` / `szDriver` fields of `ICINFO`.
fn decode_utf16le_until_nul(buf: &[u8], off: usize, field_chars: usize) -> String {
    let mut chars = Vec::with_capacity(field_chars);
    for i in 0..field_chars {
        let p = off + i * 2;
        if p + 2 > buf.len() {
            break;
        }
        let u = u16::from_le_bytes([buf[p], buf[p + 1]]);
        if u == 0 {
            break;
        }
        chars.push(u);
    }
    String::from_utf16_lossy(&chars)
}
