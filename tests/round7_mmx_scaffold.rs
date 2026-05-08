//! Round 7 / round 13 — MMX dispatch + semantics regression
//! tests.
//!
//! Round 7 routed every MMX opcode to a structured-trap variant
//! (`Trap::UnimplementedMmx`) so the trap log read as a clean
//! to-do list. Round 13 implements the semantics — the MMX
//! opcodes now run as real instructions on `Cpu::mmx[0..7]`.
//!
//! These tests assert the executed effect of a representative
//! sample of the MMX subset round 13 implements:
//!
//! * `MOVD mm, r32` — load a 32-bit GPR into the low lane.
//! * `MOVQ mm, mm` — copy a 64-bit MMX register.
//! * `PXOR mm, mm` — bitwise xor (zero-self idiom).
//! * `PADDB mm, mm` — packed byte add (wrapping per lane).
//! * `EMMS` — clear MMX state.
//! * Group-14 `PSLLQ mm, imm8` — shift the whole register by
//!   imm8.
//! * `PCMPGTB mm, mm` — signed-byte greater-than mask.
//!
//! Reference: Intel® 64 and IA-32 Architectures Software
//! Developer's Manual, Volume 2A/2B, per-instruction reference
//! pages.

use oxideav_vfw::emulator::isa_int::RET_SENTINEL;
use oxideav_vfw::emulator::mmu::{Mmu, Perm};
use oxideav_vfw::emulator::regs::Reg32;
use oxideav_vfw::emulator::Cpu;

/// Plant `bytes` at VA `0x1000` in a fresh MMU + map a small
/// stack at `0x9000_0000` and push the RET_SENTINEL so a
/// terminating `ret` at the end of the test program halts the
/// run cleanly.
fn make_cpu_with_stack(bytes: &[u8]) -> (Cpu, Mmu) {
    let mut mmu = Mmu::new();
    mmu.map(0x1000, 0x1000, Perm::R | Perm::W | Perm::X);
    mmu.write_initializer(0x1000, bytes).unwrap();
    mmu.map(0x9000_0000, 0x10_000, Perm::R | Perm::W);
    let mut cpu = Cpu::new();
    cpu.regs.eip = 0x1000;
    cpu.regs.set_esp(0x9000_F000);
    cpu.push32(&mut mmu, RET_SENTINEL).unwrap();
    (cpu, mmu)
}

#[test]
fn mmx_register_file_is_zero_initialised() {
    let cpu = Cpu::new();
    for (i, mm) in cpu.mmx.iter().enumerate() {
        assert_eq!(*mm, 0u64, "mm{i} should be zero on a fresh CPU");
    }
}

#[test]
fn mmx_register_file_is_writable() {
    let mut cpu = Cpu::new();
    cpu.mmx[3] = 0xDEAD_BEEF_F00D_FACE;
    assert_eq!(cpu.mmx[3], 0xDEAD_BEEF_F00D_FACE);
    for (i, mm) in cpu.mmx.iter().enumerate() {
        if i == 3 {
            continue;
        }
        assert_eq!(*mm, 0u64, "mm{i} should be untouched");
    }
}

/// `MOVD mm0, eax` (`0F 6E C0`) zero-extends `eax` into `mm0`.
#[test]
fn movd_mm0_eax_loads_low_lane() {
    // mov eax, 0xCAFEBABE  ; B8 BE BA FE CA
    // movd mm0, eax        ; 0F 6E C0
    // ret                  ; C3
    let (mut cpu, mut mmu) =
        make_cpu_with_stack(&[0xB8, 0xBE, 0xBA, 0xFE, 0xCA, 0x0F, 0x6E, 0xC0, 0xC3]);
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.mmx[0], 0xCAFE_BABE, "mm0 should hold zero-extended eax");
}

/// `MOVQ mm1, mm0` (`0F 6F C8`) copies one register to another.
#[test]
fn movq_mm1_mm0_copies_register() {
    let (mut cpu, mut mmu) = make_cpu_with_stack(&[0x0F, 0x6F, 0xC8, 0xC3]);
    cpu.mmx[0] = 0x1122_3344_5566_7788;
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.mmx[1], 0x1122_3344_5566_7788);
}

/// `PXOR mm0, mm0` (`0F EF C0`) zeroes mm0 — the canonical
/// MMX-register-init idiom.
#[test]
fn pxor_self_zeroes_register() {
    let (mut cpu, mut mmu) = make_cpu_with_stack(&[0x0F, 0xEF, 0xC0, 0xC3]);
    cpu.mmx[0] = 0xFFFF_FFFF_FFFF_FFFF;
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.mmx[0], 0);
}

