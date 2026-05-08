//! Round 21 — direct functional check of the x87 FPU
//! executor lit up in `src/emulator/isa_fpu.rs`.
//!
//! These are unit-level tests (one short hand-built code
//! sequence per test) that confirm the dispatcher routes
//! D8..DF correctly. A black-box test confirming the codec
//! CRT init now passes lives in
//! `tests/round21_mp43_decompress.rs`.

use oxideav_vfw::emulator::regs::Reg32;
use oxideav_vfw::emulator::{mmu::Perm, Cpu, Mmu};

fn make_env(code_bytes: &[u8]) -> (Cpu, Mmu) {
    let mut mmu = Mmu::new();
    mmu.map(0x1000, 0x1000, Perm::R | Perm::X);
    mmu.map(0x4000, 0x1000, Perm::R | Perm::W);
    mmu.write_initializer(0x1000, code_bytes).unwrap();
    let mut cpu = Cpu::new();
    cpu.regs.set_esp(0x4F00);
    cpu.regs.eip = 0x1000;
    (cpu, mmu)
}

#[test]
fn fld_qword_then_fstp_qword_round_trips_a_double() {
    // FLD qword [0x4100]
    // FSTP qword [0x4200]
    // RET
    let code: &[u8] = &[
        0xDD, 0x05, 0x00, 0x41, 0x00, 0x00, // dd /0 disp32
        0xDD, 0x1D, 0x00, 0x42, 0x00, 0x00, // dd /3 disp32
        0xC3,
    ];
    let (mut cpu, mut mmu) = make_env(code);
    let v = std::f64::consts::E;
    mmu.write_initializer(0x4100, &v.to_bits().to_le_bytes())
        .unwrap();
    // Push a return address sentinel so RET halts.
    let sentinel = 0xFFFF_FFF0u32;
    cpu.push32(&mut mmu, sentinel).unwrap();
    cpu.run(&mut mmu).unwrap();
    let lo = mmu.load32(0x4200).unwrap();
    let hi = mmu.load32(0x4204).unwrap();
    let bits = (u64::from(hi) << 32) | u64::from(lo);
    assert_eq!(f64::from_bits(bits), v);
}

#[test]
fn fld_dword_then_fstp_dword_round_trips_a_float() {
    // FLD dword [0x4100] (D9 /0)
    // FSTP dword [0x4200] (D9 /3)
    // RET
    let code: &[u8] = &[
        0xD9, 0x05, 0x00, 0x41, 0x00, 0x00, 0xD9, 0x1D, 0x00, 0x42, 0x00, 0x00, 0xC3,
    ];
    let (mut cpu, mut mmu) = make_env(code);
    let v: f32 = 3.5;
    mmu.write_initializer(0x4100, &v.to_bits().to_le_bytes())
        .unwrap();
    let sentinel = 0xFFFF_FFF0u32;
    cpu.push32(&mut mmu, sentinel).unwrap();
    cpu.run(&mut mmu).unwrap();
    let bits = mmu.load32(0x4200).unwrap();
    assert_eq!(f32::from_bits(bits), v);
}

#[test]
fn fadd_dword_into_st0_sums() {
    // FLD dword [0x4100]            ; ST(0) = 1.5
    // FADD dword [0x4104]           ; ST(0) += 2.25
    // FSTP dword [0x4200]           ; mem = 3.75
    // RET
    let code: &[u8] = &[
        0xD9, 0x05, 0x00, 0x41, 0x00, 0x00, // FLD m32
        0xD8, 0x05, 0x04, 0x41, 0x00, 0x00, // FADD m32  (D8 /0)
        0xD9, 0x1D, 0x00, 0x42, 0x00, 0x00, // FSTP m32
        0xC3,
    ];
    let (mut cpu, mut mmu) = make_env(code);
    mmu.write_initializer(0x4100, &1.5f32.to_bits().to_le_bytes())
        .unwrap();
    mmu.write_initializer(0x4104, &2.25f32.to_bits().to_le_bytes())
        .unwrap();
    let sentinel = 0xFFFF_FFF0u32;
    cpu.push32(&mut mmu, sentinel).unwrap();
    cpu.run(&mut mmu).unwrap();
    let bits = mmu.load32(0x4200).unwrap();
    assert_eq!(f32::from_bits(bits), 3.75);
}

