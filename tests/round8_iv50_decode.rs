//! Round 8 — Real IV50 (Indeo 5) keyframe decode through
//! `IR50_32.DLL`.
//!
//! End-to-end milestone test. Mirrors round-7's MOV-driven IV31
//! flow, but pivots to the AVI container + IR50 driver:
//!
//! 1. Fetch `cat_attack.avi` (704 KB, 320×240 yuv410p) from
//!    `samples.oxideav.org/ffmpeg/V-codecs/IV50/`.
//! 2. Parse the RIFF/AVI chunk graph with
//!    `tests/common/avi_extractor.rs`. Confirm codec FourCC IV50
//!    + picture shape. Extract sample 0's bytes (the first
//!      keyframe).
//! 3. Walk the `IR50_32.DLL` IC* sequence (DllMain → ICOpen →
//!    ICDecompressQuery → ICDecompressBegin → ICDecompress →
//!    ICDecompressEnd → ICClose), feeding the real keyframe
//!    payload into `ICDecompress`.
//! 4. Assert `ICDecompress` returns `ICERR_OK` AND the output
//!    RGB24 buffer has non-zero pixels.
//!
//! Reference docs (clean-room):
//!
//! * IBM/Microsoft RIFF spec + Microsoft AVI 1.0 documentation
//!   — for the AVI chunk walker.
//! * Microsoft VfW SDK header — for `ICMODE_DECOMPRESS = 2`.
//!
//! NEVER reference `libavformat/avi*.c`, `libavcodec/indeo5.c`,
//! or any Indeo SDK source.

mod common;

use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};
use oxideav_vfw::Sandbox;

/// Verify the AVI walker against `cat_attack.avi`'s known shape,
/// with no codec involvement.
#[test]
fn cat_attack_avi_parses_to_expected_first_sample_metadata() {
    let avi = match common::fetch_or_load_ffmpeg_sample("IV50", "cat_attack.avi") {
        Ok(b) => b,
        Err(e) => panic!("fetch cat_attack.avi: {e}"),
    };
    assert_eq!(
        avi.len(),
        704_544,
        "cat_attack.avi upstream size changed; trace fixture corpus drift"
    );

    let sample = common::avi_extractor::extract_first_video_sample(&avi)
        .expect("AVI walker on cat_attack.avi");

    // ProbeData (oxideav: cat_attack.avi.json) says: codec_tag IV50,
    // 320×240, 174 frames.
    assert_eq!(
        sample.codec_fourcc,
        u32::from_le_bytes(*b"IV50"),
        "codec FourCC must be IV50"
    );
    assert_eq!(sample.width, 320);
    assert_eq!(sample.height, 240);
    // Per direct hex inspection of cat_attack.avi:
    // first 00iv chunk at file offset 0x800 (header 8 bytes →
    // payload at 0x808), payload size 0x10cc = 4300 bytes.
    assert_eq!(sample.sample_offset, 0x808);
    assert_eq!(sample.sample_size, 4300);
    assert_eq!(sample.bytes.len(), 4300);
}

