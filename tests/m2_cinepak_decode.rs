//! Round-2 milestone integration test: "Decode one Cinepak
//! frame".
//!
//! Two paths, parallel to round-1's `m1_load_dll_main.rs`:
//!
//! 1. **Synthesised codec.** Always runs. Builds a minimal PE32
//!    DLL whose only export is `DriverProc`, where the body is
//!    a single `mov eax, imm32 ; ret 20`. Walks the full
//!    Sandbox::install_codec → ic_open → ic_decompress_begin →
//!    ic_decompress → ic_decompress_end → ic_close pipeline,
//!    confirming buffers round-trip through guest memory and
//!    every IC* hop dispatches `DriverProc` correctly.
//!
//! 2. **Real Cinepak DLL.** Gated behind the `test-fixtures`
//!    feature. Loads `tests/fixtures/iccvid.dll` (user-staged;
//!    not in git) and an encoded Cinepak frame from
//!    `tests/fixtures/cinepak-32x32-1frame.cvid` (or any
//!    `*.cvid` payload the user stages); runs the decode and,
//!    if a `tests/fixtures/cinepak-32x32-1frame.expected.rgb`
//!    ground-truth file is present, byte-checks the output.
//!
//! With `test-fixtures` off (CI default) the staged-codec test
//! is silently elided. Within `test-fixtures` but with no DLL
//! staged, the test prints a "skipping" message and returns Ok.

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

#[cfg(feature = "test-fixtures")]
#[test]
fn staged_cinepak_decoder_produces_a_frame() {
    use std::path::PathBuf;

    let dll_path = PathBuf::from("tests/fixtures/iccvid.dll");
    if !dll_path.exists() {
        eprintln!(
            "no codec DLL staged at {} — silently skipping. \
             See tests/README.md for legitimate sources.",
            dll_path.display()
        );
        return;
    }
    // The encoded payload — name is illustrative; users may stage
    // any single-frame `.cvid` blob with a co-located `.json`
    // sidecar describing width/height. For round-2 the test
    // looks for a fixed 32×32 fixture.
    let payload_path = PathBuf::from("tests/fixtures/cinepak-32x32-1frame.cvid");
    if !payload_path.exists() {
        eprintln!(
            "no Cinepak payload at {} — DLL loads but skip-decoding. \
             See tests/README.md for how to extract a frame from an AVI.",
            payload_path.display()
        );
        return;
    }

    let dll_bytes = std::fs::read(&dll_path).expect("read iccvid.dll");
    let frame = std::fs::read(&payload_path).expect("read cinepak payload");

    let mut sb = Sandbox::new();
    let img = sb
        .load(dll_path.to_str().unwrap(), &dll_bytes)
        .expect("PE32 load");
    sb.install_codec(&img).expect("DriverProc exported");

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_cvid = u32::from_le_bytes(*b"cvid");
    let hic = sb
        .ic_open(fcc_video, fcc_cvid, 1)
        .expect("ICOpen iccvid succeeded");
    assert_ne!(hic, 0, "Cinepak open returned NULL HIC");

    let bih_in = Bih {
        bi_size: BIH_SIZE,
        width: 32,
        height: 32,
        planes: 1,
        bit_count: 24,
        compression: *b"cvid",
        size_image: frame.len() as u32,
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

    let _ = sb
        .ic_decompress_begin(hic, &bih_in, &bih_out)
        .expect("ICDecompressBegin");
    let (_lr, decoded) = sb
        .ic_decompress(hic, 0, &bih_in, &frame, &bih_out, 32 * 32 * 3)
        .expect("ICDecompress");
    let _ = sb.ic_decompress_end(hic).expect("ICDecompressEnd");
    let _ = sb.ic_close(hic).expect("ICClose");

    // If the user staged a ground-truth file, compare bytes.
    let expected_path = PathBuf::from("tests/fixtures/cinepak-32x32-1frame.expected.rgb");
    if expected_path.exists() {
        let expected = std::fs::read(&expected_path).expect("read expected.rgb");
        assert_eq!(
            decoded, expected,
            "decoded bytes do not match staged ground truth"
        );
    } else {
        eprintln!(
            "no ground-truth file at {} — decode produced {} bytes, not byte-checking.",
            expected_path.display(),
            decoded.len()
        );
    }
}