#[test]
fn fild_then_fistp_roundtrips_signed_int32() {
    // FILD dword [0x4100]   (DB /0)
    // FISTP dword [0x4200]  (DB /3)
    // RET
    let code: &[u8] = &[
        0xDB, 0x05, 0x00, 0x41, 0x00, 0x00, 0xDB, 0x1D, 0x00, 0x42, 0x00, 0x00, 0xC3,
    ];
    let (mut cpu, mut mmu) = make_env(code);
    let v: i32 = -12345;
    mmu.write_initializer(0x4100, &v.to_le_bytes()).unwrap();
    let sentinel = 0xFFFF_FFF0u32;
    cpu.push32(&mut mmu, sentinel).unwrap();
    cpu.run(&mut mmu).unwrap();
    let out = mmu.load32(0x4200).unwrap() as i32;
    assert_eq!(out, v);
}

#[test]
fn fnstsw_ax_after_fld_zero_then_ftst_sets_c3() {
    // FLD dword [0x4100]   ; ST(0) = 0.0
    // FTST                 ; D9 E4 — compare ST(0) with 0
    // FNSTSW AX            ; DF E0
    // RET
    let code: &[u8] = &[
        0xD9, 0x05, 0x00, 0x41, 0x00, 0x00, // FLD
        0xD9, 0xE4, // FTST
        0xDF, 0xE0, // FNSTSW AX
        0xC3,
    ];
    let (mut cpu, mut mmu) = make_env(code);
    mmu.write_initializer(0x4100, &0.0f32.to_bits().to_le_bytes())
        .unwrap();
    let sentinel = 0xFFFF_FFF0u32;
    cpu.push32(&mut mmu, sentinel).unwrap();
    cpu.run(&mut mmu).unwrap();
    // C3 bit (1 << 14) signals "equal".
    let ax = cpu.regs.get32(Reg32::Eax) as u16;
    assert_ne!(ax & (1 << 14), 0, "C3 should be set");
}

#[test]
fn fxch_swaps_st0_and_st1() {
    // FLD dword [0x4100]    ; ST(0) = 1.5; ST(1) = ...
    // FLD dword [0x4104]    ; ST(0) = 2.5; ST(1) = 1.5
    // FXCH ST(1)            ; D9 C9 — swap
    // FSTP dword [0x4200]   ; → 1.5 popped
    // FSTP dword [0x4204]   ; → 2.5 popped
    // RET
    let code: &[u8] = &[
        0xD9, 0x05, 0x00, 0x41, 0x00, 0x00, 0xD9, 0x05, 0x04, 0x41, 0x00, 0x00, 0xD9, 0xC9, 0xD9,
        0x1D, 0x00, 0x42, 0x00, 0x00, 0xD9, 0x1D, 0x04, 0x42, 0x00, 0x00, 0xC3,
    ];
    let (mut cpu, mut mmu) = make_env(code);
    mmu.write_initializer(0x4100, &1.5f32.to_bits().to_le_bytes())
        .unwrap();
    mmu.write_initializer(0x4104, &2.5f32.to_bits().to_le_bytes())
        .unwrap();
    let sentinel = 0xFFFF_FFF0u32;
    cpu.push32(&mut mmu, sentinel).unwrap();
    cpu.run(&mut mmu).unwrap();
    let a = f32::from_bits(mmu.load32(0x4200).unwrap());
    let b = f32::from_bits(mmu.load32(0x4204).unwrap());
    assert_eq!(a, 1.5);
    assert_eq!(b, 2.5);
}

#[test]
fn fldcw_fnstcw_round_trip_through_shadow() {
    // FLDCW  word [0x4100]  (D9 /5)
    // FNSTCW word [0x4200]  (D9 /7)
    // RET
    let code: &[u8] = &[
        0xD9, 0x2D, 0x00, 0x41, 0x00, 0x00, 0xD9, 0x3D, 0x00, 0x42, 0x00, 0x00, 0xC3,
    ];
    let (mut cpu, mut mmu) = make_env(code);
    mmu.write_initializer(0x4100, &0x027Fu16.to_le_bytes())
        .unwrap();
    let sentinel = 0xFFFF_FFF0u32;
    cpu.push32(&mut mmu, sentinel).unwrap();
    cpu.run(&mut mmu).unwrap();
    let read_back = mmu.load16(0x4200).unwrap();
    assert_eq!(read_back, 0x027F);
}
