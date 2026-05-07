//! Round 8 — DllMain + ICOpen smoke test for IR50_32.DLL. Exists
//! to localise the memory fault before we drive ICDecompress.

mod common;

use oxideav_vfw::Sandbox;

#[test]
fn dllmain_smoke() {
    let dll_bytes = common::fetch_or_load("IR50_32.DLL").expect("fetch IR50_32.DLL");
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(50_000_000);
    let img = sb
        .load("IR50_32.DLL", &dll_bytes)
        .expect("load IR50_32.DLL");
    eprintln!("Image base = {:#x}", img.image_base);
    eprintln!(
        "DriverProc export VA = {:?}",
        img.export("DriverProc")
    );
    let pre = sb.cpu.instr_count;
    let r = sb
        .call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .expect("DllMain");
    let elapsed = sb.cpu.instr_count - pre;
    eprintln!(
        "DllMain → {r:#010x}, {elapsed} instructions, last debug log: {:?}",
        sb.host.debug_log.last()
    );

    sb.install_codec(&img).expect("DriverProc not exported");
    sb.host.trace_stubs = true;
    let dp_va = img.export("DriverProc").unwrap();
    eprintln!("DriverProc VA = {dp_va:#010x}");
    let mut dp_bytes = Vec::new();
    for i in 0..96u32 {
        dp_bytes.push(sb.mmu.load8(dp_va + i).unwrap_or(0));
    }
    eprintln!("DriverProc bytes: {dp_bytes:02x?}");

    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"IV50");
    let pre = sb.cpu.instr_count;
    match sb.ic_open(fcc_video, fcc_handler, 2) {
        Ok(hic) => eprintln!(
            "ICOpen → hic={hic:#010x}, instr={}, last debug={:?}",
            sb.cpu.instr_count - pre,
            sb.host.debug_log.last()
        ),
        Err(e) => {
            eprintln!(
                "ICOpen FAILED after {} instructions; eip={:#010x}; trap: {e}",
                sb.cpu.instr_count - pre,
                sb.cpu.regs.eip,
            );
            // Dump esi memory
            let esi = sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Esi);
            let mut esi_bytes = Vec::new();
            for i in 0..32u32 {
                esi_bytes.push(sb.mmu.load8(esi + i).unwrap_or(0));
            }
            eprintln!("  esi={esi:#010x}, mem [esi..+32): {esi_bytes:02x?}");
            // Also esi-1, esi+1 area (in case of off-by-one)
            let esi_aligned = esi - 1;
            let mut esi_a_bytes = Vec::new();
            for i in 0..40u32 {
                esi_a_bytes.push(sb.mmu.load8(esi_aligned + i).unwrap_or(0));
            }
            eprintln!("  mem [esi-1..+40): {esi_a_bytes:02x?}");
            // Dump bytes around eip
            let eip = sb.cpu.regs.eip;
            let mut prev = Vec::new();
            for i in 1..=192u32 {
                prev.push(sb.mmu.load8(eip - 193 + i).unwrap_or(0));
            }
            let mut bytes = Vec::new();
            for i in 0..32u32 {
                bytes.push(sb.mmu.load8(eip + i).unwrap_or(0));
            }
            eprintln!("  bytes [eip-24..eip): {prev:02x?}");
            eprintln!("  bytes @ eip:         {bytes:02x?}");
            eprintln!(
                "  registers: eax={:#x} ebx={:#x} ecx={:#x} edx={:#x} esi={:#x} edi={:#x} ebp={:#x} esp={:#x}",
                sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Eax),
                sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Ebx),
                sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Ecx),
                sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Edx),
                sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Esi),
                sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Edi),
                sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Ebp),
                sb.cpu.regs.get32(oxideav_vfw::emulator::regs::Reg32::Esp),
            );
            eprintln!("  debug log (full, {} entries):", sb.host.debug_log.len());
            for (i, line) in sb.host.debug_log.iter().enumerate() {
                eprintln!("    [{i}]: {line}");
            }
            eprintln!("  modules: {:?}", sb.host.modules);
            eprintln!(
                "  stub trace ({} stub calls):",
                sb.host.stub_trace.len()
            );
            for (i, line) in sb.host.stub_trace.iter().enumerate() {
                eprintln!("    [{i}]: {line}");
            }
            panic!("ICOpen failed");
        }
    }
}
