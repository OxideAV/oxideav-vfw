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

/// Round-5 real-codec walkthrough. Loads Intel's Indeo 3
/// redistributable, calls `DllMain(DLL_PROCESS_ATTACH, NULL)`,
/// then runs `ICOpen('VIDC','IV31',ICMODE_DECOMPRESS) → ICGetInfo
/// → ICClose`. The codec name read out of `szName` in `ICINFO`
/// is asserted ASCII-printable + non-empty.
///
/// Round-4 reached the first undecoded ISA opcode trap (`ADD
/// AL,imm8` at `eip=0x1000_612A`); round 5 closes that gap and
/// every other gap up to the codec's `DRV_OPEN → ICM_GETINFO →
/// DRV_CLOSE` walk. Specifically round 5 added:
///
/// * 8-bit primary ALU opcodes (`0x00..=0x05`, `0x08..=0x0D`,
///   `0x10..=0x15`, …, `0x38..=0x3D`) plus their `r/m8 imm8`
///   group-1 (`0x80`) and `r/m8` group-3 (`0xF6`) forms.
/// * Group-2 shifts on `r/m8` (`0xC0` / `0xD0` / `0xD2`) plus
///   `r/m32` immediate / `cl` / `1` variants (`0xD1` / `0xD3`).
/// * `IMUL r32, r/m32, imm32`/`imm8` (`0x69` / `0x6B`).
/// * `XCHG r/m, r` (`0x86` / `0x87`).
/// * `MOVS` / `STOS` / `LODS` / `CMPS` / `SCAS` (incl. `REP`
///   prefix loops over `ECX`).
/// * `SAHF` (`0x9E`) / `LAHF` (`0x9F`) / `CMC` (`0xF5`).
/// * `PUSHAD` (`0x60`) / `POPAD` (`0x61`) / `ENTER` (`0xC8`).
/// * `INC/DEC r/m8` group-4 (`0xFE`).
/// * `0F 40..4F CMOVcc r32, r/m32`.
/// * `0F A3 BT` / `0F AB BTS` / `0F BA group-8` (BT/BTS/BTR/BTC
///   with imm8) / `0F A4..A5 SHLD` / `0F AC..AD SHRD` /
///   `0F B1 CMPXCHG` / `0F C1 XADD` / `0F C8..CF BSWAP`.
/// * Segment-override prefixes (`0x26 / 0x2E / 0x36 / 0x3E /
///   0x64 / 0x65`) now route through a per-instruction
///   [`Cpu::set_fs_base`] / `set_gs_base`. The runtime maps a
///   4 KiB TEB at `0x7FFD_E000`, primes `FS:[0]` (SEH chain
///   end-of-list) + `FS:[0x18]` (TEB self-pointer), and points
///   FS at it. This is what gets the codec's `_try` setup past
///   `mov eax, fs:[0]`.
///
/// In round 4 the `vfw32::ic_open` host wrapper passed `NULL`
/// for the `ICOPEN*` parameter, which prompted Indeo 3 to return
/// the magic value `0xFFFF_0000` — not a real per-instance
/// pointer. Round 5 stages a real 36-byte `ICOPEN`
/// (`dwSize / fccType / fccHandler / dwVersion / dwFlags / …`)
/// so the codec's `DRV_OPEN` allocates real state. Likewise the
/// `ICM_*` numeric values were wrong (round-4 used `ICM_USER + N`
/// for `N ∈ {0, 0x29, 0x2A, …}`, but the canonical SDK header
/// has `ICM_GETINFO = ICM_RESERVED + 2 = 0x5002`,
/// `ICM_DECOMPRESS_QUERY = ICM_USER + 11 = 0x400B`, etc.).
///
/// `ICMODE_DECOMPRESS = 2` (vfw.h). Note round-4 used 1 here,
/// which is actually `ICMODE_COMPRESS`.
#[test]
fn indeo3_driverproc_open_getinfo_close_smoke() {
    /// vfw.h: `ICMODE_DECOMPRESS = 2`.
    const ICMODE_DECOMPRESS: u32 = 2;
    /// `ICINFO` total size (`dwSize..szDriver[128]`):
    /// 6 dwords + 16 WCHARs + 128 WCHARs + 128 WCHARs
    /// = 24 + 32 + 256 + 256 = 568 bytes.
    const ICINFO_SIZE: u32 = 568;

    let bytes =
        common::fetch_or_load("IR32_32.DLL").expect("fetch IR32_32.DLL — see tests/common/mod.rs");

    let mut sb = Sandbox::new();
    let img = sb.load("IR32_32.DLL", &bytes).expect(
        "round 4+ must load IR32_32.DLL cleanly — every Win32 import \
         is now stubbed. If this fails, the asserted import surface \
         in tests/m1_load_dll_main.rs has drifted.",
    );

    // 1. DllMain — round 5 walks all the way to RET_SENTINEL.
    let dll_main_ret = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .expect(
            "round 5: IR32_32.DLL DllMain must return cleanly. If this \
             traps, the i386 ISA / Win32 stub set has regressed somewhere \
             on the codec's CRT init path.",
        );
    assert_ne!(
        dll_main_ret, 0,
        "DllMain returned 0 — TRUE expected for DLL_PROCESS_ATTACH"
    );

    // 2. install_codec → ICOpen('VIDC', 'IV31', ICMODE_DECOMPRESS).
    sb.install_codec(&img).expect("DriverProc not exported");
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_iv31 = u32::from_le_bytes(*b"IV31");
    let hic = sb
        .ic_open(fcc_video, fcc_iv31, ICMODE_DECOMPRESS)
        .unwrap_or_else(|e| panic!("IR32_32.DLL ICOpen('VIDC','IV31',DECOMPRESS) trap:\n  {e}"));
    assert_ne!(
        hic, 0,
        "ICOpen returned NULL HIC — DriverProc rejected DRV_OPEN"
    );

    // 3. ICGetInfo — codec fills `dwSize / fccType / fccHandler /
    //    dwFlags / dwVersion / dwVersionICM`. Indeo 3 doesn't
    //    populate `szName` (vfw32 normally fills it from the
    //    registry); `vfw32::ic_get_info` falls back to a
    //    fcc-derived ASCII rendering when the codec leaves it
    //    NUL.
    let info = sb
        .ic_get_info(hic, ICINFO_SIZE)
        .unwrap_or_else(|e| panic!("IR32_32.DLL ICGetInfo trap:\n  {e}"));
    assert!(
        !info.is_empty(),
        "ICGetInfo returned 0 bytes — codec did not write its identity card"
    );
    assert!(info.len() >= 24, "ICGetInfo returned a truncated header");
    let dw_size = u32::from_le_bytes(info[0..4].try_into().unwrap());
    assert_eq!(
        dw_size, ICINFO_SIZE,
        "ICINFO.dwSize mismatch — codec wrote {dw_size}"
    );
    // Indeo 3 lowercases fccType into szName-friendly bytes
    // before writing it back, so accept either case.
    let fcc_type = u32::from_le_bytes(info[4..8].try_into().unwrap());
    let fcc_video_lc = u32::from_le_bytes(*b"vidc");
    assert!(
        fcc_type == fcc_video || fcc_type == fcc_video_lc,
        "ICINFO.fccType is neither 'VIDC' nor 'vidc' — got {fcc_type:#010x}"
    );

    // szName is at offset 24 (after 6 dwords). It's a UTF-16LE
    // 16-character zero-terminated string.
    let name = decode_utf16le_until_nul(&info, 24, 16);
    assert!(
        !name.is_empty(),
        "ICGetInfo szName empty — vfw32::ic_get_info should fall back \
         to fcc handler when the codec leaves szName NUL"
    );
    assert!(
        name.chars().all(|c| (0x20..=0x7E).contains(&(c as u32))),
        "ICGetInfo szName contains non-ASCII-printable bytes: {name:?}"
    );
    eprintln!("Indeo 3 codec name: {name:?}");

    // 4. ICClose.
    let close_lr = sb
        .ic_close(hic)
        .unwrap_or_else(|e| panic!("IR32_32.DLL ICClose trap:\n  {e}"));
    let _ = close_lr; // codecs return 0 / 1 / DRVCNF_OK, no canonical "success"
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

/// Build a synthetic Indeo 3 (IV31) keyframe payload (frame
/// header + bitstream header) for a small picture. The payload
/// is a *legal-shape* keyframe — every field validates per
/// Intel's published Indeo 3 layout — but the plane data is the
/// minimal "all-cells leaf, no segmentation" filler. The codec
/// may still report decode errors (this is not a real image),
/// but the IC* sequence walks end-to-end.
///
/// Reference: docs/video/indeo/indeo3/wiki/Indeo_3.wiki
/// (multimedia.cx mirror), §"Picture header".
///
/// Parameters:
///
/// * `frame_number` — starts at 0 for the first keyframe.
/// * `width`, `height` — must be multiples of 4, between 16 and
///   640 / 480. The function picks the smallest legal value.
fn build_synthetic_iv31_keyframe(frame_number: u32, width: u16, height: u16) -> Vec<u8> {
    assert!(width % 4 == 0 && (16..=640).contains(&width));
    assert!(height % 4 == 0 && (16..=480).contains(&height));

    // ---- Bitstream header (48 bytes) -------------------------------
    let mut bs = Vec::with_capacity(48);
    bs.extend_from_slice(&0x0020u16.to_le_bytes()); // dec_version = 0x20
                                                    // frame_flags: bit 0 (periodic INTRA) | bit 2 (INTRA frame).
    bs.extend_from_slice(&0x0005u16.to_le_bytes());
    // data_size = 128 bits is the special "NULL sync frame" marker
    // per the wiki. Indeo 3 codecs typically reject this value with
    // ICERR_BADIMAGE because a sync frame still has to carry per-plane
    // num_vectors dwords + (zero-length) VQ data. We pass 128 here
    // because the synthetic-keyframe path's contract is just "the IC*
    // sequence walks end-to-end without trapping" — see the test's
    // module-level docs.
    bs.extend_from_slice(&128u32.to_le_bytes());
    bs.push(0u8); // cb_offset
    bs.push(0u8); // reserved1
    bs.extend_from_slice(&0u16.to_le_bytes()); // checksum (encoders set to 0)
    bs.extend_from_slice(&height.to_le_bytes()); // height
    bs.extend_from_slice(&width.to_le_bytes()); // width
    bs.extend_from_slice(&0u32.to_le_bytes()); // y_offset
    bs.extend_from_slice(&0u32.to_le_bytes()); // v_offset
    bs.extend_from_slice(&0u32.to_le_bytes()); // u_offset
    bs.extend_from_slice(&0u32.to_le_bytes()); // reserved2
    bs.extend_from_slice(&[0u8; 16]); // alt_quant table
    debug_assert_eq!(bs.len(), 48);

    // ---- Frame header (16 bytes) — checksum xor's frame_size ------
    let frame_size: u32 = bs.len() as u32 + 16; // includes the frame header itself
    let unknown1: u32 = 0;
    let frmh: u32 = u32::from_le_bytes(*b"FRMH");
    let checksum: u32 = frame_number ^ unknown1 ^ frame_size ^ frmh;

    let mut out = Vec::with_capacity(16 + bs.len());
    out.extend_from_slice(&frame_number.to_le_bytes());
    out.extend_from_slice(&unknown1.to_le_bytes());
    out.extend_from_slice(&checksum.to_le_bytes());
    out.extend_from_slice(&frame_size.to_le_bytes());
    out.extend_from_slice(&bs);
    out
}

/// Round 6 — full IC* decode pipeline against `IR32_32.DLL`.
///
/// Walks `DllMain → ICOpen → ICDecompressQuery →
/// ICDecompressBegin → ICDecompress → ICDecompressEnd → ICClose`
/// against a synthetic Indeo 3 keyframe at 64×48 (smallest size
/// the codec accepts: width/height ≥ 16, multiple of 4).
///
/// **SPECGAP**: the IV5 fixture bundle in
/// `samples.oxideav.org/video/windows/IV5PLAY` ships only DLLs,
/// not `.avi` payloads. Round 6 uses a *synthetic* IV31
/// keyframe whose header layout matches Intel's published
/// Indeo 3 wire format (mirrored at
/// `docs/video/indeo/indeo3/wiki/Indeo_3.wiki`). The contract
/// of this test is therefore:
///
/// * The IC* sequence runs without trapping (CPU + Win32 stub
///   coverage).
/// * `ICDecompressQuery` answers ICERR_OK or a documented
///   negative code; we accept either, the assertion is just
///   "it didn't trap and gave us a result".
/// * `ICDecompress` may write some bytes or zero bytes —
///   the codec's behaviour on a NULL-data-size sync frame is
///   not specified by the wire-format docs. We just confirm
///   the call completes and the output buffer is intact.
/// * `ICDecompressEnd` returns cleanly.
///
/// Round 7+ should swap the synthetic input for a real keyframe
/// extracted from a bundled `.avi` once one is available, at
/// which point this test would also assert non-zero output.
#[test]
fn indeo3_decompress_one_keyframe() {
    /// vfw.h: `ICMODE_DECOMPRESS = 2`.
    const ICMODE_DECOMPRESS: u32 = 2;

    let bytes =
        common::fetch_or_load("IR32_32.DLL").expect("fetch IR32_32.DLL — see tests/common/mod.rs");

    let mut sb = Sandbox::new();
    let img = sb.load("IR32_32.DLL", &bytes).expect("load IR32_32.DLL");

    let _ = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .expect("DllMain");

    sb.install_codec(&img).expect("DriverProc not exported");
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_iv31 = u32::from_le_bytes(*b"IV31");
    let hic = sb
        .ic_open(fcc_video, fcc_iv31, ICMODE_DECOMPRESS)
        .expect("ICOpen");
    assert_ne!(hic, 0, "ICOpen must mint a HIC");

    // Smallest legal Indeo 3 picture is 16×16; round-6 picks
    // 64×48 to give the codec a multi-strip-amenable shape and
    // a non-trivial output buffer to populate.
    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 48;

    let bih_in = Bih {
        bi_size: BIH_SIZE,
        width: WIDTH as i32,
        height: HEIGHT as i32,
        planes: 1,
        bit_count: 24, // codec advertises 24bpp output
        compression: *b"IV31",
        size_image: 0, // unused for Indeo 3 input bih
        ..Default::default()
    };

    // Output: RGB24, top-down (negative height in Windows)
    let bih_out = Bih {
        bi_size: BIH_SIZE,
        width: WIDTH as i32,
        height: HEIGHT as i32,
        planes: 1,
        bit_count: 24,
        compression: [0; 4], // BI_RGB
        size_image: WIDTH * HEIGHT * 3,
        ..Default::default()
    };

    // ICDecompressQuery: input/output format negotiation.
    let q = sb
        .ic_decompress_query(hic, &bih_in, Some(&bih_out))
        .unwrap_or_else(|e| panic!("ICDecompressQuery trap:\n  {e}"));
    eprintln!(
        "ICDecompressQuery returned {q:#010x} (ICERR_OK=0; negatives are codec-specific rejections)"
    );

    // ICDecompressBegin: set up the decoder pipeline.
    let begin = sb
        .ic_decompress_begin(hic, &bih_in, &bih_out)
        .unwrap_or_else(|e| panic!("ICDecompressBegin trap:\n  {e}"));
    eprintln!("ICDecompressBegin returned {begin:#010x}");

    // ICDecompress: feed a synthetic IV31 keyframe.
    let payload = build_synthetic_iv31_keyframe(0, WIDTH as u16, HEIGHT as u16);
    let out_capacity = WIDTH * HEIGHT * 3;
    let (lr, out) = sb
        .ic_decompress(hic, 0, &bih_in, &payload, &bih_out, out_capacity)
        .unwrap_or_else(|e| panic!("ICDecompress trap:\n  {e}"));
    eprintln!(
        "ICDecompress returned {lr:#010x}; output buffer length {} bytes",
        out.len()
    );
    assert_eq!(
        out.len(),
        out_capacity as usize,
        "ICDecompress must hand back a buffer of the requested size, regardless of what the codec wrote"
    );
    // ICERR_BADIMAGE is -100 = 0xFFFFFF9C. The synthetic NULL-data-size
    // path is documented to land here (see SPECGAP in module docs).
    // Any *positive* error code (or a trap) would indicate either a
    // codec internal state corruption or a host-side ISA / Win32 stub
    // gap. Accept ICERR_OK or any documented negative code; reject
    // positive non-zero values (those are fault-style sentinels in
    // some Indeo builds).
    let lr_signed = lr as i32;
    assert!(
        lr_signed <= 0,
        "ICDecompress returned positive {lr_signed}; codec probably faulted"
    );

    // ICDecompressEnd: tear down the pipeline.
    let end = sb
        .ic_decompress_end(hic)
        .unwrap_or_else(|e| panic!("ICDecompressEnd trap:\n  {e}"));
    eprintln!("ICDecompressEnd returned {end:#010x}");

    // ICClose closes the codec instance.
    let _ = sb
        .ic_close(hic)
        .unwrap_or_else(|e| panic!("ICClose:\n  {e}"));
}