/// `PADDB mm1, mm0` (`0F FC C8`) wraps per byte. With mm0 = 1s
/// and mm1 = 0xFF...FF, each lane wraps 0xFF + 1 = 0x00.
#[test]
fn paddb_wraps_per_lane() {
    let (mut cpu, mut mmu) = make_cpu_with_stack(&[0x0F, 0xFC, 0xC8, 0xC3]);
    cpu.mmx[0] = 0x0101_0101_0101_0101; // mm0 = all 0x01
    cpu.mmx[1] = 0xFFFF_FFFF_FFFF_FFFF; // mm1 = all 0xFF
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.mmx[1], 0); // each byte wraps to 0
}

/// `EMMS` (`0F 77`) clears all eight MMX registers.
#[test]
fn emms_clears_mmx_state() {
    let (mut cpu, mut mmu) = make_cpu_with_stack(&[0x0F, 0x77, 0xC3]);
    for i in 0..8 {
        cpu.mmx[i] = 0xAAAA_BBBB_CCCC_DDDD;
    }
    cpu.run(&mut mmu).unwrap();
    for (i, mm) in cpu.mmx.iter().enumerate() {
        assert_eq!(*mm, 0, "mm{i} should be zeroed by EMMS");
    }
}

/// Group-14 `PSLLQ mm0, 8` (`0F 73 F0 08`) shifts mm0 left by 8.
#[test]
fn psllq_imm_shifts_64_bits() {
    let (mut cpu, mut mmu) = make_cpu_with_stack(&[0x0F, 0x73, 0xF0, 0x08, 0xC3]);
    cpu.mmx[0] = 0x0000_0000_0000_00FF;
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.mmx[0], 0x0000_0000_0000_FF00);
}

/// `PCMPGTB mm0, mm1` (`0F 64 C1`) — per-byte signed compare.
/// With mm0 lanes = 1 (positive) and mm1 lanes = 0xFF (-1), each
/// lane sets to 0xFF (true).
#[test]
fn pcmpgtb_signed_compare() {
    let (mut cpu, mut mmu) = make_cpu_with_stack(&[0x0F, 0x64, 0xC1, 0xC3]);
    cpu.mmx[0] = 0x0101_0101_0101_0101;
    cpu.mmx[1] = 0xFFFF_FFFF_FFFF_FFFF; // -1 in each lane
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.mmx[0], 0xFFFF_FFFF_FFFF_FFFF);
}

/// Regression: BSWAP `0F C8` is *not* MMX and must still
/// execute integer-ISA semantics. (`0F C8..CF` swaps a GPR.)
#[test]
fn bswap_eax_still_works_after_mmx_routing() {
    // mov eax, 0x11223344 ; bswap eax ; ret
    let bytes = [0xB8, 0x44, 0x33, 0x22, 0x11, 0x0F, 0xC8, 0xC3];
    let (mut cpu, mut mmu) = make_cpu_with_stack(&bytes);
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.regs.get32(Reg32::Eax), 0x4433_2211);
}

/// `MOVQ mm/m64, mm` (`0F 7F`) round-trips a register through
/// memory: store mm0 to [eax], load it back into mm1.
#[test]
fn movq_to_memory_and_back_roundtrips() {
    // Layout @ 0x1000:
    //   mov eax, 0x9000_0100         ; B8 00 01 00 90
    //   movq [eax], mm0              ; 0F 7F 00
    //   movq mm1, [eax]              ; 0F 6F 08
    //   ret                          ; C3
    let bytes = [
        0xB8, 0x00, 0x01, 0x00, 0x90, 0x0F, 0x7F, 0x00, 0x0F, 0x6F, 0x08, 0xC3,
    ];
    let (mut cpu, mut mmu) = make_cpu_with_stack(&bytes);
    cpu.mmx[0] = 0xAABB_CCDD_EEFF_0011;
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.mmx[1], 0xAABB_CCDD_EEFF_0011);
    // And the value should be at [0x9000_0100] in memory.
    assert_eq!(mmu.load64(0x9000_0100).unwrap(), 0xAABB_CCDD_EEFF_0011);
}

/// `MOVD r/m32, mm` (`0F 7E C0`) — store mm0 low 32 bits into eax.
#[test]
fn movd_stores_low_lane_into_gpr() {
    let (mut cpu, mut mmu) = make_cpu_with_stack(&[0x0F, 0x7E, 0xC0, 0xC3]);
    cpu.mmx[0] = 0xCAFE_BABE_DEAD_BEEF;
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.regs.get32(Reg32::Eax), 0xDEAD_BEEF);
}

/// `mmx_dispatch_count` is the round-13 sentinel; verify it
/// counts every MMX dispatch even when the instructions
/// otherwise pass.
#[test]
fn mmx_dispatch_count_increments_per_opcode() {
    // pxor mm0, mm0  (0F EF C0)
    // movq mm1, mm0  (0F 6F C8)
    // emms           (0F 77)
    // ret            (C3)
    let (mut cpu, mut mmu) =
        make_cpu_with_stack(&[0x0F, 0xEF, 0xC0, 0x0F, 0x6F, 0xC8, 0x0F, 0x77, 0xC3]);
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.mmx_dispatch_count, 3);
}
