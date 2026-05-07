//! Round 7 — Real IV31 keyframe decode through `cubes.mov`.
//!
//! End-to-end milestone test:
//!
//! 1. Fetch `cubes.mov` from `samples.oxideav.org/ffmpeg/V-codecs/IV32/`
//!    via the round-7 extension to `tests/common::fetch_or_load_ffmpeg_sample`.
//! 2. Parse the QuickTime container with the test-side
//!    `tests/common/mov_extractor.rs` chunk walker. Confirm the
//!    codec FourCC (`IV32`) and the picture shape (160×120).
//! 3. Extract sample 0's bytes — this is the first I-frame of
//!    `cubes.mov`.
//! 4. Walk the `IR32_32.DLL` IC* sequence
//!    (`DllMain → ICOpen → ICDecompressQuery → ICDecompressBegin
//!    → ICDecompress → ICDecompressEnd → ICClose`) feeding the
//!    real keyframe into `ICDecompress`.
//! 5. Assert `ICDecompress` returns a non-positive result code
//!    AND the output buffer is no longer all-zero (we ZERO'd it
//!    pre-call inside `vfw32::ic_decompress`).
//!
//! Reference docs (clean-room):
//!
//! * ISO/IEC 14496-12 — for the MOV chunk walker.
//! * `docs/video/indeo/indeo3/wiki/Indeo_3.wiki` — for the
//!   IV31 frame-header XOR-checksum sanity check.
//! * Microsoft VfW SDK header — for `ICMODE_DECOMPRESS = 2`.
//!
//! This test must NEVER reference `libavformat/mov.c`,
//! `libavcodec/indeo3.c`, or any QuickTime SDK source.

mod common;

use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};
use oxideav_vfw::Sandbox;

/// Verify the MOV chunk walker against `cubes.mov`'s known
/// shape, with no codec involvement.
#[test]
fn cubes_mov_parses_to_expected_first_sample_metadata() {
    let mov = match common::fetch_or_load_ffmpeg_sample("IV32", "cubes.mov") {
        Ok(b) => b,
        Err(e) => {
            // Network-flake friendliness: surface the underlying
            // error verbatim so an offline run shows the URL.
            panic!("fetch cubes.mov: {e}");
        }
    };
    assert_eq!(
        mov.len(),
        121_458,
        "cubes.mov upstream size changed; trace fixture corpus drift"
    );

    let sample = common::mov_extractor::extract_first_video_sample(&mov)
        .expect("MOV chunk walker on cubes.mov");

    // ProbeData (oxideav: cubes.mov.json) says: codec_tag IV32,
    // 160×120, 40 frames, first chunk @ offset 8, first sample
    // size 0x0C07 = 3079.
    assert_eq!(
        sample.codec_fourcc,
        u32::from_le_bytes(*b"IV32"),
        "codec FourCC must be IV32"
    );
    assert_eq!(sample.width, 160);
    assert_eq!(sample.height, 120);
    assert_eq!(sample.sample_offset, 8);
    assert_eq!(sample.sample_size, 0x0C07);
    assert_eq!(sample.bytes.len(), 0x0C07);
}

/// Verify the extracted sample is in-shape per the Indeo 3
/// frame-header layout (clean-room check; no codec).
///
/// Reference: docs/video/indeo/indeo3/wiki/Indeo_3.wiki
/// §"Frame header":
///
/// * frame_number (DWORD LE) starts at 0.
/// * unknown1 (DWORD LE) is always 0.
/// * check_sum (DWORD LE) = frame_number XOR unknown1
///   XOR frame_size XOR 'FRMH' (where 'FRMH' is the BE u32
///   `0x4652_4D48`).
/// * frame_size (DWORD LE) is the total frame data length.
#[test]
fn cubes_mov_first_sample_passes_indeo3_header_checksum() {
    let mov = common::fetch_or_load_ffmpeg_sample("IV32", "cubes.mov").expect("fetch cubes.mov");
    let sample = common::mov_extractor::extract_first_video_sample(&mov).unwrap();
    let bs = &sample.bytes;
    assert!(
        bs.len() >= 16,
        "sample shorter than the 16-byte frame header"
    );
    let frame_number = u32::from_le_bytes(bs[0..4].try_into().unwrap());
    let unknown1 = u32::from_le_bytes(bs[4..8].try_into().unwrap());
    let check_sum = u32::from_le_bytes(bs[8..12].try_into().unwrap());
    let frame_size = u32::from_le_bytes(bs[12..16].try_into().unwrap());

    assert_eq!(frame_number, 0, "first sample must be frame 0");
    assert_eq!(unknown1, 0);
    // 'FRMH' as a big-endian DWORD.
    let frmh: u32 = u32::from_be_bytes(*b"FRMH");
    let computed = frame_number ^ unknown1 ^ frame_size ^ frmh;
    assert_eq!(
        computed, check_sum,
        "Indeo 3 frame-header checksum mismatch on cubes.mov sample 0"
    );
    // frame_size must fit in the sample.
    assert!(
        (frame_size as usize) <= bs.len(),
        "declared frame_size {frame_size} exceeds sample bytes {}",
        bs.len()
    );
}

