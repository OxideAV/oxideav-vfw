//! Round 22 — research instrument: trace `ICDecompressBegin`
//! through `mpg4c32.dll` to find which gate returns
//! `ICERR_INTERNAL = -100`.
//!
//! Round 21 left ICDecompressBegin returning 0xFFFFFF9C
//! (ICERR_INTERNAL). Static disassembly (`objdump -d
//! mpg4c32.dll`) shows DriverProc (RVA 0x2070) routes
//! ICM_DECOMPRESS_BEGIN (0x400C) through a jump table at
//! 0x1c202492 to handler 0x1c2021c6, which falls into
//! 0x1c203469. That body issues a chain of `IsBadWritePtr`
//! / `IsBadReadPtr` calls; if any returns non-zero the
//! handler jumps to 0x1c20355f and returns -0x64 (= -100).
//! The dwDriverId pointer, lParam1 (input BIH) and lParam2
//! (output BIH) are all probed.
//!
//! This test enables both `Cpu::trace_ring` (last-128
//! instructions) and `Cpu::visited_eips` and dumps:
//!   1. last 64 EIPs before the call returns
//!   2. final EAX (= LRESULT)
//!   3. set of distinct EIPs visited inside the begin handler
//!
//! Skipped if mpg4c32.dll isn't present.

mod common;

use oxideav_vfw::{Bih, Sandbox};
use std::path::PathBuf;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn mpg4c32_path() -> Option<PathBuf> {
    let p =
        workspace_root()?.join("docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/mpg4c32.dll");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

#[test]
fn trace_decompress_begin_eip_path() {
    let Some(p) = mpg4c32_path() else {
        eprintln!("round22: mpg4c32.dll missing; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(500_000_000);
    let img = sb.load("mpg4c32.dll", &bytes).unwrap();

    let _ = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .unwrap();
    sb.install_codec(&img).unwrap();

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"MP43");
    let hic = sb.ic_open(fcc_video, fcc_handler, 2).unwrap();
    eprintln!("round22: hic = {hic:#010x}");
    if hic == 0 {
        return;
    }

    // Look up the per-instance state pointer the codec gave us.
    // ic_open stored it in the `HicEntry::driver_id`.
    let driver_id = sb.host.hics.get(&hic).map(|e| e.driver_id).unwrap_or(0);
    eprintln!("round22: driver_id = {driver_id:#010x}");

    let input = Bih {
        bi_size: 40,
        width: 176,
        height: 144,
        planes: 1,
        bit_count: 24,
        compression: *b"MP43",
        size_image: 1024,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
    };
    let output = Bih {
        bi_size: 40,
        width: 176,
        height: 144,
        planes: 1,
        bit_count: 24,
        compression: [0; 4],
        size_image: 176 * 144 * 3,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
    };

    let q = sb.ic_decompress_query(hic, &input, Some(&output));
    eprintln!("round22: ICDecompressQuery -> {q:?}");

    // Turn on the instruction-trace ring + visited-eip set
    // before the begin call. The ring holds the last 256
    // instructions; that's enough to capture the failure-path
    // CMP/JE chain at the tail of the begin handler.
    sb.cpu.enable_trace_ring(256);
    sb.cpu.enable_visited_eip_tracking();

    let begin = sb.ic_decompress_begin(hic, &input, &output);
    eprintln!("round22: ICDecompressBegin -> {begin:?}");

    let visited = sb.cpu.take_visited_eips();
    let ring = sb.cpu.trace_ring.clone();

    // Per the static disasm, the begin handler at 0x1c203469
    // and the failure tail at 0x1c20355f are at known VAs;
    // also flag whether each was reached.
    let saw_handler = visited.contains(&0x1c203469);
    let saw_failure_tail = visited.contains(&0x1c20355f);
    let saw_success_div = visited.contains(&0x1c203545);
    eprintln!(
        "round22: visited_eips total={} saw_handler={} saw_failure_tail={} saw_success_div={}",
        visited.len(),
        saw_handler,
        saw_failure_tail,
        saw_success_div
    );

    // Print the last 64 entries of the ring. If the handler
    // entered failure_tail, the last addresses leading to
    // 0x1c20355f tell us which CMP failed.
    eprintln!("round22: trace_ring.tail(64):");
    let n = ring.len();
    for &eip in ring.iter().skip(n.saturating_sub(64)) {
        eprintln!("  {eip:#010x}");
    }

    // Filter visited EIPs in the begin-handler range
    // [0x1c203469, 0x1c203570].
    let in_handler: Vec<u32> = visited
        .iter()
        .copied()
        .filter(|&e| (0x1c203469..0x1c203570).contains(&e))
        .collect();
    eprintln!(
        "round22: visited EIPs inside begin-handler [0x1c203469..0x1c203570]: {}",
        in_handler.len()
    );
    for e in &in_handler {
        eprintln!("  {e:#010x}");
    }

    let _ = sb.ic_decompress_end(hic);
    let _ = sb.ic_close(hic);
}
