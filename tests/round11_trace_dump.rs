//! Round-12 regression sentinel — guards the codec-init globals
//! that were the round-11 → round-12 pivot point.
//!
//! Round 11 plumbed `DRV_LOAD` + `DRV_ENABLE` through `ic_open`
//! but `[0x1009c770]` (the codec's huffman-table base pointer)
//! still came back NULL, leaving `ICDecompress` returning
//! `ICERR_BADIMAGE` (-100). Round 12 implemented
//! `kernel32!FindResourceA` / `LoadResource` / `LockResource`
//! against the loaded PE's resource directory, plus
//! `CreateFileMappingA` / `MapViewOfFile` returning real
//! buffers, which let `IR50_32.DLL`'s `DRV_LOAD` chain copy
//! the tables out of `RT_BITMAP/112` and `RT_BITMAP/113` and
//! flip the init guard.
//!
//! This test asserts the post-`ICOpen` shape:
//!
//! * `[0x10084790]` (init guard) is incremented from 0 to 1.
//! * `[0x1009c770]` (huffman-table allocation base) is non-NULL.
//! * `[0x100847a0]` (codec-internal frame height) is 0x78 (=120,
//!   the bitmap-resource's BiHeight).
//! * `[0x10084798]` (codec-internal frame width) is 0xa0 (=160,
//!   the bitmap-resource's BiWidth aligned).
//!
//! …and then asserts the end-to-end decode succeeds with
//! `ICERR_OK` and a non-empty output. If any of these regress
//! we want a clearly-named failing test, not a confusing -100
//! cascade in `round8_iv50_decode`.

mod common;

use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};
use oxideav_vfw::Sandbox;

#[test]
fn cat_attack_first_keyframe_post_init_globals_and_decode() {
    const ICMODE_DECOMPRESS: u32 = 2;

    let dll_bytes = common::fetch_or_load("IR50_32.DLL").expect("fetch IR50_32.DLL");
    let avi = common::fetch_or_load_ffmpeg_sample("IV50", "cat_attack.avi")
        .expect("fetch cat_attack.avi");
    let sample = common::avi_extractor::extract_first_video_sample(&avi).expect("AVI walker");
    let payload = &sample.bytes;
    let width: u32 = sample.width;
    let height: u32 = sample.height;

    let mut sb = Sandbox::new();
    let img = sb.load("IR50_32.DLL", &dll_bytes).expect("load");
    let _ = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .expect("DllMain");
    sb.install_codec(&img).expect("DriverProc");

    // Pre-state: every codec global is zero (the PE image's
    // .data section was zeroed at load-time).
    assert_eq!(sb.mmu.load32(0x10084790).unwrap(), 0);
    assert_eq!(sb.mmu.load32(0x1009c770).unwrap(), 0);

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"IV50");
    let hic = sb
        .ic_open(fcc_video, fcc_handler, ICMODE_DECOMPRESS)
        .expect("ICOpen");
    assert_ne!(hic, 0);

    // Post-ICOpen: DRV_LOAD's init chain populated the table
    // globals.
    let init_guard = sb.mmu.load32(0x10084790).unwrap();
    let alloc_base = sb.mmu.load32(0x1009c770).unwrap();
    let h_global = sb.mmu.load32(0x100847a0).unwrap();
    let w_global = sb.mmu.load32(0x10084798).unwrap();
    assert_eq!(
        init_guard, 1,
        "post-ICOpen [0x10084790] should be 1 (DRV_LOAD's table-init guard); got {init_guard:#010x}"
    );
    assert_ne!(
        alloc_base, 0,
        "post-ICOpen [0x1009c770] should hold a real allocation; got NULL"
    );
    // The huffman / inverse-DCT bitmap is 160×120 8bpp; codec
    // stores the dims (after BIH-style aligning) in these
    // globals.
    assert_eq!(
        h_global, 0x78,
        "post-ICOpen [0x100847a0] (frame height) should be 0x78 = 120; got {h_global:#x}"
    );
    assert_eq!(
        w_global, 0xa0,
        "post-ICOpen [0x10084798] (frame width) should be 0xa0 = 160; got {w_global:#x}"
    );

    let bih_in = Bih {
        bi_size: BIH_SIZE,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: *b"IV50",
        size_image: payload.len() as u32,
        ..Default::default()
    };
    let bih_out = Bih {
        bi_size: BIH_SIZE,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: [0; 4],
        size_image: width * height * 3,
        ..Default::default()
    };
    let q = sb
        .ic_decompress_query(hic, &bih_in, Some(&bih_out))
        .expect("Query");
    assert_eq!(q, 0, "ICDecompressQuery should return ICERR_OK; got {q:#x}");
    let b = sb
        .ic_decompress_begin(hic, &bih_in, &bih_out)
        .expect("Begin");
    assert_eq!(b, 0, "ICDecompressBegin should return ICERR_OK; got {b:#x}");
    sb.cpu.set_instr_limit(200_000_000);
    let out_capacity = width * height * 3;
    let (lr, out) = sb
        .ic_decompress(hic, 0, &bih_in, payload, &bih_out, out_capacity)
        .expect("ICDecompress");
    assert_eq!(
        lr, 0,
        "round-12 milestone: ICDecompress should return ICERR_OK (0); got {} (signed {})",
        lr, lr as i32
    );
    let nonzero = out.iter().filter(|&&b| b != 0).count();
    assert!(
        nonzero > 0,
        "ICDecompress reported success but produced an all-zero \
         output buffer ({} bytes)",
        out.len()
    );
}
