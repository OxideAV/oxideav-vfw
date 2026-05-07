//! Round-11 PRE-FIX investigation test.
//!
//! Reproduces round-10's ICDecompress -100 against
//! `cat_attack.avi`, but with the 64-deep trace ring enabled, and
//! prints the ring on completion. The objective is to localise
//! the codec's pre-MMX validation gate.
//!
//! This test is NOT part of the round-11 milestone — it exists
//! as an investigative aid. Once the validation gate is fixed,
//! this test can be deleted (or kept as a regression sentinel
//! that asserts the codec runs PAST the previously-trapping
//! address).

mod common;

use oxideav_vfw::win32::vfw32::{Bih, BIH_SIZE};
use oxideav_vfw::Sandbox;

#[test]
fn cat_attack_first_keyframe_ring_dump() {
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

    // Check global 0x10084790 (init guard) BEFORE ICOpen
    let init_guard_pre = sb.mmu.load32(0x10084790).unwrap_or(0xFFFF_FFFF);
    let alloc_ptr_pre = sb.mmu.load32(0x1009c770).unwrap_or(0xFFFF_FFFF);
    let g_1009f10c_pre = sb.mmu.load32(0x1009f10c).unwrap_or(0xFFFF_FFFF);
    eprintln!("PRE-ICOpen: [0x10084790]={init_guard_pre:#010x} [0x1009c770]={alloc_ptr_pre:#010x} [0x1009f10c]={g_1009f10c_pre:#010x}");

    sb.cpu.enable_trace_ring(8192);
    let pre_open = sb.cpu.instr_count;
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"IV50");
    let hic = sb
        .ic_open(fcc_video, fcc_handler, ICMODE_DECOMPRESS)
        .expect("ICOpen");
    let icopen_instr = sb.cpu.instr_count - pre_open;
    eprintln!("ICOpen ran {icopen_instr} instructions");

    // Show ICOpen "far→" path
    eprintln!("ICOpen far→ entries:");
    let icring = sb.cpu.trace_ring.clone();
    let mut prev: Option<u32> = None;
    let mut cnt = 0;
    for &eip in &icring {
        let far = match prev {
            Some(p) => {
                let d = eip.abs_diff(p);
                d > 0x40
            }
            None => true,
        };
        if far {
            let mut bs = [0u8; 8];
            for j in 0..8u32 {
                bs[j as usize] = sb.mmu.load8(eip + j).unwrap_or(0);
            }
            eprintln!("  far→ {eip:#010x} bytes={bs:02x?}");
            cnt += 1;
            if cnt > 60 {
                break;
            }
        }
        prev = Some(eip);
    }
    sb.cpu.enable_trace_ring(0); // disable

    // Check global 0x10084790 AFTER ICOpen
    let init_guard_post = sb.mmu.load32(0x10084790).unwrap_or(0xFFFF_FFFF);
    let alloc_ptr_post = sb.mmu.load32(0x1009c770).unwrap_or(0xFFFF_FFFF);
    let g_1009f10c_post = sb.mmu.load32(0x1009f10c).unwrap_or(0xFFFF_FFFF);
    eprintln!("POST-ICOpen: [0x10084790]={init_guard_post:#010x} [0x1009c770]={alloc_ptr_post:#010x} [0x1009f10c]={g_1009f10c_post:#010x}");
    let _ = sb.ic_get_info(hic, 96);

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
    // Original test uses BI_RGB / 24-bit output.
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

    let _q = sb
        .ic_decompress_query(hic, &bih_in, Some(&bih_out))
        .expect("Q");
    eprintln!("ICDecompressQuery = {_q:#010x}");
    let _b = sb.ic_decompress_begin(hic, &bih_in, &bih_out).expect("B");
    eprintln!("ICDecompressBegin = {_b:#010x}");

    // ENABLE the ring NOW with a deep capacity so we capture the
    // entire ICDecompress execution.
    sb.cpu.enable_trace_ring(8192);
    sb.cpu.set_instr_limit(200_000_000);

    let out_capacity = width * height * 3;
    let pre = sb.cpu.instr_count;
    let (lr, _out) = sb
        .ic_decompress(hic, 0, &bih_in, payload, &bih_out, out_capacity)
        .expect("ICDecompress trap-free");
    let elapsed = sb.cpu.instr_count.saturating_sub(pre);
    eprintln!(
        "ICDecompress lr={lr:#010x} ({}) after {elapsed} instrs",
        lr as i32
    );

    eprintln!("trace_ring (last {} eips, deep):", sb.cpu.trace_ring.len());
    // We don't print the deep ring entirely — too much.
    // Print only entries where the EIP is the first byte of a
    // function (heuristic: previous EIP is far away).
    let ring = &sb.cpu.trace_ring;
    let mut prev_eip: Option<u32> = None;
    let mut cnt = 0;
    for (i, &eip) in ring.iter().enumerate() {
        let far = match prev_eip {
            Some(p) => {
                let d = eip.abs_diff(p);
                d > 0x40
            }
            None => true,
        };
        if far {
            let mut bs = [0u8; 12];
            for j in 0..12u32 {
                bs[j as usize] = sb.mmu.load8(eip + j).unwrap_or(0);
            }
            eprintln!("  [{i:5}] far→ {eip:#010x} bytes={bs:02x?}");
            cnt += 1;
            if cnt > 200 {
                break;
            }
        }
        prev_eip = Some(eip);
    }

    // Also dump verbatim around indices [4540..4620] and the LAST 100.
    let n = ring.len();
    eprintln!("VERBATIM ring [3100..3300]:");
    if n > 3100 {
        let upper = (3300usize).min(n);
        for (i, &eip) in ring[3100..upper].iter().enumerate() {
            let mut bs = [0u8; 8];
            for j in 0..8u32 {
                bs[j as usize] = sb.mmu.load8(eip + j).unwrap_or(0);
            }
            eprintln!("  [{:5}]: {eip:#010x} bytes={bs:02x?}", 3100 + i);
        }
    }
    eprintln!("VERBATIM ring [3440..3470]:");
    if n > 3440 {
        let upper = (3470usize).min(n);
        for (i, &eip) in ring[3440..upper].iter().enumerate() {
            let mut bs = [0u8; 8];
            for j in 0..8u32 {
                bs[j as usize] = sb.mmu.load8(eip + j).unwrap_or(0);
            }
            eprintln!("  [{:5}]: {eip:#010x} bytes={bs:02x?}", 3440 + i);
        }
    }
    eprintln!("VERBATIM ring [4480..4530]:");
    if n > 4480 {
        let upper = (4530usize).min(n);
        for (i, &eip) in ring[4480..upper].iter().enumerate() {
            let mut bs = [0u8; 8];
            for j in 0..8u32 {
                bs[j as usize] = sb.mmu.load8(eip + j).unwrap_or(0);
            }
            eprintln!("  [{:5}]: {eip:#010x} bytes={bs:02x?}", 4480 + i);
        }
    }
    eprintln!("VERBATIM ring [4549..4570]:");
    if n > 4549 {
        let upper = (4570usize).min(n);
        for (i, &eip) in ring[4549..upper].iter().enumerate() {
            let mut bs = [0u8; 8];
            for j in 0..8u32 {
                bs[j as usize] = sb.mmu.load8(eip + j).unwrap_or(0);
            }
            eprintln!("  [{:5}]: {eip:#010x} bytes={bs:02x?}", 4549 + i);
        }
    }
    eprintln!("VERBATIM ring [4540..4620]:");
    if n > 4540 {
        let upper = (4620usize).min(n);
        for (i, &eip) in ring[4540..upper].iter().enumerate() {
            let mut bs = [0u8; 8];
            for j in 0..8u32 {
                bs[j as usize] = sb.mmu.load8(eip + j).unwrap_or(0);
            }
            eprintln!("  [{:5}]: {eip:#010x} bytes={bs:02x?}", 4540 + i);
        }
    }
    eprintln!("LAST 80 ring entries:");
    let start = n.saturating_sub(80);
    for (i, &eip) in ring[start..].iter().enumerate() {
        let mut bs = [0u8; 8];
        for j in 0..8u32 {
            bs[j as usize] = sb.mmu.load8(eip + j).unwrap_or(0);
        }
        eprintln!("  [{:5}]: {eip:#010x} bytes={bs:02x?}", start + i);
    }

    if let Some(&first) = sb.cpu.trace_ring.first() {
        eprintln!("first ring eip = {first:#010x}");
    }

    // Dump dispatch table at 0x1004f80c (22 entries, 0..=0x15).
    eprintln!("dispatch table at 0x1004f80c:");
    for i in 0..=0x15u32 {
        let slot = 0x1004f80c + 4 * i;
        let target = sb.mmu.load32(slot).unwrap_or(0xDEADBEEF);
        eprintln!("  [{i:#04x}] @ {slot:#010x} → {target:#010x}");
    }

    // Dump the dispatch body for context.
    eprint!("dispatch fn 0x1004f7c0..0x1004f810:");
    for off in 0..0x50u32 {
        let b = sb.mmu.load8(0x1004f7c0 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x1004f7c0 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    // Dump the calling fn 0x10041e1e and its preceding section.
    eprint!("calling fn 0x10041d80..0x10041ec0:");
    for off in 0..0x140u32 {
        let b = sb.mmu.load8(0x10041d80 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x10041d80 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    // We need also the global structure at 0x10098874 which was
    // pushed as 2nd arg to the error-code mapper.
    eprint!("global at 0x10098874..+64:");
    for off in 0..0x40u32 {
        let b = sb.mmu.load8(0x10098874 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x10098874 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    // Look at 0x10045c08..0x10045c20 — site after 0x100458b5 calls
    // landing back at 0x10045c0b (function exit).
    eprint!("exit fn 0x10045c00..0x10045c20:");
    for off in 0..0x20u32 {
        let b = sb.mmu.load8(0x10045c00 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x10045c00 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    // Higher-level fn at 0x100458a1 area we returned from.
    eprint!("fn 0x10045880..0x100458d0:");
    for off in 0..0x50u32 {
        let b = sb.mmu.load8(0x10045880 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x10045880 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    // The "test al, 0x10" at 0x10041eb2 is interesting — flag check.
    // After 0x10041eae (RET 32), we land at 0x10041eb2 area in another fn.
    eprint!("fn 0x10041eb0..0x10041f80:");
    for off in 0..0xd0u32 {
        let b = sb.mmu.load8(0x10041eb0 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x10041eb0 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    // The "mov eax, 2" set the error code at 0x1003f89c.
    // Find the conditional jmp that took us there.
    eprint!("fn 0x1003f880..0x1003f900 (error-code site):");
    for off in 0..0x80u32 {
        let b = sb.mmu.load8(0x1003f880 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x1003f880 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    // 0x1003f210 is where post-call lands (post call to something).
    eprint!("fn 0x1003f1e0..0x1003f300:");
    for off in 0..0x120u32 {
        let b = sb.mmu.load8(0x1003f1e0 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x1003f1e0 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    // 0x10001c90 — error gen point.
    eprint!("fn 0x10001c90..0x10001d00:");
    for off in 0..0x70u32 {
        let b = sb.mmu.load8(0x10001c90 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x10001c90 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();
    eprint!("fn 0x10002f00..0x10002f80:");
    for off in 0..0x80u32 {
        let b = sb.mmu.load8(0x10002f00 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x10002f00 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();
    eprint!("fn 0x1000ba00..0x1000bf80 (decoder body):");
    for off in 0..0x600u32 {
        let b = sb.mmu.load8(0x1000ba00 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x1000ba00 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();
    eprint!("fn 0x1000c100..0x1000c500:");
    for off in 0..0x400u32 {
        let b = sb.mmu.load8(0x1000c100 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x1000c100 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    eprint!("fn 0x10001010..0x10001100 (unknown call returning non-zero):");
    for off in 0..0xf0u32 {
        let b = sb.mmu.load8(0x10001010 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x10001010 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    let g = sb.mmu.load32(0x1009c770).unwrap_or(0xFFFF_FFFF);
    eprintln!("global at 0x1009c770 = {g:#010x}");

    let g2 = sb.mmu.load32(0x1009f088).unwrap_or(0xFFFF_FFFF);
    eprintln!("global at 0x1009f088 = {g2:#010x}");

    let g3 = sb.mmu.load32(0x100af030).unwrap_or(0xFFFF_FFFF);
    eprintln!("global at 0x100af030 = {g3:#010x}");

    let g4 = sb.mmu.load32(0x1009f00c).unwrap_or(0xFFFF_FFFF);
    eprintln!("global at 0x1009f00c = {g4:#010x}");

    // Read raw at 0x10002f14
    eprint!("raw at 0x10002f14..0x10002f60:");
    for off in 0..0x50u32 {
        let b = sb.mmu.load8(0x10002f14 + off).unwrap_or(0);
        if off % 16 == 0 {
            eprint!("\n  {:#010x}:", 0x10002f14 + off);
        }
        eprint!(" {b:02x}");
    }
    eprintln!();

    // String pushes at 0x10002f14 onward: PUSH_imm32 (5 bytes each).
    let str_addrs = [
        0x100027e7u32,
        0x100026a1,
        0x10002541,
        0x10002400,
        0x100021c0,
        0x10002ed2,
        0x1000223f,
        0x100022cd,
        0x100023cf,
        0x100022a6,
        0x10002318,
    ];
    eprintln!("Strings:");
    for &p in &str_addrs {
        let mut s = String::new();
        for i in 0..60u32 {
            let b = sb.mmu.load8(p + i).unwrap_or(0);
            if b == 0 {
                break;
            }
            s.push(if (32..=126).contains(&b) {
                b as char
            } else {
                '?'
            });
        }
        eprintln!("  {p:#010x}: {s:?}");
    }

    // Read what was at the picture header bit-stream byte 0..32.
    eprintln!(
        "Payload first 32 bytes: {:02x?}",
        &payload[..32.min(payload.len())]
    );
}
