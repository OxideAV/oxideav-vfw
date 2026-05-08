//! Round 15 — IV41 (Indeo 4) decode through `IR41_32.AX`'s
//! `DriverProc` export.
//!
//! Round 14 Part B established that `IR41_32.AX` is **simultaneously**
//! a DirectShow filter (the `DllGetClassObject` / `DllRegisterServer`
//! surface) AND a Video for Windows codec (the `DriverProc` export).
//! Microsoft's Indeo 4 ships a single binary that fronts both APIs —
//! the same compressor body sits behind a VfW dispatch table at one
//! export and a COM filter at another. The COM stack is multi-round
//! work to scaffold; the VfW IC* surface is what rounds 8..14 already
//! drive against `IR50_32.DLL`. Round 15 reuses that pipeline.
//!
//! This probe walks:
//!   1. fetch the binary,
//!   2. confirm `DriverProc` is exported (the VfW path is reachable),
//!   3. load + DllMain + install_codec(via DriverProc),
//!   4. ICOpen('VIDC','IV41', ICMODE_DECOMPRESS),
//!   5. ICGetInfo (record szName for diagnostics),
//!   6. fetch the smallest properly-aligned IV41 fixture
//!      (`crashtest.avi`, 5.0 MiB, 240×180) from the FFmpeg
//!      corpus,
//!   7. extract the first sample bytes via `avi_extractor`,
//!   8. ICDecompressQuery → ICDecompressBegin → ICDecompress →
//!      ICDecompressEnd → ICClose.
//!
//! Acceptance gate (round-15 milestone bar): `ICDecompress`
//! returns `ICERR_OK` (0) AND the RGB24 output buffer has more
//! than 25 % non-zero bytes — the same shape rounds 7
//! (IR32 / cubes.mov) and 12 (IR50 / cat_attack.avi) ratchet to.
//!
//! References (clean-room):
//! * Microsoft VfW SDK header `vfw.h` — for `DriverProc` ABI +
//!   the IC* message numbering this driver receives.
//! * Microsoft "Installable Drivers" specification — for the
//!   `DRV_LOAD` / `DRV_ENABLE` / `DRV_OPEN` / `DRV_CLOSE` /
//!   `DRV_DISABLE` / `DRV_FREE` lifecycle that wraps the
//!   per-instance `IC*` calls.
//! * `docs/video/indeo/indeo4/wiki/Indeo_4.wiki` — for the IV41
//!   bitstream shape (transforms / quantisation / motion model).
//!
//! NEVER reference: ffmpeg's `libavcodec/indeo4.c`, Wine's
//! `dlls/quartz`, ReactOS, or any third-party reverse of the
//! Indeo filter.

mod common;

use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};
use oxideav_vfw::Sandbox;

/// Confirm `IR41_32.AX` exports `DriverProc` so the existing IC*
/// pipeline can drive it. Round 14's surface probe was named
/// after the `.AX` suffix and assumed COM-only; the round-14
/// dispatch prompt later confirmed `DriverProc` is in fact
/// present, which is the round-15 unblock.
#[test]
fn ir41_32_ax_exports_driverproc() {
    let bytes = common::fetch_or_load("IR41_32.AX").expect("fetch IR41_32.AX");
    let parsed = oxideav_vfw::pe::header::parse(&bytes).expect("parse IR41_32.AX");
    let exports =
        oxideav_vfw::pe::exports::parse_exports(&parsed, &bytes, parsed.optional.image_base)
            .expect("parse exports");
    eprintln!("IR41_32.AX exports ({}):", exports.len());
    for (name, rva) in &exports {
        eprintln!("  {name:<32} rva={rva:#x}");
    }
    assert!(
        exports.contains_key("DriverProc"),
        "round-15 gate: IR41_32.AX must export DriverProc \
         (the VfW IC* dispatch entry); without it, the round-15 \
         plan to reuse the IR50/IR32 pipeline is invalid. \
         Available exports: {:?}",
        exports.keys().collect::<Vec<_>>(),
    );
}