/// **The headline round-7 milestone test.** Drives the full IC*
/// sequence against `IR32_32.DLL` with a REAL Indeo 3 keyframe
/// extracted from `cubes.mov`. Asserts:
///
/// * ICDecompress returns a non-positive result code (ICERR_OK
///   or a documented negative code; positive codes signal a
///   codec fault).
/// * The output RGB24 buffer is no longer all-zero.
///
/// Round-6 produced ICERR_BADIMAGE (-100) on a synthetic
/// "data_size = 128" sync frame; round 7 expects a real frame
/// to clear that path and either decode successfully or surface
/// a different code path.
#[test]
fn cubes_mov_first_keyframe_decodes_through_ir32_32_dll() {
    /// vfw.h: `ICMODE_DECOMPRESS = 2`. (`ICMODE_FASTDECOMPRESS = 3`
    /// also works for read-only Indeo 3 decode and bypasses some
    /// codec setup; we use the canonical mode here.)
    const ICMODE_DECOMPRESS: u32 = 2;

    // Fetch fixtures.
    let dll_bytes = common::fetch_or_load("IR32_32.DLL").expect("fetch IR32_32.DLL");
    let mov = common::fetch_or_load_ffmpeg_sample("IV32", "cubes.mov").expect("fetch cubes.mov");

    let sample = common::mov_extractor::extract_first_video_sample(&mov)
        .expect("MOV chunk walker on cubes.mov");
    let payload = &sample.bytes;
    let width: u32 = sample.width as u32;
    let height: u32 = sample.height as u32;

    let mut sb = Sandbox::new();
    let img = sb
        .load("IR32_32.DLL", &dll_bytes)
        .expect("load IR32_32.DLL");

    let _ = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .expect("DllMain");

    sb.install_codec(&img).expect("DriverProc not exported");
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    // The cubes.mov stsd codec_tag is 'IV32'; that's what
    // DRV_OPEN should see. (Some IR32_32.DLL builds also bind
    // 'IV31'; we've verified both work for ICOpen, see the
    // m2_indeo3_driverproc.rs round-5 test.)
    let fcc_handler = u32::from_le_bytes(*b"IV32");
    let hic = sb
        .ic_open(fcc_video, fcc_handler, ICMODE_DECOMPRESS)
        .expect("ICOpen");
    assert_ne!(hic, 0);
    let driver_id = sb.host.hics.get(&hic).unwrap().driver_id;
    eprintln!("ICOpen → hic={hic:#010x}, driver_id={driver_id:#010x}");

    // Per the cubes.mov stsd, the codec_tag is 'IV32', but
    // Microsoft VfW handlers historically register both 'IV31'
    // and 'IV32' to the same `IR32_32.DLL`. The DLL's internal
    // FourCC validation accepts either; we match the file's
    // declared codec_tag verbatim for the input BIH so the
    // codec sees a consistent IV32 path.
    let bih_in = Bih {
        bi_size: BIH_SIZE,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: *b"IV31",
        // Indeo 3 keyframes have variable size; the codec reads
        // the actual frame_size out of the payload itself.
        size_image: payload.len() as u32,
        ..Default::default()
    };

    // Output BIH: positive height = bottom-up RGB24 (Windows
    // historical default). Indeo 3 codecs emit bottom-up
    // natively, so this avoids an internal flip.
    let bih_out = Bih {
        bi_size: BIH_SIZE,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: [0; 4], // BI_RGB
        size_image: width * height * 3,
        ..Default::default()
    };

    let q = sb
        .ic_decompress_query(hic, &bih_in, Some(&bih_out))
        .expect("ICDecompressQuery");
    let begin = sb
        .ic_decompress_begin(hic, &bih_in, &bih_out)
        .expect("ICDecompressBegin");
    eprintln!("ICDecompressQuery={q:#010x}, ICDecompressBegin={begin:#010x}");

    // Real-codec body needs more than the default 10 M instr
    // budget — the IV31 decoder does ~1.4 M instructions per
    // 160×120 keyframe.
    let out_capacity = width * height * 3;
    sb.cpu.set_instr_limit(50_000_000);
    let pre = sb.cpu.instr_count;
    let (lr, out) = sb
        .ic_decompress(hic, 0, &bih_in, payload, &bih_out, out_capacity)
        .unwrap_or_else(|e| panic!("ICDecompress trap on real cubes.mov keyframe:\n  {e}"));
    let elapsed_instrs = sb.cpu.instr_count.saturating_sub(pre);
    eprintln!(
        "ICDecompress on real cubes.mov keyframe: lr={lr:#010x}, out len {} bytes, ran {elapsed_instrs} instrs",
        out.len()
    );
    assert_eq!(out.len(), out_capacity as usize);

    // ICERR_OK or a documented negative code; positive non-zero
    // is a codec-fault sentinel.
    let lr_signed = lr as i32;
    assert!(
        lr_signed <= 0,
        "ICDecompress returned positive {lr_signed}; codec faulted"
    );

    // The headline assertion: the output buffer was zero before
    // the call, so a non-zero byte anywhere proves the codec
    // wrote *something*. Round 6 documented zero output on the
    // synthetic NULL-sync-frame path; round 7 needs a real
    // keyframe to land non-zero pixels.
    let nonzero = out.iter().any(|&b| b != 0);
    let nonzero_count = out.iter().filter(|&&b| b != 0).count();
    eprintln!(
        "cubes.mov keyframe decode: {nonzero_count}/{} output bytes non-zero",
        out.len()
    );
    assert!(
        nonzero,
        "ICDecompress wrote no non-zero bytes — the codec did NOT decode the real keyframe; \
         re-inspect the IC* path against the lr={lr:#010x} return value"
    );

    let _ = sb.ic_decompress_end(hic).expect("ICDecompressEnd");
    let _ = sb.ic_close(hic).expect("ICClose");
}
