//! Round 7 — MMX scaffolding regression tests.
//!
//! The round-7 dispatch surface for the MMX opcode space
//! (`0F 60..6F`, `0F 70..7F`, `0F D0..FF`) routes every
//! recognised opcode to a structured-trap variant
//! ([`oxideav_vfw::emulator::Trap::UnimplementedMmx`]) instead
//! of the generic [`oxideav_vfw::emulator::Trap::UndefinedOpcode`].
//!
//! These tests assert:
//!
//! * The MMX register file `mm0..mm7` exists on `Cpu` and is
//!   zero-initialised.
//! * Each of a representative sample of MMX opcodes (one per
//!   block) traps as `UnimplementedMmx` carrying:
//!   - the full 2-byte opcode value `0x0Fxx`
//!   - the EIP of the leading `0F` byte
//!   - a non-empty mnemonic hint string.
//! * A non-MMX `0F` opcode (`0F C8 BSWAP eax`) still works, so
//!   the routing change did not regress integer ISA coverage.
//!
//! Reference: Intel® 64 and IA-32 Architectures Software
//! Developer's Manual, Volume 2, Appendix A Table A-3
//! ("Two-byte Opcode Map").
//!
//! Round 8 will land MMX semantics opcode-by-opcode by reading
//! this trap log and implementing each named mnemonic in turn.

use oxideav_vfw::emulator::mmu::{Mmu, Perm};
use oxideav_vfw::emulator::regs::Reg32;
use oxideav_vfw::emulator::{Cpu, Trap};

/// Plant `bytes` at VA `0x1000` in a fresh MMU + zero CPU and
/// return the pair. Code page is RWX so tests can patch easily.
fn make_cpu_at_1000(bytes: &[u8]) -> (Cpu, Mmu) {
    let mut mmu = Mmu::new();
    mmu.map(0x1000, 0x1000, Perm::R | Perm::W | Perm::X);
    mmu.write_initializer(0x1000, bytes).unwrap();
    let mut cpu = Cpu::new();
    cpu.regs.eip = 0x1000;
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
    // Other lanes untouched.
    for (i, mm) in cpu.mmx.iter().enumerate() {
        if i == 3 {
            continue;
        }
        assert_eq!(*mm, 0u64, "mm{i} should be untouched");
    }
}

/// `0F 60 C0` — PUNPCKLBW mm0, mm0. ModR/M = 0xC0 (mod=3,
/// reg=0, rm=0).
#[test]
fn punpcklbw_routes_to_structured_trap() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0x60, 0xC0]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            eip,
            opcode,
            mnemonic_hint,
        } => {
            assert_eq!(eip, 0x1000);
            assert_eq!(opcode, 0x0F60);
            assert_eq!(mnemonic_hint, "PUNPCKLBW MMX");
        }
        other => panic!("expected Trap::UnimplementedMmx, got {other:?}"),
    }
    // EIP should advance past 0F + opcode + ModR/M = 3 bytes.
    assert_eq!(cpu.regs.eip, 0x1003);
}

/// `0F 6E C0` — MOVD mm0, eax. ModR/M = 0xC0.
#[test]
fn movd_mmx_routes_to_structured_trap() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0x6E, 0xC0]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            opcode,
            mnemonic_hint,
            ..
        } => {
            assert_eq!(opcode, 0x0F6E);
            assert_eq!(mnemonic_hint, "MOVD MMX");
        }
        other => panic!("expected Trap::UnimplementedMmx, got {other:?}"),
    }
}

/// `0F 6F C8` — MOVQ mm1, mm0.
#[test]
fn movq_mmx_routes_to_structured_trap() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0x6F, 0xC8]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            opcode,
            mnemonic_hint,
            ..
        } => {
            assert_eq!(opcode, 0x0F6F);
            assert_eq!(mnemonic_hint, "MOVQ MMX");
        }
        other => panic!("expected Trap::UnimplementedMmx, got {other:?}"),
    }
}

/// `0F 70 C0 02` — PSHUFW mm0, mm0, 2. ModR/M + imm8.
#[test]
fn pshufw_consumes_modrm_and_imm8() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0x70, 0xC0, 0x02]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            opcode,
            mnemonic_hint,
            ..
        } => {
            assert_eq!(opcode, 0x0F70);
            assert_eq!(mnemonic_hint, "PSHUFW MMX");
        }
        other => panic!("expected UnimplementedMmx, got {other:?}"),
    }
    // 0F + 70 + ModR/M + imm8 = 4 bytes.
    assert_eq!(cpu.regs.eip, 0x1004);
}