/// Round-15 milestone test — drive IV41 through every IC* call in
/// the pipeline and confirm `ICDecompress` produces non-zero RGB24
/// pixels. Mirrors the round-12 (IR50 / cat_attack.avi) bar against
/// the new IR41 path.
#[test]
fn ir41_first_sample_reaches_ic_decompress() {
    /// vfw.h: `ICMODE_DECOMPRESS = 2`.
    const ICMODE_DECOMPRESS: u32 = 2;

    // -- Step 1 + 2: fetch the codec, confirm DriverProc.
    let dll_bytes = common::fetch_or_load("IR41_32.AX").expect("fetch IR41_32.AX");
    eprintln!("IR41_32.AX: {} bytes", dll_bytes.len());

    // -- Step 6 + 7: fetch the smallest properly-aligned IV41
    // fixture and extract the first sample.
    //
    // samples.oxideav.org/ffmpeg/V-codecs/IV41/index.json lists
    // four fixtures. The smallest (`mario001.mov`, 1.74 MiB,
    // 300×225) trips the codec at `ICDecompressBegin` with
    // `ICERR_BADIMAGESIZE = -201` because 225 isn't a multiple
    // of 4 — Indeo 4's `Picture height ex` field
    // (`docs/video/indeo/indeo4/wiki/Indeo_4.wiki` §"Bitstream
    // format description") is documented as "should be a
    // multiply of 4". The next-smallest is `crashtest.avi`
    // (5.0 MiB, 240×180 — both dimensions multiples of 4),
    // which we use here.
    let avi =
        common::fetch_or_load_ffmpeg_sample("IV41", "crashtest.avi").expect("fetch crashtest.avi");
    eprintln!("crashtest.avi: {} bytes", avi.len());
    let sample = common::avi_extractor::extract_first_video_sample(&avi)
        .expect("AVI walker on crashtest.avi");
    eprintln!(
        "crashtest.avi sample 0: codec_fourcc={:08x} ({}) {}x{} @offset={} size={}",
        sample.codec_fourcc,
        std::str::from_utf8(&sample.codec_fourcc.to_le_bytes())
            .unwrap_or("?")
            .escape_debug(),
        sample.width,
        sample.height,
        sample.sample_offset,
        sample.sample_size,
    );
    assert_eq!(
        sample.codec_fourcc,
        u32::from_le_bytes(*b"IV41"),
        "expected crashtest.avi sample-0 fourcc to be IV41"
    );
    let payload = sample.bytes.clone();
    let width = sample.width;
    let height = sample.height;

    // -- Step 3: load + DllMain.
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(200_000_000);
    let img = sb
        .load("IR41_32.AX", &dll_bytes)
        .expect("round-15 milestone: IR41_32.AX must load");
    eprintln!("IR41_32.AX image_base={:#x}", img.image_base);
    eprintln!(
        "IR41_32.AX exports of interest: DriverProc={:?} DllMain={:?} DllGetClassObject={:?}",
        img.export("DriverProc"),
        img.export("DllMain"),
        img.export("DllGetClassObject"),
    );
    let pre = sb.cpu.instr_count;
    let dll_main_ret = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .expect("round-15 milestone: IR41 DllMain must not trap");
    eprintln!(
        "DllMain → {dll_main_ret:#010x} ({} instructions)",
        sb.cpu.instr_count - pre
    );

    // -- Step 4: install + ICOpen.
    sb.install_codec(&img)
        .expect("round-15 milestone: IR41 must export DriverProc");
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"IV41");
    let pre = sb.cpu.instr_count;
    let hic = sb
        .ic_open(fcc_video, fcc_handler, ICMODE_DECOMPRESS)
        .expect("round-15 milestone: ICOpen IV41 must not trap");
    eprintln!(
        "ICOpen IV41 → hic={hic:#010x} ({} instructions)",
        sb.cpu.instr_count - pre
    );
    assert_ne!(
        hic, 0,
        "round-15 milestone: ICOpen IV41 must mint a HIC (codec accepted DRV_OPEN)"
    );
    let driver_id = sb.host.hics.get(&hic).unwrap().driver_id;
    eprintln!("ICOpen → driver_id={driver_id:#010x}");

    // -- Step 5: ICGetInfo (diagnostic only — short returns are
    // OK; many codecs delegate this to the host vfw32 registry).
    match sb.ic_get_info(hic, 96) {
        Ok(info) => {
            eprintln!("ICGetInfo returned {} bytes", info.len());
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
                eprintln!("ICGetInfo szName (ASCII tail): {sz_name_ascii:?}");
            }
        }
        Err(e) => eprintln!("ICGetInfo trapped (non-fatal for round 15): {e}"),
    }

    // -- Step 8: ICDecompressQuery → Begin → Decompress → End.
    let bih_in = Bih {
        bi_size: BIH_SIZE,
        width: width as i32,
        height: height as i32,
        planes: 1,
        bit_count: 24,
        compression: *b"IV41",
        size_image: payload.len() as u32,
        ..Default::default()
    };
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

    let pre = sb.cpu.instr_count;
    let q = sb
        .ic_decompress_query(hic, &bih_in, Some(&bih_out))
        .expect("round-15 milestone: ICDecompressQuery must not trap");
    eprintln!(
        "ICDecompressQuery={q:#010x} ({} instructions)",
        sb.cpu.instr_count - pre
    );

    let pre = sb.cpu.instr_count;
    let b = sb
        .ic_decompress_begin(hic, &bih_in, &bih_out)
        .expect("round-15 milestone: ICDecompressBegin must not trap");
    eprintln!(
        "ICDecompressBegin={b:#010x} ({} instructions)",
        sb.cpu.instr_count - pre
    );

    let out_capacity = width * height * 3;
    let pre = sb.cpu.instr_count;
    let (lr, out) = sb
        .ic_decompress(hic, 0, &bih_in, &payload, &bih_out, out_capacity)
        .expect("ICDecompress should not trap (round-15 milestone)");
    let nonzero = out.iter().filter(|&&b| b != 0).count();
    eprintln!(
        "ICDecompress: lr={lr:#010x} ({}), {} bytes, {nonzero} non-zero, \
         {} instructions",
        lr as i32,
        out.len(),
        sb.cpu.instr_count - pre
    );

    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);

    // Round-15 milestone bar: matches the round-7 (cubes.mov) and
    // round-12 (cat_attack.avi) acceptance bars on the IR32 / IR50
    // pipelines — `ICDecompress` returns `ICERR_OK` (0) AND the
    // RGB24 output buffer has > 25 % non-zero bytes. Round-14
    // confirmed the IV41 path was reachable through the existing
    // VfW IC* surface (the `IR41_32.AX` `.AX` filter ALSO exports
    // `DriverProc`); round-15 is the first end-to-end IV41 frame.
    assert_eq!(
        b, 0,
        "round-15 expected ICDecompressBegin to return ICERR_OK (0)"
    );
    assert_eq!(
        q, 0,
        "round-15 expected ICDecompressQuery to return ICERR_OK (0)"
    );
    assert_eq!(
        lr, 0,
        "round-15 expected ICDecompress to return ICERR_OK (0); got {} (signed {})",
        lr, lr as i32
    );
    assert!(
        nonzero > out_capacity as usize / 4,
        "round-15 expected ICDecompress to populate at least \
         25% of the output buffer with non-zero pixels; only got \
         {nonzero}/{} non-zero",
        out_capacity
    );
}