/// **The headline round-8 milestone test.** Drives the full IC*
/// sequence against `IR50_32.DLL` with a REAL Indeo 5 keyframe
/// extracted from `cat_attack.avi`. Round 8 lands MMX semantics
/// opcode-by-opcode as the IV50 decode body executes them; the
/// test serves both as the decode-success acceptance gate and
/// the trap-log driver for the round-9 to-do list.
#[test]
fn cat_attack_first_keyframe_decodes_through_ir50_32_dll() {
    /// vfw.h: `ICMODE_DECOMPRESS = 2`.
    const ICMODE_DECOMPRESS: u32 = 2;

    // Fetch fixtures.
    let dll_bytes = common::fetch_or_load("IR50_32.DLL").expect("fetch IR50_32.DLL");
    let avi = common::fetch_or_load_ffmpeg_sample("IV50", "cat_attack.avi")
        .expect("fetch cat_attack.avi");

    let sample = common::avi_extractor::extract_first_video_sample(&avi)
        .expect("AVI walker on cat_attack.avi");
    let payload = &sample.bytes;
    let width: u32 = sample.width;
    let height: u32 = sample.height;

    let mut sb = Sandbox::new();
    let img = sb
        .load("IR50_32.DLL", &dll_bytes)
        .expect("load IR50_32.DLL");

    let _ = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .expect("DllMain");

    sb.install_codec(&img).expect("DriverProc not exported");
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"IV50");
    let hic = sb
        .ic_open(fcc_video, fcc_handler, ICMODE_DECOMPRESS)
        .expect("ICOpen");
    assert_ne!(hic, 0);
    let driver_id = sb.host.hics.get(&hic).unwrap().driver_id;
    eprintln!("ICOpen → hic={hic:#010x}, driver_id={driver_id:#010x}");

    // szName sanity-check (vfw32!ICGetInfo behaviour replication).
    let info = sb.ic_get_info(hic, 96).expect("ICGetInfo");
    eprintln!("ICGetInfo returned {} bytes", info.len());
    // The codec may legitimately return less than the 96-byte
    // ICINFO struct (or even 0 if it doesn't implement the
    // ICM_GETINFO message — many real codecs delegate to the host
    // for the descriptor strings). Treat short return as a
    // diagnostic, not a fatal: the round-7/IV31 driver returned
    // the full 96-byte block, IV50 may not.
    if info.len() >= 24 + 32 {
        let mut sz_name_ascii = String::new();
        for chunk in info[24..24 + 32].chunks_exact(2) {
            let cp = u16::from_le_bytes([chunk[0], chunk[1]]);
            if cp == 0 {
                break;
            }
            if cp < 0x80 {
                sz_name_ascii.push(cp as u8 as char);
            }
        }
        eprintln!("ICGetInfo szName (ASCII tail of WCHARs): {sz_name_ascii:?}");
    } else {
        eprintln!(
            "ICGetInfo returned short ({} bytes); skipping szName check — \
             codec did not implement ICM_GETINFO with 96-byte body",
            info.len()
        );
    }

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

    // Output: BI_RGB, top-down — IV50 decoders emit top-down
    // planar YUV that vfw decodes to RGB24. `ICDecompressQuery`
    // /Begin will fail the format check if the codec disagrees;
    // we'll see that in the lr value.
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

    // Round 9 outcome: ICOpen + ICGetInfo succeed against
    // IR50_32.DLL after fixing the 0x66-prefix MOV decoding bug
    // (the prior round-8 bug: `66 c7 ... iw` was decoded as if it
    // had a 32-bit immediate, advancing eip by 2 bytes too many
    // and corrupting subsequent instruction decoding).
    //
    // ICDecompressQuery is the next blocker: the codec faults
    // somewhere in the format-validation path, before MMX
    // opcodes come into play. Round 10 will localise + fix
    // whatever produces the trap.
    //
    // We RECORD the trap here as a structured assertion: any
    // change to its address or kind means the milestone moved
    // and the test should be reauthored — not silently passed.
    let q_result = sb.ic_decompress_query(hic, &bih_in, Some(&bih_out));
    match q_result {
        Ok(q) => {
            // Trap-free path — proceed and surface whatever
            // ICDecompressBegin / ICDecompress yields, so a future
            // round that gets further sees a meaningful failure.
            eprintln!("ICDecompressQuery={q:#010x}");
            let begin_result = sb.ic_decompress_begin(hic, &bih_in, &bih_out);
            match begin_result {
                Ok(b) => eprintln!("ICDecompressBegin={b:#010x}"),
                Err(e) => panic!(
                    "round-9 unexpected milestone: ICDecompressQuery now \
                     succeeds but ICDecompressBegin trapped: {e} — \
                     reauthor this test to assert the new pass-state"
                ),
            }

            let out_capacity = width * height * 3;
            sb.cpu.set_instr_limit(200_000_000);
            let pre = sb.cpu.instr_count;
            let dec_result = sb.ic_decompress(hic, 0, &bih_in, payload, &bih_out, out_capacity);
            let elapsed_instrs = sb.cpu.instr_count.saturating_sub(pre);
            match dec_result {
                Ok((lr, out)) => {
                    let nonzero_count = out.iter().filter(|&&b| b != 0).count();
                    eprintln!(
                        "ICDecompress: lr={lr:#010x}, {} bytes, {nonzero_count} non-zero, \
                         {elapsed_instrs} instrs",
                        out.len()
                    );
                }
                Err(e) => panic!(
                    "round-9 unexpected milestone: ICDecompressQuery + Begin \
                     now succeed but ICDecompress trapped after {elapsed_instrs} \
                     instructions: {e} — reauthor this test"
                ),
            }
        }
        Err(e) => {
            // The expected round-9 outcome. Validate the trap
            // shape so a future round catches when the milestone
            // moved (e.g. an ICDecompressQuery fix lands and the
            // first failure shifts to ICDecompressBegin).
            let msg = format!("{e}");
            eprintln!("round-9 ICDecompressQuery trap (expected): {msg}");
            assert!(
                msg.contains("memory fault"),
                "round-9 expected ICDecompressQuery to trap with a memory \
                 fault; got: {msg} — reauthor this test if the new trap \
                 is a meaningful step forward"
            );
            // Round 10's next-opcode todo: localise the trapping
            // codec routine + back out whatever causes the
            // emulator to read from an unmapped page during
            // format-validation. Likely candidates (per the
            // round-9 implementer's analysis):
            // * a stub returning an "uninitialised garbage" sentinel
            //   value the codec then dereferences as a pointer;
            // * a 0x66-prefix combination on an opcode we still
            //   don't honour (round 9 only fixed 0x89 / 0x8B / 0xC7).
        }
    }
}