/// `0F 73 F0 03` — PSLLQ mm0, 3 (group-14, /6 in ModR/M).
#[test]
fn pslq_imm_consumes_modrm_and_imm8() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0x73, 0xF0, 0x03]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            opcode,
            mnemonic_hint,
            ..
        } => {
            assert_eq!(opcode, 0x0F73);
            // mnemonic is the umbrella "group-14" string per
            // dispatch_mmx contract.
            assert!(
                mnemonic_hint.contains("group-14"),
                "expected group-14 hint, got {mnemonic_hint:?}"
            );
        }
        other => panic!("expected UnimplementedMmx, got {other:?}"),
    }
    assert_eq!(cpu.regs.eip, 0x1004);
}

/// `0F 77` — EMMS. No ModR/M, no imm8.
#[test]
fn emms_routes_to_structured_trap_with_no_modrm() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0x77]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            opcode,
            mnemonic_hint,
            ..
        } => {
            assert_eq!(opcode, 0x0F77);
            assert_eq!(mnemonic_hint, "EMMS");
        }
        other => panic!("expected UnimplementedMmx, got {other:?}"),
    }
    // EMMS has no ModR/M — only the 2 opcode bytes.
    assert_eq!(cpu.regs.eip, 0x1002);
}

/// `0F EF C0` — PXOR mm0, mm0.
#[test]
fn pxor_routes_to_structured_trap() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0xEF, 0xC0]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            opcode,
            mnemonic_hint,
            ..
        } => {
            assert_eq!(opcode, 0x0FEF);
            assert_eq!(mnemonic_hint, "PXOR MMX");
        }
        other => panic!("expected UnimplementedMmx, got {other:?}"),
    }
}

/// `0F FC C8` — PADDB mm1, mm0.
#[test]
fn paddb_routes_to_structured_trap() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0xFC, 0xC8]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            opcode,
            mnemonic_hint,
            ..
        } => {
            assert_eq!(opcode, 0x0FFC);
            assert_eq!(mnemonic_hint, "PADDB MMX");
        }
        other => panic!("expected UnimplementedMmx, got {other:?}"),
    }
}

/// `0F FE D9` — PADDD mm3, mm1.
#[test]
fn paddd_routes_to_structured_trap() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0xFE, 0xD9]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            opcode,
            mnemonic_hint,
            ..
        } => {
            assert_eq!(opcode, 0x0FFE);
            assert_eq!(mnemonic_hint, "PADDD MMX");
        }
        other => panic!("expected UnimplementedMmx, got {other:?}"),
    }
}

/// Regression: BSWAP `0F C8` is *not* MMX and must still
/// execute integer-ISA semantics. (`0F C8..CF` swaps a GPR.)
#[test]
fn bswap_eax_still_works_after_mmx_routing() {
    // mov eax, 0x11223344 ; bswap eax ; ret
    let bytes = [0xB8, 0x44, 0x33, 0x22, 0x11, 0x0F, 0xC8, 0xC3];
    let mut mmu = Mmu::new();
    mmu.map(0x1000, 0x1000, Perm::R | Perm::W | Perm::X);
    mmu.write_initializer(0x1000, &bytes).unwrap();
    // Stack for ret.
    mmu.map(0x9000_0000, 0x10_000, Perm::R | Perm::W);
    let mut cpu = Cpu::new();
    cpu.regs.eip = 0x1000;
    cpu.regs.set_esp(0x9000_F000);
    // Push the return sentinel.
    cpu.push32(&mut mmu, oxideav_vfw::emulator::isa_int::RET_SENTINEL)
        .unwrap();
    cpu.run(&mut mmu).unwrap();
    assert_eq!(cpu.regs.get32(Reg32::Eax), 0x4433_2211);
}

/// `0F D1 C0` — PSRLW mm0, mm0.
#[test]
fn psrlw_routes_to_structured_trap() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0xD1, 0xC0]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            opcode,
            mnemonic_hint,
            ..
        } => {
            assert_eq!(opcode, 0x0FD1);
            assert_eq!(mnemonic_hint, "PSRLW MMX");
        }
        other => panic!("expected UnimplementedMmx, got {other:?}"),
    }
}

/// `0F 64 C0` — PCMPGTB mm0, mm0.
#[test]
fn pcmpgtb_routes_to_structured_trap() {
    let (mut cpu, mut mmu) = make_cpu_at_1000(&[0x0F, 0x64, 0xC0]);
    let err = cpu.run(&mut mmu).unwrap_err();
    match err {
        Trap::UnimplementedMmx {
            opcode,
            mnemonic_hint,
            ..
        } => {
            assert_eq!(opcode, 0x0F64);
            assert_eq!(mnemonic_hint, "PCMPGTB MMX");
        }
        other => panic!("expected UnimplementedMmx, got {other:?}"),
    }
}
