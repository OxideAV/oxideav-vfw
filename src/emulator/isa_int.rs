//! i386 integer ISA executor.
//!
//! Each call to [`Cpu::step`] fetches one instruction at `eip`,
//! advances `eip`, and applies the instruction's effect on
//! [`Regs`] / [`Mmu`]. Multi-byte prefixes and ModR/M / SIB /
//! displacement decoding lives in [`super::decode`].
//!
//! This is **not** a complete i386 implementation. It covers
//! enough of the integer ISA for round-1's "load a DLL and run
//! through `DllMain(DLL_PROCESS_ATTACH)`" milestone. The unsupported
//! tail of the opcode space traps as [`Trap::UndefinedOpcode`] —
//! deliberately, so we discover what real codec DLLs need by
//! looking at the trap address and disassembling the surrounding
//! bytes.
//!
//! Reference: Intel® 64 and IA-32 Architectures Software
//! Developer's Manual, Volumes 2A + 2B (instruction set
//! reference), Volume 1 §3 (basic execution environment), Volume
//! 1 Appendix B (EFLAGS Cross-Reference).

use super::decode::{
    read_operand32, resolve_modrm32, sign_ext_8_to_32, write_operand32, ModRm, Operand,
};
use super::mmu::Mmu;
use super::regs::{Flags, Reg16, Reg32, Reg8, Regs};
use super::Trap;

/// Sentinel return address pushed by host-initiated calls. When
/// `eip` reaches the sentinel after a `ret`, [`Cpu::run`] stops.
pub const RET_SENTINEL: u32 = 0xFFFF_FFF0;

/// Default instruction limit for [`Cpu::run`] — guards against a
/// runaway codec. The limit is configurable via
/// [`Cpu::set_instr_limit`] for tests that intentionally execute
/// long sequences.
pub const DEFAULT_INSTR_LIMIT: u64 = 10_000_000;

/// Outcome of `step` when there is no trap. Mostly informational
/// — used by integration tests to confirm a known instruction
/// dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOk {
    /// Normal advance.
    Continued,
    /// `eip` reached the [`RET_SENTINEL`]. [`Cpu::run`] stops.
    Halted,
}

/// CPU instance — owns its own register file. The [`Mmu`] is
/// passed by mutable reference into every step so that one MMU
/// can be reused across multiple emulator instances.
pub struct Cpu {
    pub regs: Regs,
    pub instr_count: u64,
    instr_limit: u64,
    /// Operand-size override prefix (0x66) applies to one
    /// instruction. Stored as state so that decoding within one
    /// `step` call can examine it.
    op_size_16: bool,
    /// Address-size override prefix (0x67). 32-bit-mode default
    /// already gives us 32-bit addresses; this flag flips to
    /// 16-bit. Round-1 instructions don't exercise it; setting it
    /// is preserved for the future.
    addr_size_16: bool,
    /// REP / REPE / REPNE prefix.
    rep_prefix: Option<RepPrefix>,
    /// Active segment-override prefix for the current instruction.
    /// `None` after each `step` reset; set when the prefix loop
    /// consumes one of `0x26 / 0x2E / 0x36 / 0x3E / 0x64 / 0x65`.
    seg_override: Option<Seg>,
    /// Per-segment linear bases. In flat 32-bit Windows mode, all
    /// segment bases are 0 except FS (TEB) and GS (rarely used in
    /// Indeo-era code). The PE loader / Sandbox primes these via
    /// [`Cpu::set_fs_base`]. References:
    /// Intel SDM Vol. 1 §3.4.4 (segment registers in 32-bit
    /// flat mode); Microsoft "TEB" documentation for FS use.
    fs_base: u32,
    gs_base: u32,
}

/// Segment-override prefix selector.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Seg {
    Es,
    Cs,
    Ss,
    Ds,
    Fs,
    Gs,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum RepPrefix {
    Rep,   // 0xF3 — rep / repe (depends on instruction)
    Repne, // 0xF2 — repne / repnz
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            regs: Regs::new(),
            instr_count: 0,
            instr_limit: DEFAULT_INSTR_LIMIT,
            op_size_16: false,
            addr_size_16: false,
            rep_prefix: None,
            seg_override: None,
            fs_base: 0,
            gs_base: 0,
        }
    }

    /// Override the per-run instruction count limit.
    pub fn set_instr_limit(&mut self, n: u64) {
        self.instr_limit = n;
    }

    /// Configure the linear base of the FS segment. In 32-bit flat
    /// mode every segment base is 0 EXCEPT FS, which Windows uses
    /// to point at the current thread's TEB (Thread Environment
    /// Block). The runtime calls this after mapping the TEB.
    pub fn set_fs_base(&mut self, base: u32) {
        self.fs_base = base;
    }

    /// Configure the linear base of the GS segment. Almost always
    /// 0 in user-mode 32-bit Windows; the setter is provided for
    /// completeness.
    pub fn set_gs_base(&mut self, base: u32) {
        self.gs_base = base;
    }

    /// Translate an effective address according to the current
    /// segment-override prefix. Called by every memory-touching
    /// helper. Returns the final linear address.
    fn seg_translate(&self, ea: u32) -> u32 {
        match self.seg_override {
            Some(Seg::Fs) => ea.wrapping_add(self.fs_base),
            Some(Seg::Gs) => ea.wrapping_add(self.gs_base),
            // In 32-bit flat mode all other segment bases are 0,
            // so a CS/DS/ES/SS override is a no-op.
            _ => ea,
        }
    }

    /// Apply the active segment-override to an [`Operand`] —
    /// memory operands get `seg_base` added; register operands
    /// pass through unchanged. Use this on every operand returned
    /// from [`resolve_modrm32`].
    fn seg_apply(&self, op: Operand) -> Operand {
        match op {
            Operand::Mem32(a) => Operand::Mem32(self.seg_translate(a)),
            other => other,
        }
    }

    /// Run until the instruction at `eip` is the synthetic return
    /// sentinel (== [`RET_SENTINEL`]) or a trap is raised.
    pub fn run(&mut self, mmu: &mut Mmu) -> Result<(), Trap> {
        loop {
            if self.regs.eip == RET_SENTINEL {
                return Ok(());
            }
            if self.instr_count >= self.instr_limit {
                return Err(Trap::InstructionLimitExceeded {
                    eip: self.regs.eip,
                    count: self.instr_count,
                });
            }
            match self.step(mmu)? {
                StepOk::Continued => continue,
                StepOk::Halted => return Ok(()),
            }
        }
    }

    /// Push a 32-bit value onto the guest stack.
    pub fn push32(&mut self, mmu: &mut Mmu, value: u32) -> Result<(), Trap> {
        let new_esp = self.regs.esp().wrapping_sub(4);
        self.regs.set_esp(new_esp);
        mmu.store32(new_esp, value)
    }

    /// Pop a 32-bit value off the guest stack.
    pub fn pop32(&mut self, mmu: &mut Mmu) -> Result<u32, Trap> {
        let esp = self.regs.esp();
        let v = mmu.load32(esp)?;
        self.regs.set_esp(esp.wrapping_add(4));
        Ok(v)
    }

    /// Decode + execute one instruction. Returns
    /// [`StepOk::Halted`] when the instruction was a `ret` and
    /// the popped return address was [`RET_SENTINEL`].
    pub fn step(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        // Reset per-instruction prefix state.
        self.op_size_16 = false;
        self.addr_size_16 = false;
        self.rep_prefix = None;
        self.seg_override = None;

        let entry_eip = self.regs.eip;

        // Consume legacy prefixes (max ~4; we cap at 8 to avoid
        // adversarial loops).
        for _ in 0..8 {
            let b = mmu.fetch_x8(self.regs.eip)?;
            match b {
                0x66 => {
                    self.op_size_16 = true;
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                0x67 => {
                    self.addr_size_16 = true;
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                0xF3 => {
                    self.rep_prefix = Some(RepPrefix::Rep);
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                0xF2 => {
                    self.rep_prefix = Some(RepPrefix::Repne);
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                // Segment-override prefixes. In 32-bit flat mode
                // ES/CS/SS/DS bases are 0 (no-op); FS/GS bases are
                // primed by the runtime (FS → TEB, GS → user-data
                // base). All effective addresses computed for this
                // instruction get `seg_base` added in
                // [`Self::seg_translate`].
                0x26 => {
                    self.seg_override = Some(Seg::Es);
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                0x2E => {
                    self.seg_override = Some(Seg::Cs);
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                0x36 => {
                    self.seg_override = Some(Seg::Ss);
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                0x3E => {
                    self.seg_override = Some(Seg::Ds);
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                0x64 => {
                    self.seg_override = Some(Seg::Fs);
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                0x65 => {
                    self.seg_override = Some(Seg::Gs);
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                // Lock prefix — accepted; the relevant atomic
                // instructions are correct-by-construction in our
                // single-threaded interpreter.
                0xF0 => {
                    self.regs.eip = self.regs.eip.wrapping_add(1);
                }
                _ => break,
            }
        }

        let opcode = mmu.fetch_x8(self.regs.eip)?;
        self.regs.eip = self.regs.eip.wrapping_add(1);
        self.instr_count += 1;

        let res = self.dispatch(opcode, entry_eip, mmu);
        match res {
            Ok(s) => Ok(s),
            Err(Trap::UndefinedOpcode { opcode: op, .. }) => {
                // Restate the trap with the actual entry-eip.
                Err(Trap::UndefinedOpcode {
                    eip: entry_eip,
                    opcode: op,
                })
            }
            Err(e) => Err(e),
        }
    }

    fn dispatch(&mut self, op: u8, entry_eip: u32, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        match op {
            // ----------- ALU r/m, r and r, r/m (group on opcode bits) ------
            // 0x00..=0x05 : ADD
            0x00 => self.alu_rm8_r8(mmu, alu_add_8),
            0x01 => self.alu_rm32_r32(op, mmu, alu_add_32),
            0x02 => self.alu_r8_rm8(mmu, alu_add_8),
            0x03 => self.alu_r32_rm32(op, mmu, alu_add_32),
            0x04 => self.alu_al_imm8(mmu, alu_add_8),
            0x05 => self.alu_eax_imm32(mmu, alu_add_32),

            // 0x08..=0x0D : OR
            0x08 => self.alu_rm8_r8(mmu, alu_or_8),
            0x09 => self.alu_rm32_r32(op, mmu, alu_or_32),
            0x0A => self.alu_r8_rm8(mmu, alu_or_8),
            0x0B => self.alu_r32_rm32(op, mmu, alu_or_32),
            0x0C => self.alu_al_imm8(mmu, alu_or_8),
            0x0D => self.alu_eax_imm32(mmu, alu_or_32),

            // 0x10..=0x15 : ADC
            0x10 => self.alu_rm8_r8(mmu, alu_adc_8),
            0x11 => self.alu_rm32_r32(op, mmu, alu_adc_32),
            0x12 => self.alu_r8_rm8(mmu, alu_adc_8),
            0x13 => self.alu_r32_rm32(op, mmu, alu_adc_32),
            0x14 => self.alu_al_imm8(mmu, alu_adc_8),
            0x15 => self.alu_eax_imm32(mmu, alu_adc_32),

            // 0x18..=0x1D : SBB
            0x18 => self.alu_rm8_r8(mmu, alu_sbb_8),
            0x19 => self.alu_rm32_r32(op, mmu, alu_sbb_32),
            0x1A => self.alu_r8_rm8(mmu, alu_sbb_8),
            0x1B => self.alu_r32_rm32(op, mmu, alu_sbb_32),
            0x1C => self.alu_al_imm8(mmu, alu_sbb_8),
            0x1D => self.alu_eax_imm32(mmu, alu_sbb_32),

            // 0x20..=0x25 : AND
            0x20 => self.alu_rm8_r8(mmu, alu_and_8),
            0x21 => self.alu_rm32_r32(op, mmu, alu_and_32),
            0x22 => self.alu_r8_rm8(mmu, alu_and_8),
            0x23 => self.alu_r32_rm32(op, mmu, alu_and_32),
            0x24 => self.alu_al_imm8(mmu, alu_and_8),
            0x25 => self.alu_eax_imm32(mmu, alu_and_32),

            // 0x28..=0x2D : SUB
            0x28 => self.alu_rm8_r8(mmu, alu_sub_8),
            0x29 => self.alu_rm32_r32(op, mmu, alu_sub_32),
            0x2A => self.alu_r8_rm8(mmu, alu_sub_8),
            0x2B => self.alu_r32_rm32(op, mmu, alu_sub_32),
            0x2C => self.alu_al_imm8(mmu, alu_sub_8),
            0x2D => self.alu_eax_imm32(mmu, alu_sub_32),

            // 0x30..=0x35 : XOR
            0x30 => self.alu_rm8_r8(mmu, alu_xor_8),
            0x31 => self.alu_rm32_r32(op, mmu, alu_xor_32),
            0x32 => self.alu_r8_rm8(mmu, alu_xor_8),
            0x33 => self.alu_r32_rm32(op, mmu, alu_xor_32),
            0x34 => self.alu_al_imm8(mmu, alu_xor_8),
            0x35 => self.alu_eax_imm32(mmu, alu_xor_32),

            // 0x38..=0x3D : CMP
            0x38 => self.alu_rm8_r8(mmu, alu_cmp_8),
            0x39 => self.alu_rm32_r32(op, mmu, alu_cmp_32),
            0x3A => self.alu_r8_rm8(mmu, alu_cmp_8),
            0x3B => self.alu_r32_rm32(op, mmu, alu_cmp_32),
            0x3C => self.alu_al_imm8(mmu, alu_cmp_8),
            0x3D => self.alu_eax_imm32(mmu, alu_cmp_32),

            // ----------- INC/DEC r32 (single-byte forms 0x40..=0x4F) ------
            0x40..=0x47 => {
                let r = Reg32::from_bits(op - 0x40);
                let v = self.regs.get32(r);
                let (out, carry_unchanged) = (v.wrapping_add(1), self.regs.flags.cf);
                self.regs.set32(r, out);
                set_flags_inc_dec_32(&mut self.regs.flags, v, 1, out, /*sub*/ false);
                self.regs.flags.cf = carry_unchanged; // INC preserves CF
                Ok(StepOk::Continued)
            }
            0x48..=0x4F => {
                let r = Reg32::from_bits(op - 0x48);
                let v = self.regs.get32(r);
                let out = v.wrapping_sub(1);
                let carry_unchanged = self.regs.flags.cf;
                self.regs.set32(r, out);
                set_flags_inc_dec_32(&mut self.regs.flags, v, 1, out, /*sub*/ true);
                self.regs.flags.cf = carry_unchanged; // DEC preserves CF
                Ok(StepOk::Continued)
            }

            // 0x60 — PUSHAD: push eax, ecx, edx, ebx, original esp, ebp, esi, edi
            0x60 => {
                let original_esp = self.regs.esp();
                let to_push = [
                    self.regs.get32(Reg32::Eax),
                    self.regs.get32(Reg32::Ecx),
                    self.regs.get32(Reg32::Edx),
                    self.regs.get32(Reg32::Ebx),
                    original_esp,
                    self.regs.get32(Reg32::Ebp),
                    self.regs.get32(Reg32::Esi),
                    self.regs.get32(Reg32::Edi),
                ];
                for v in to_push.iter() {
                    self.push32(mmu, *v)?;
                }
                Ok(StepOk::Continued)
            }
            // 0x61 — POPAD: edi, esi, ebp, (skip original esp), ebx, edx, ecx, eax
            0x61 => {
                let edi = self.pop32(mmu)?;
                let esi = self.pop32(mmu)?;
                let ebp = self.pop32(mmu)?;
                let _skip_esp = self.pop32(mmu)?;
                let ebx = self.pop32(mmu)?;
                let edx = self.pop32(mmu)?;
                let ecx = self.pop32(mmu)?;
                let eax = self.pop32(mmu)?;
                self.regs.set32(Reg32::Edi, edi);
                self.regs.set32(Reg32::Esi, esi);
                self.regs.set32(Reg32::Ebp, ebp);
                self.regs.set32(Reg32::Ebx, ebx);
                self.regs.set32(Reg32::Edx, edx);
                self.regs.set32(Reg32::Ecx, ecx);
                self.regs.set32(Reg32::Eax, eax);
                Ok(StepOk::Continued)
            }

            // ----------- PUSH / POP r32 (0x50..=0x5F) ------
            0x50..=0x57 => {
                let r = Reg32::from_bits(op - 0x50);
                let v = self.regs.get32(r);
                self.push32(mmu, v)?;
                Ok(StepOk::Continued)
            }
            0x58..=0x5F => {
                let r = Reg32::from_bits(op - 0x58);
                let v = self.pop32(mmu)?;
                self.regs.set32(r, v);
                Ok(StepOk::Continued)
            }

            // PUSH imm32 (0x68) / PUSH imm8 (0x6A)
            0x68 => {
                let v = self.fetch_imm32(mmu)?;
                self.push32(mmu, v)?;
                Ok(StepOk::Continued)
            }
            0x6A => {
                let v = sign_ext_8_to_32(self.fetch_imm8(mmu)?);
                self.push32(mmu, v)?;
                Ok(StepOk::Continued)
            }

            // 0x69 — IMUL r32, r/m32, imm32
            0x69 => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (src_op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let src_op = self.seg_apply(src_op);
                let imm = self.fetch_imm32(mmu)? as i32 as i64;
                let dst = Reg32::from_bits(mr.reg);
                let a = read_operand32(src_op, &self.regs, mmu)? as i32 as i64;
                let prod = a.wrapping_mul(imm);
                let trunc = prod as i32 as u32;
                self.regs.set32(dst, trunc);
                let overflow = prod != prod as i32 as i64;
                self.regs.flags.cf = overflow;
                self.regs.flags.of = overflow;
                self.regs.flags.set_szp_32(trunc);
                Ok(StepOk::Continued)
            }
            // 0x6B — IMUL r32, r/m32, imm8 (sign-extended)
            0x6B => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (src_op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let src_op = self.seg_apply(src_op);
                let imm = sign_ext_8_to_32(self.fetch_imm8(mmu)?) as i32 as i64;
                let dst = Reg32::from_bits(mr.reg);
                let a = read_operand32(src_op, &self.regs, mmu)? as i32 as i64;
                let prod = a.wrapping_mul(imm);
                let trunc = prod as i32 as u32;
                self.regs.set32(dst, trunc);
                let overflow = prod != prod as i32 as i64;
                self.regs.flags.cf = overflow;
                self.regs.flags.of = overflow;
                self.regs.flags.set_szp_32(trunc);
                Ok(StepOk::Continued)
            }

            // ----------- Jcc rel8 (0x70..=0x7F) ------
            0x70..=0x7F => {
                let disp = sign_ext_8_to_32(self.fetch_imm8(mmu)?);
                let cond = condition_holds(op & 0x0F, &self.regs.flags);
                if cond {
                    self.regs.eip = self.regs.eip.wrapping_add(disp);
                }
                Ok(StepOk::Continued)
            }

            // ----------- 0x80 / 0x81 / 0x83 — group 1 ALU r/m, imm ------
            0x80 => self.group1_rm8_imm8(mmu),
            0x81 => self.group1_rm32_imm32(mmu),
            0x83 => self.group1_rm32_imm8(mmu),

            // ----------- 0x84 / 0x85 — TEST ------
            0x84 => self.test_rm8_r8(mmu),
            0x85 => self.alu_rm32_r32(op, mmu, alu_test_32),

            // 0x86 — XCHG r/m8, r8
            0x86 => {
                let mr = self.fetch_modrm(mmu)?;
                let (lhs, dst) = self.resolve_op8(mr, mmu)?;
                let rhs_reg = Reg8::from_bits(mr.reg);
                let rhs = self.regs.get8(rhs_reg);
                self.write_op8(dst, rhs, mmu)?;
                self.regs.set8(rhs_reg, lhs);
                Ok(StepOk::Continued)
            }
            // 0x87 — XCHG r/m32, r32
            0x87 => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (rm_op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let rm_op = self.seg_apply(rm_op);
                let rhs_reg = Reg32::from_bits(mr.reg);
                let lhs = read_operand32(rm_op, &self.regs, mmu)?;
                let rhs = self.regs.get32(rhs_reg);
                write_operand32(rm_op, rhs, &mut self.regs, mmu)?;
                self.regs.set32(rhs_reg, lhs);
                Ok(StepOk::Continued)
            }

            // ----------- 0x88..=0x8B — MOV r/m, r // r, r/m ------
            0x88 => self.mov_rm8_r8(mmu),
            0x89 => self.mov_rm32_r32(mmu),
            0x8A => self.mov_r8_rm8(mmu),
            0x8B => self.mov_r32_rm32(mmu),

            // 0x8D — LEA r32, m
            0x8D => self.lea_r32_m(mmu),

            // 0x8F /0 — POP r/m32
            0x8F => self.pop_rm32(mmu),

            // 0x90 — NOP (also XCHG eax, eax)
            0x90 => Ok(StepOk::Continued),

            // 0x91..=0x97 — XCHG eax, r32
            0x91..=0x97 => {
                let r = Reg32::from_bits(op - 0x90);
                let a = self.regs.get32(Reg32::Eax);
                let b = self.regs.get32(r);
                self.regs.set32(Reg32::Eax, b);
                self.regs.set32(r, a);
                Ok(StepOk::Continued)
            }

            // 0x98 — CWDE: sign-extend ax into eax
            0x98 => {
                let v = self.regs.get16(Reg16::Ax) as i16 as i32 as u32;
                self.regs.set32(Reg32::Eax, v);
                Ok(StepOk::Continued)
            }
            // 0x99 — CDQ: sign-extend eax into edx:eax
            0x99 => {
                let v = self.regs.get32(Reg32::Eax);
                let sign = if (v & 0x8000_0000) != 0 {
                    0xFFFF_FFFF
                } else {
                    0
                };
                self.regs.set32(Reg32::Edx, sign);
                Ok(StepOk::Continued)
            }

            // 0x9C — PUSHFD
            0x9C => {
                let v = self.regs.flags.pack();
                self.push32(mmu, v)?;
                Ok(StepOk::Continued)
            }
            // 0x9D — POPFD
            0x9D => {
                let v = self.pop32(mmu)?;
                self.regs.flags = Flags::unpack(v);
                Ok(StepOk::Continued)
            }
            // 0x9E — SAHF: load AH bits 0,2,4,6,7 → SF/ZF/AF/PF/CF
            0x9E => {
                let ah = self.regs.get8(Reg8::Ah);
                self.regs.flags.cf = (ah & 0x01) != 0;
                self.regs.flags.pf = (ah & 0x04) != 0;
                self.regs.flags.af = (ah & 0x10) != 0;
                self.regs.flags.zf = (ah & 0x40) != 0;
                self.regs.flags.sf = (ah & 0x80) != 0;
                Ok(StepOk::Continued)
            }
            // 0x9F — LAHF: store SF/ZF/AF/PF/CF (and bit-1 reserved=1) into AH
            0x9F => {
                let mut ah: u8 = 0b0000_0010; // bit 1 reads as 1
                if self.regs.flags.cf {
                    ah |= 0x01;
                }
                if self.regs.flags.pf {
                    ah |= 0x04;
                }
                if self.regs.flags.af {
                    ah |= 0x10;
                }
                if self.regs.flags.zf {
                    ah |= 0x40;
                }
                if self.regs.flags.sf {
                    ah |= 0x80;
                }
                self.regs.set8(Reg8::Ah, ah);
                Ok(StepOk::Continued)
            }

            // 0xA0 — MOV al, moffs8 ; 0xA1 — MOV eax, moffs32 ; A2/A3 inverse
            0xA0 => {
                let imm = self.fetch_imm32(mmu)?;
                let m = self.seg_translate(imm);
                let v = mmu.load8(m)?;
                self.regs.set8(Reg8::Al, v);
                Ok(StepOk::Continued)
            }
            0xA1 => {
                let imm = self.fetch_imm32(mmu)?;
                let m = self.seg_translate(imm);
                let v = mmu.load32(m)?;
                self.regs.set32(Reg32::Eax, v);
                Ok(StepOk::Continued)
            }
            0xA2 => {
                let imm = self.fetch_imm32(mmu)?;
                let m = self.seg_translate(imm);
                mmu.store8(m, self.regs.get8(Reg8::Al))?;
                Ok(StepOk::Continued)
            }
            0xA3 => {
                let imm = self.fetch_imm32(mmu)?;
                let m = self.seg_translate(imm);
                mmu.store32(m, self.regs.get32(Reg32::Eax))?;
                Ok(StepOk::Continued)
            }

            // 0xA4 — MOVSB ; 0xA5 — MOVSD
            0xA4 => self.string_movs(mmu, /*sized_dword*/ false),
            0xA5 => self.string_movs(mmu, /*sized_dword*/ true),

            // 0xA6 — CMPSB ; 0xA7 — CMPSD
            0xA6 => self.string_cmps(mmu, /*sized_dword*/ false),
            0xA7 => self.string_cmps(mmu, /*sized_dword*/ true),

            // 0xAA — STOSB ; 0xAB — STOSD
            0xAA => self.string_stos(mmu, /*sized_dword*/ false),
            0xAB => self.string_stos(mmu, /*sized_dword*/ true),

            // 0xAC — LODSB ; 0xAD — LODSD
            0xAC => self.string_lods(mmu, /*sized_dword*/ false),
            0xAD => self.string_lods(mmu, /*sized_dword*/ true),

            // 0xAE — SCASB ; 0xAF — SCASD
            0xAE => self.string_scas(mmu, /*sized_dword*/ false),
            0xAF => self.string_scas(mmu, /*sized_dword*/ true),

            // 0xA8 — TEST al, imm8
            0xA8 => {
                let imm = self.fetch_imm8(mmu)?;
                let res = self.regs.get8(Reg8::Al) & imm;
                self.regs.flags.cf = false;
                self.regs.flags.of = false;
                self.regs.flags.set_szp_8(res);
                Ok(StepOk::Continued)
            }
            // 0xA9 — TEST eax, imm32
            0xA9 => {
                let imm = self.fetch_imm32(mmu)?;
                let res = self.regs.get32(Reg32::Eax) & imm;
                self.regs.flags.cf = false;
                self.regs.flags.of = false;
                self.regs.flags.set_szp_32(res);
                Ok(StepOk::Continued)
            }

            // 0xB0..=0xB7 — MOV r8, imm8
            0xB0..=0xB7 => {
                let r = Reg8::from_bits(op - 0xB0);
                let imm = self.fetch_imm8(mmu)?;
                self.regs.set8(r, imm);
                Ok(StepOk::Continued)
            }
            // 0xB8..=0xBF — MOV r32, imm32
            0xB8..=0xBF => {
                let r = Reg32::from_bits(op - 0xB8);
                let imm = self.fetch_imm32(mmu)?;
                self.regs.set32(r, imm);
                Ok(StepOk::Continued)
            }

            // 0xC0 — Group 2 (shifts) r/m8, imm8
            0xC0 => self.group2_rm8(mmu, ShiftCount::Imm8),
            // 0xC1 — Group 2 (shifts) r/m32, imm8
            0xC1 => self.group2_rm32(mmu, ShiftCount::Imm8),
            // 0xD0 — Group 2 (shifts) r/m8, 1
            0xD0 => self.group2_rm8(mmu, ShiftCount::One),
            // 0xD1 — Group 2 (shifts) r/m32, 1
            0xD1 => self.group2_rm32(mmu, ShiftCount::One),
            // 0xD2 — Group 2 (shifts) r/m8, CL
            0xD2 => self.group2_rm8(mmu, ShiftCount::Cl),
            // 0xD3 — Group 2 (shifts) r/m32, CL
            0xD3 => self.group2_rm32(mmu, ShiftCount::Cl),

            // 0xC2 — RETN imm16 ; 0xC3 — RETN
            0xC2 => {
                let pop = self.fetch_imm16(mmu)?;
                let ret = self.pop32(mmu)?;
                self.regs
                    .set_esp(self.regs.esp().wrapping_add(u32::from(pop)));
                self.regs.eip = ret;
                if ret == RET_SENTINEL {
                    Ok(StepOk::Halted)
                } else {
                    Ok(StepOk::Continued)
                }
            }
            0xC3 => {
                let ret = self.pop32(mmu)?;
                self.regs.eip = ret;
                if ret == RET_SENTINEL {
                    Ok(StepOk::Halted)
                } else {
                    Ok(StepOk::Continued)
                }
            }

            // 0xC6 — MOV r/m8, imm8 (group 11 /0)
            0xC6 => {
                let mr = self.fetch_modrm(mmu)?;
                debug_assert!(mr.reg == 0, "group 11 /0");
                // The displacement (if any) follows the ModR/M
                // and *precedes* the imm8. Resolve the operand
                // first so eip advances over the disp, then fetch
                // the immediate.
                let (op_val, addr_or_reg) = self.resolve_op8(mr, mmu)?;
                let _ = op_val;
                let imm = self.fetch_imm8(mmu)?;
                self.write_op8(addr_or_reg, imm, mmu)?;
                Ok(StepOk::Continued)
            }
            // 0xC7 — MOV r/m32, imm32
            0xC7 => {
                let mr = self.fetch_modrm(mmu)?;
                debug_assert!(mr.reg == 0, "group 11 /0");
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let op = self.seg_apply(op);
                let imm = self.fetch_imm32(mmu)?;
                write_operand32(op, imm, &mut self.regs, mmu)?;
                Ok(StepOk::Continued)
            }

            // 0xC8 — ENTER imm16, imm8 (frame setup). We model the
            // common nesting=0 case fully; nesting>0 is rare and
            // pushes copied display links — handle it by tracing.
            0xC8 => {
                let alloc_size = self.fetch_imm16(mmu)?;
                let nesting_level = self.fetch_imm8(mmu)? & 0x1F;
                let frame_temp = self.regs.esp().wrapping_sub(4);
                self.push32(mmu, self.regs.ebp())?;
                if nesting_level == 0 {
                    self.regs.set32(Reg32::Ebp, frame_temp);
                    self.regs
                        .set_esp(self.regs.esp().wrapping_sub(u32::from(alloc_size)));
                } else {
                    let mut current_ebp = self.regs.ebp();
                    for _ in 1..nesting_level {
                        current_ebp = current_ebp.wrapping_sub(4);
                        let display = mmu.load32(current_ebp)?;
                        self.push32(mmu, display)?;
                    }
                    self.push32(mmu, frame_temp)?;
                    self.regs.set32(Reg32::Ebp, frame_temp);
                    self.regs
                        .set_esp(self.regs.esp().wrapping_sub(u32::from(alloc_size)));
                }
                Ok(StepOk::Continued)
            }
            // 0xC9 — LEAVE: mov esp, ebp; pop ebp
            0xC9 => {
                self.regs.set_esp(self.regs.ebp());
                let v = self.pop32(mmu)?;
                self.regs.set32(Reg32::Ebp, v);
                Ok(StepOk::Continued)
            }

            // 0xCC — INT 3 → trap. Codec DLLs do not use INT3
            // outside of debugger breakpoints, which the sandbox
            // cannot service.
            0xCC => Err(Trap::PrivilegedOpcode {
                eip: entry_eip,
                mnemonic: "int3",
            }),

            // 0xCD — INT imm8 → trap.
            0xCD => Err(Trap::PrivilegedOpcode {
                eip: entry_eip,
                mnemonic: "int imm8",
            }),

            // 0xCF — IRETD → trap.
            0xCF => Err(Trap::PrivilegedOpcode {
                eip: entry_eip,
                mnemonic: "iretd",
            }),

            // 0xE8 — CALL rel32
            0xE8 => {
                let disp = self.fetch_imm32(mmu)? as i32;
                let target = (self.regs.eip as i32).wrapping_add(disp) as u32;
                self.push32(mmu, self.regs.eip)?;
                self.regs.eip = target;
                Ok(StepOk::Continued)
            }
            // 0xE9 — JMP rel32
            0xE9 => {
                let disp = self.fetch_imm32(mmu)? as i32;
                self.regs.eip = (self.regs.eip as i32).wrapping_add(disp) as u32;
                Ok(StepOk::Continued)
            }
            // 0xEB — JMP rel8
            0xEB => {
                let disp = sign_ext_8_to_32(self.fetch_imm8(mmu)?);
                self.regs.eip = self.regs.eip.wrapping_add(disp);
                Ok(StepOk::Continued)
            }

            // 0xF4 — HLT → trap
            0xF4 => Err(Trap::PrivilegedOpcode {
                eip: entry_eip,
                mnemonic: "hlt",
            }),

            // 0xF5 — CMC : invert CF
            0xF5 => {
                self.regs.flags.cf = !self.regs.flags.cf;
                Ok(StepOk::Continued)
            }

            // 0xF6 — Group 3 (TEST/NOT/NEG/MUL/IMUL/DIV/IDIV) r/m8
            0xF6 => self.group3_rm8(mmu, entry_eip),
            // 0xF7 — Group 3 (NEG / NOT / MUL / IMUL / DIV / IDIV) r/m32
            0xF7 => self.group3_rm32(mmu, entry_eip),

            // 0xF8 — CLC ; 0xF9 — STC
            0xF8 => {
                self.regs.flags.cf = false;
                Ok(StepOk::Continued)
            }
            0xF9 => {
                self.regs.flags.cf = true;
                Ok(StepOk::Continued)
            }

            // 0xFA / 0xFB — CLI / STI → trap
            0xFA | 0xFB => Err(Trap::PrivilegedOpcode {
                eip: entry_eip,
                mnemonic: "cli/sti",
            }),

            // 0xFC — CLD ; 0xFD — STD
            0xFC => {
                self.regs.flags.df = false;
                Ok(StepOk::Continued)
            }
            0xFD => {
                self.regs.flags.df = true;
                Ok(StepOk::Continued)
            }

            // 0xFE — Group 4 (INC / DEC r/m8)
            0xFE => {
                let mr = self.fetch_modrm(mmu)?;
                let (val, dst) = self.resolve_op8(mr, mmu)?;
                match mr.reg {
                    0 => {
                        // INC: preserves CF
                        let r = val.wrapping_add(1);
                        let cf = self.regs.flags.cf;
                        self.regs.flags.af = ((val ^ 1 ^ r) & 0x10) != 0;
                        self.regs.flags.of = (((val ^ r) & (1u8 ^ r)) & 0x80) != 0;
                        self.regs.flags.set_szp_8(r);
                        self.regs.flags.cf = cf;
                        self.write_op8(dst, r, mmu)?;
                    }
                    1 => {
                        // DEC: preserves CF
                        let r = val.wrapping_sub(1);
                        let cf = self.regs.flags.cf;
                        self.regs.flags.af = ((val ^ 1 ^ r) & 0x10) != 0;
                        self.regs.flags.of = (((val ^ 1) & (val ^ r)) & 0x80) != 0;
                        self.regs.flags.set_szp_8(r);
                        self.regs.flags.cf = cf;
                        self.write_op8(dst, r, mmu)?;
                    }
                    other => {
                        return Err(Trap::UndefinedOpcode {
                            eip: entry_eip,
                            opcode: 0xFE00 | u32::from(other),
                        });
                    }
                }
                Ok(StepOk::Continued)
            }
            // 0xFF — Group 5 (INC / DEC / CALL / JMP / PUSH r/m32)
            0xFF => self.group5_rm32(mmu, entry_eip),

            // ----------- 0x0F two-byte escape ------
            0x0F => self.dispatch_0f(entry_eip, mmu),

            // Far-call / far-jmp / segment loads and other
            // non-supported single-byte opcodes trap.
            0x9A | 0xEA => Err(Trap::PrivilegedOpcode {
                eip: entry_eip,
                mnemonic: "far call/jmp",
            }),
            0x8E | 0x8C => Err(Trap::PrivilegedOpcode {
                eip: entry_eip,
                mnemonic: "mov sreg",
            }),

            other => Err(Trap::UndefinedOpcode {
                eip: entry_eip,
                opcode: u32::from(other),
            }),
        }
    }

    fn dispatch_0f(&mut self, entry_eip: u32, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let op2 = mmu.fetch_x8(self.regs.eip)?;
        self.regs.eip = self.regs.eip.wrapping_add(1);
        match op2 {
            // 0x0F 0x40..0x4F — CMOVcc r32, r/m32
            0x40..=0x4F => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (src_op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let src_op = self.seg_apply(src_op);
                let dst = Reg32::from_bits(mr.reg);
                let src = read_operand32(src_op, &self.regs, mmu)?;
                if condition_holds(op2 & 0x0F, &self.regs.flags) {
                    self.regs.set32(dst, src);
                }
                Ok(StepOk::Continued)
            }
            // 0x0F 0x80..0x8F — Jcc rel32
            0x80..=0x8F => {
                let disp = self.fetch_imm32(mmu)? as i32;
                if condition_holds(op2 & 0x0F, &self.regs.flags) {
                    self.regs.eip = (self.regs.eip as i32).wrapping_add(disp) as u32;
                }
                Ok(StepOk::Continued)
            }
            // 0x0F 0x90..0x9F — SETcc r/m8
            0x90..=0x9F => {
                let mr = self.fetch_modrm(mmu)?;
                let bit = condition_holds(op2 & 0x0F, &self.regs.flags) as u8;
                let (_v, dst) = self.resolve_op8(mr, mmu)?;
                self.write_op8(dst, bit, mmu)?;
                Ok(StepOk::Continued)
            }
            // 0x0F 0xA2 — CPUID
            0xA2 => {
                self.cpuid();
                Ok(StepOk::Continued)
            }
            // 0x0F 0xA3 — BT r/m32, r32
            0xA3 => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let op = self.seg_apply(op);
                let v = read_operand32(op, &self.regs, mmu)?;
                let bit = self.regs.get32(Reg32::from_bits(mr.reg)) & 31;
                self.regs.flags.cf = ((v >> bit) & 1) != 0;
                Ok(StepOk::Continued)
            }
            // 0x0F 0xA4 — SHLD r/m32, r32, imm8
            0xA4 => self.shld_imm(mmu),
            // 0x0F 0xA5 — SHLD r/m32, r32, CL
            0xA5 => self.shld_cl(mmu),
            // 0x0F 0xAC — SHRD r/m32, r32, imm8
            0xAC => self.shrd_imm(mmu),
            // 0x0F 0xAD — SHRD r/m32, r32, CL
            0xAD => self.shrd_cl(mmu),
            // 0x0F 0xAB — BTS r/m32, r32 (set bit, copy old value to CF)
            0xAB => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let op = self.seg_apply(op);
                let v = read_operand32(op, &self.regs, mmu)?;
                let bit = self.regs.get32(Reg32::from_bits(mr.reg)) & 31;
                self.regs.flags.cf = ((v >> bit) & 1) != 0;
                let new = v | (1u32 << bit);
                write_operand32(op, new, &mut self.regs, mmu)?;
                Ok(StepOk::Continued)
            }
            // 0x0F 0xB1 — CMPXCHG r/m32, r32
            0xB1 => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let op = self.seg_apply(op);
                let dest = read_operand32(op, &self.regs, mmu)?;
                let acc = self.regs.get32(Reg32::Eax);
                let src = self.regs.get32(Reg32::from_bits(mr.reg));
                let (_, _) = alu_sub_32(acc, dest, &mut self.regs.flags); // sets flags
                if acc == dest {
                    write_operand32(op, src, &mut self.regs, mmu)?;
                } else {
                    self.regs.set32(Reg32::Eax, dest);
                }
                Ok(StepOk::Continued)
            }
            // 0x0F 0xC1 — XADD r/m32, r32
            0xC1 => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let op = self.seg_apply(op);
                let dest = read_operand32(op, &self.regs, mmu)?;
                let src_reg = Reg32::from_bits(mr.reg);
                let src = self.regs.get32(src_reg);
                let (sum, _) = alu_add_32(dest, src, &mut self.regs.flags);
                self.regs.set32(src_reg, dest);
                write_operand32(op, sum, &mut self.regs, mmu)?;
                Ok(StepOk::Continued)
            }
            // 0x0F 0xC8..=0xCF — BSWAP r32
            0xC8..=0xCF => {
                let r = Reg32::from_bits(op2 - 0xC8);
                let v = self.regs.get32(r);
                self.regs.set32(r, v.swap_bytes());
                Ok(StepOk::Continued)
            }
            // 0x0F 0xAF — IMUL r32, r/m32
            0xAF => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let op = self.seg_apply(op);
                let dst = Reg32::from_bits(mr.reg);
                let a = self.regs.get32(dst) as i32 as i64;
                let b = read_operand32(op, &self.regs, mmu)? as i32 as i64;
                let prod = a.wrapping_mul(b);
                let trunc = prod as i32 as u32;
                self.regs.set32(dst, trunc);
                let overflow = prod != prod as i32 as i64;
                self.regs.flags.cf = overflow;
                self.regs.flags.of = overflow;
                self.regs.flags.set_szp_32(trunc);
                Ok(StepOk::Continued)
            }
            // 0x0F 0xB6 — MOVZX r32, r/m8
            0xB6 => {
                let mr = self.fetch_modrm(mmu)?;
                let dst = Reg32::from_bits(mr.reg);
                let (v, _) = self.resolve_op8(mr, mmu)?;
                self.regs.set32(dst, u32::from(v));
                Ok(StepOk::Continued)
            }
            // 0x0F 0xB7 — MOVZX r32, r/m16
            0xB7 => {
                let mr = self.fetch_modrm(mmu)?;
                let dst = Reg32::from_bits(mr.reg);
                let (v, _) = self.resolve_op16(mr, mmu)?;
                self.regs.set32(dst, u32::from(v));
                Ok(StepOk::Continued)
            }
            // 0x0F 0xBA — Group 8 (BT/BTS/BTR/BTC r/m32, imm8)
            0xBA => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let op = self.seg_apply(op);
                let v = read_operand32(op, &self.regs, mmu)?;
                let bit = u32::from(self.fetch_imm8(mmu)? & 0x1F);
                self.regs.flags.cf = ((v >> bit) & 1) != 0;
                let new = match mr.reg {
                    4 => return Ok(StepOk::Continued), // BT — flags only
                    5 => v | (1u32 << bit),            // BTS
                    6 => v & !(1u32 << bit),           // BTR
                    7 => v ^ (1u32 << bit),            // BTC
                    other => {
                        return Err(Trap::UndefinedOpcode {
                            eip: self.regs.eip,
                            opcode: 0x0F_BA00 | u32::from(other),
                        });
                    }
                };
                write_operand32(op, new, &mut self.regs, mmu)?;
                Ok(StepOk::Continued)
            }
            // 0x0F 0xBC — BSF r32, r/m32 ; 0xBD — BSR
            0xBC | 0xBD => {
                let mr = self.fetch_modrm(mmu)?;
                let bytes = self.peek_after_modrm(mmu, 16)?;
                let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
                self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
                let op = self.seg_apply(op);
                let dst = Reg32::from_bits(mr.reg);
                let v = read_operand32(op, &self.regs, mmu)?;
                if v == 0 {
                    self.regs.flags.zf = true;
                } else {
                    self.regs.flags.zf = false;
                    let idx = if op2 == 0xBC {
                        v.trailing_zeros()
                    } else {
                        31 - v.leading_zeros()
                    };
                    self.regs.set32(dst, idx);
                }
                Ok(StepOk::Continued)
            }
            // 0x0F 0xBE — MOVSX r32, r/m8 ; 0xBF — MOVSX r32, r/m16
            0xBE => {
                let mr = self.fetch_modrm(mmu)?;
                let dst = Reg32::from_bits(mr.reg);
                let (v, _) = self.resolve_op8(mr, mmu)?;
                self.regs.set32(dst, v as i8 as i32 as u32);
                Ok(StepOk::Continued)
            }
            0xBF => {
                let mr = self.fetch_modrm(mmu)?;
                let dst = Reg32::from_bits(mr.reg);
                let (v, _) = self.resolve_op16(mr, mmu)?;
                self.regs.set32(dst, v as i16 as i32 as u32);
                Ok(StepOk::Continued)
            }
            other => Err(Trap::UndefinedOpcode {
                eip: entry_eip,
                opcode: 0x0F00 | u32::from(other),
            }),
        }
    }

    /// CPUID — return a fixed Pentium-class response. Per design
    /// doc §"Instruction set" Phase 1: vendor "GenuineIntel", no
    /// SSE, no AMD extensions.
    fn cpuid(&mut self) {
        let leaf = self.regs.get32(Reg32::Eax);
        match leaf {
            0 => {
                // Max leaf = 1; vendor = "GenuineIntel" (ebx, edx, ecx)
                self.regs.set32(Reg32::Eax, 1);
                self.regs.set32(Reg32::Ebx, u32::from_le_bytes(*b"Genu"));
                self.regs.set32(Reg32::Edx, u32::from_le_bytes(*b"ineI"));
                self.regs.set32(Reg32::Ecx, u32::from_le_bytes(*b"ntel"));
            }
            1 => {
                // Pentium model (family 5, model 2, stepping 0).
                self.regs.set32(Reg32::Eax, (5 << 8) | (2 << 4));
                self.regs.set32(Reg32::Ebx, 0);
                self.regs.set32(Reg32::Ecx, 0);
                // Feature bits: FPU(0)+TSC(4)+CX8(8) only. No MMX,
                // no SSE, no SSE2 — round-1 surface.
                self.regs.set32(Reg32::Edx, (1 << 0) | (1 << 4) | (1 << 8));
            }
            _ => {
                self.regs.set32(Reg32::Eax, 0);
                self.regs.set32(Reg32::Ebx, 0);
                self.regs.set32(Reg32::Ecx, 0);
                self.regs.set32(Reg32::Edx, 0);
            }
        }
    }

    // ----- helpers ------------------------------------------------

    fn fetch_imm8(&mut self, mmu: &Mmu) -> Result<u8, Trap> {
        let v = mmu.fetch_x8(self.regs.eip)?;
        self.regs.eip = self.regs.eip.wrapping_add(1);
        Ok(v)
    }

    fn fetch_imm16(&mut self, mmu: &Mmu) -> Result<u16, Trap> {
        let lo = mmu.fetch_x8(self.regs.eip)?;
        let hi = mmu.fetch_x8(self.regs.eip.wrapping_add(1))?;
        self.regs.eip = self.regs.eip.wrapping_add(2);
        Ok(u16::from_le_bytes([lo, hi]))
    }

    fn fetch_imm32(&mut self, mmu: &Mmu) -> Result<u32, Trap> {
        let b0 = mmu.fetch_x8(self.regs.eip)?;
        let b1 = mmu.fetch_x8(self.regs.eip.wrapping_add(1))?;
        let b2 = mmu.fetch_x8(self.regs.eip.wrapping_add(2))?;
        let b3 = mmu.fetch_x8(self.regs.eip.wrapping_add(3))?;
        self.regs.eip = self.regs.eip.wrapping_add(4);
        Ok(u32::from_le_bytes([b0, b1, b2, b3]))
    }

    fn fetch_modrm(&mut self, mmu: &Mmu) -> Result<ModRm, Trap> {
        let b = self.fetch_imm8(mmu)?;
        Ok(ModRm::decode(b))
    }

    fn peek_after_modrm(&self, mmu: &Mmu, n: usize) -> Result<Vec<u8>, Trap> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            // We tolerate an unmapped/unreadable trailing byte
            // because we may have asked for more than is needed;
            // resolve_modrm32 only reads what the (mod, rm)
            // encoding actually requires. We still propagate
            // execute-protect faults from the first few bytes.
            match mmu.fetch_x8(self.regs.eip.wrapping_add(i as u32)) {
                Ok(b) => out.push(b),
                Err(_) if i >= 1 => break,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    fn resolve_op8(&mut self, mr: ModRm, mmu: &mut Mmu) -> Result<(u8, Op8Dst), Trap> {
        if mr.mode == 0b11 {
            let r = Reg8::from_bits(mr.rm);
            Ok((self.regs.get8(r), Op8Dst::Reg(r)))
        } else {
            let bytes = self.peek_after_modrm(mmu, 16)?;
            let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
            self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
            match self.seg_apply(op) {
                Operand::Reg32(_) => unreachable!("mod != 11 cannot be reg form"),
                Operand::Mem32(addr) => Ok((mmu.load8(addr)?, Op8Dst::Mem(addr))),
            }
        }
    }

    fn resolve_op16(&mut self, mr: ModRm, mmu: &mut Mmu) -> Result<(u16, Op16Dst), Trap> {
        if mr.mode == 0b11 {
            let r = Reg16::from_bits(mr.rm);
            Ok((self.regs.get16(r), Op16Dst::Reg(r)))
        } else {
            let bytes = self.peek_after_modrm(mmu, 16)?;
            let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
            self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
            match self.seg_apply(op) {
                Operand::Reg32(_) => unreachable!(),
                Operand::Mem32(addr) => Ok((mmu.load16(addr)?, Op16Dst::Mem(addr))),
            }
        }
    }

    fn write_op8(&mut self, dst: Op8Dst, value: u8, mmu: &mut Mmu) -> Result<(), Trap> {
        match dst {
            Op8Dst::Reg(r) => {
                self.regs.set8(r, value);
                Ok(())
            }
            Op8Dst::Mem(addr) => mmu.store8(addr, value),
        }
    }

    // ----- ALU dispatch helpers (32-bit) --------------------------

    fn alu_rm32_r32(&mut self, _op: u8, mmu: &mut Mmu, f: AluFn32) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (lhs_op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let lhs_op = self.seg_apply(lhs_op);
        let lhs = read_operand32(lhs_op, &self.regs, mmu)?;
        let rhs = self.regs.get32(Reg32::from_bits(mr.reg));
        let (result, write_back) = f(lhs, rhs, &mut self.regs.flags);
        if write_back {
            write_operand32(lhs_op, result, &mut self.regs, mmu)?;
        }
        Ok(StepOk::Continued)
    }

    fn alu_r32_rm32(&mut self, _op: u8, mmu: &mut Mmu, f: AluFn32) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (rhs_op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let rhs_op = self.seg_apply(rhs_op);
        let dst = Reg32::from_bits(mr.reg);
        let lhs = self.regs.get32(dst);
        let rhs = read_operand32(rhs_op, &self.regs, mmu)?;
        let (result, write_back) = f(lhs, rhs, &mut self.regs.flags);
        if write_back {
            self.regs.set32(dst, result);
        }
        Ok(StepOk::Continued)
    }

    fn alu_eax_imm32(&mut self, mmu: &Mmu, f: AluFn32) -> Result<StepOk, Trap> {
        let imm = self.fetch_imm32(mmu)?;
        let lhs = self.regs.get32(Reg32::Eax);
        let (result, write_back) = f(lhs, imm, &mut self.regs.flags);
        if write_back {
            self.regs.set32(Reg32::Eax, result);
        }
        Ok(StepOk::Continued)
    }

    // 0x80 — group 1 r/m8, imm8 (covers ADD/OR/ADC/SBB/AND/SUB/XOR/CMP)
    fn group1_rm8_imm8(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let (lhs, dst) = self.resolve_op8(mr, mmu)?;
        let imm = self.fetch_imm8(mmu)?;
        let (result, write_back) = group1_op_8(mr.reg, lhs, imm, &mut self.regs.flags);
        if write_back {
            self.write_op8(dst, result, mmu)?;
        }
        Ok(StepOk::Continued)
    }

    // 0x81 — group 1 r/m32, imm32
    fn group1_rm32_imm32(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (lhs_op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let lhs_op = self.seg_apply(lhs_op);
        let lhs = read_operand32(lhs_op, &self.regs, mmu)?;
        let imm = self.fetch_imm32(mmu)?;
        let (result, write_back) = group1_op_32(mr.reg, lhs, imm, &mut self.regs.flags);
        if write_back {
            write_operand32(lhs_op, result, &mut self.regs, mmu)?;
        }
        Ok(StepOk::Continued)
    }

    // 0x83 — group 1 r/m32, imm8 (sign-extended)
    fn group1_rm32_imm8(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (lhs_op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let lhs_op = self.seg_apply(lhs_op);
        let lhs = read_operand32(lhs_op, &self.regs, mmu)?;
        let imm = sign_ext_8_to_32(self.fetch_imm8(mmu)?);
        let (result, write_back) = group1_op_32(mr.reg, lhs, imm, &mut self.regs.flags);
        if write_back {
            write_operand32(lhs_op, result, &mut self.regs, mmu)?;
        }
        Ok(StepOk::Continued)
    }

    fn test_rm8_r8(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let (lhs, _dst) = self.resolve_op8(mr, mmu)?;
        let rhs = self.regs.get8(Reg8::from_bits(mr.reg));
        let res = lhs & rhs;
        self.regs.flags.cf = false;
        self.regs.flags.of = false;
        self.regs.flags.set_szp_8(res);
        Ok(StepOk::Continued)
    }

    fn mov_rm8_r8(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let (_v, dst) = self.resolve_op8(mr, mmu)?;
        let src = self.regs.get8(Reg8::from_bits(mr.reg));
        self.write_op8(dst, src, mmu)?;
        Ok(StepOk::Continued)
    }

    fn mov_rm32_r32(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let op = self.seg_apply(op);
        let src = self.regs.get32(Reg32::from_bits(mr.reg));
        write_operand32(op, src, &mut self.regs, mmu)?;
        Ok(StepOk::Continued)
    }

    fn mov_r8_rm8(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let (v, _dst) = self.resolve_op8(mr, mmu)?;
        self.regs.set8(Reg8::from_bits(mr.reg), v);
        Ok(StepOk::Continued)
    }

    fn mov_r32_rm32(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let op = self.seg_apply(op);
        let src = read_operand32(op, &self.regs, mmu)?;
        self.regs.set32(Reg32::from_bits(mr.reg), src);
        Ok(StepOk::Continued)
    }

    fn lea_r32_m(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        if mr.mode == 0b11 {
            return Err(Trap::UndefinedOpcode {
                eip: self.regs.eip,
                opcode: 0x8D,
            });
        }
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        // LEA computes the effective address WITHOUT applying any
        // segment-base — Intel SDM Vol. 2A LEA.
        let addr = match op {
            Operand::Mem32(a) => a,
            Operand::Reg32(_) => unreachable!(),
        };
        self.regs.set32(Reg32::from_bits(mr.reg), addr);
        Ok(StepOk::Continued)
    }

    fn pop_rm32(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        if mr.reg != 0 {
            return Err(Trap::UndefinedOpcode {
                eip: self.regs.eip,
                opcode: 0x8F,
            });
        }
        let v = self.pop32(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let op = self.seg_apply(op);
        write_operand32(op, v, &mut self.regs, mmu)?;
        Ok(StepOk::Continued)
    }

    fn group2_rm32(&mut self, mmu: &mut Mmu, source: ShiftCount) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let op = self.seg_apply(op);
        let val = read_operand32(op, &self.regs, mmu)?;
        let count = match source {
            ShiftCount::One => 1u32,
            ShiftCount::Cl => u32::from(self.regs.get8(Reg8::Cl)) & 0x1F,
            ShiftCount::Imm8 => u32::from(self.fetch_imm8(mmu)? & 0x1F),
        };
        let result = match mr.reg {
            // 0=ROL 1=ROR 2=RCL 3=RCR 4=SHL 5=SHR 6=SAL 7=SAR
            4 | 6 => {
                let r = if count >= 32 { 0 } else { val << count };
                if count != 0 {
                    self.regs.flags.cf = if count <= 32 {
                        (val >> (32 - count)) & 1 != 0
                    } else {
                        false
                    };
                    self.regs.flags.set_szp_32(r);
                }
                r
            }
            5 => {
                let r = if count >= 32 { 0 } else { val >> count };
                if count != 0 {
                    self.regs.flags.cf = ((val >> (count - 1)) & 1) != 0;
                    self.regs.flags.set_szp_32(r);
                }
                r
            }
            7 => {
                let signed = val as i32;
                let r = if count >= 32 {
                    if signed < 0 {
                        -1i32 as u32
                    } else {
                        0
                    }
                } else {
                    (signed >> count) as u32
                };
                if count != 0 {
                    self.regs.flags.cf = ((val >> (count - 1)) & 1) != 0;
                    self.regs.flags.set_szp_32(r);
                }
                r
            }
            0 => {
                // ROL
                let c = count % 32;
                let r = if c == 0 { val } else { val.rotate_left(c) };
                if count != 0 {
                    self.regs.flags.cf = (r & 1) != 0;
                }
                r
            }
            1 => {
                // ROR
                let c = count % 32;
                let r = if c == 0 { val } else { val.rotate_right(c) };
                if count != 0 {
                    self.regs.flags.cf = (r & 0x8000_0000) != 0;
                }
                r
            }
            other => {
                return Err(Trap::UndefinedOpcode {
                    eip: self.regs.eip,
                    opcode: 0xC100 | u32::from(other),
                })
            }
        };
        write_operand32(op, result, &mut self.regs, mmu)?;
        Ok(StepOk::Continued)
    }

    fn group3_rm32(&mut self, mmu: &mut Mmu, entry_eip: u32) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let op = self.seg_apply(op);
        let val = read_operand32(op, &self.regs, mmu)?;
        match mr.reg {
            0 | 1 => {
                // TEST r/m32, imm32
                let imm = self.fetch_imm32(mmu)?;
                let r = val & imm;
                self.regs.flags.cf = false;
                self.regs.flags.of = false;
                self.regs.flags.set_szp_32(r);
            }
            2 => {
                // NOT
                write_operand32(op, !val, &mut self.regs, mmu)?;
            }
            3 => {
                // NEG
                let r = 0u32.wrapping_sub(val);
                self.regs.flags.cf = val != 0;
                self.regs.flags.of = val == 0x8000_0000;
                self.regs.flags.set_szp_32(r);
                write_operand32(op, r, &mut self.regs, mmu)?;
            }
            4 => {
                // MUL eax, r/m32 → edx:eax
                let prod = u64::from(self.regs.get32(Reg32::Eax)) * u64::from(val);
                self.regs.set32(Reg32::Eax, prod as u32);
                self.regs.set32(Reg32::Edx, (prod >> 32) as u32);
                let hi_nonzero = (prod >> 32) != 0;
                self.regs.flags.cf = hi_nonzero;
                self.regs.flags.of = hi_nonzero;
            }
            5 => {
                // IMUL eax, r/m32 → edx:eax (signed)
                let prod = (self.regs.get32(Reg32::Eax) as i32 as i64) * (val as i32 as i64);
                self.regs.set32(Reg32::Eax, prod as u32);
                self.regs.set32(Reg32::Edx, (prod >> 32) as u32);
                let truncated = prod != prod as i32 as i64;
                self.regs.flags.cf = truncated;
                self.regs.flags.of = truncated;
            }
            6 => {
                // DIV r/m32: edx:eax / r/m32
                if val == 0 {
                    return Err(Trap::DivideByZero { eip: entry_eip });
                }
                let dividend = (u64::from(self.regs.get32(Reg32::Edx)) << 32)
                    | u64::from(self.regs.get32(Reg32::Eax));
                let q = dividend / u64::from(val);
                let r = dividend % u64::from(val);
                if q > u32::MAX as u64 {
                    return Err(Trap::DivideByZero { eip: entry_eip });
                }
                self.regs.set32(Reg32::Eax, q as u32);
                self.regs.set32(Reg32::Edx, r as u32);
            }
            7 => {
                // IDIV r/m32 (signed)
                if val == 0 {
                    return Err(Trap::DivideByZero { eip: entry_eip });
                }
                let dividend = (i64::from(self.regs.get32(Reg32::Edx) as i32) << 32)
                    | i64::from(self.regs.get32(Reg32::Eax));
                let divisor = val as i32 as i64;
                let q = dividend / divisor;
                let r = dividend % divisor;
                if !(i32::MIN as i64..=i32::MAX as i64).contains(&q) {
                    return Err(Trap::DivideByZero { eip: entry_eip });
                }
                self.regs.set32(Reg32::Eax, q as u32);
                self.regs.set32(Reg32::Edx, r as u32);
            }
            _ => unreachable!(),
        }
        Ok(StepOk::Continued)
    }

    // ----- ALU dispatch helpers (8-bit) ---------------------------

    fn alu_rm8_r8(&mut self, mmu: &mut Mmu, f: AluFn8) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let (lhs, dst) = self.resolve_op8(mr, mmu)?;
        let rhs = self.regs.get8(Reg8::from_bits(mr.reg));
        let (result, write_back) = f(lhs, rhs, &mut self.regs.flags);
        if write_back {
            self.write_op8(dst, result, mmu)?;
        }
        Ok(StepOk::Continued)
    }

    fn alu_r8_rm8(&mut self, mmu: &mut Mmu, f: AluFn8) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let (rhs, _dst) = self.resolve_op8(mr, mmu)?;
        let dst_reg = Reg8::from_bits(mr.reg);
        let lhs = self.regs.get8(dst_reg);
        let (result, write_back) = f(lhs, rhs, &mut self.regs.flags);
        if write_back {
            self.regs.set8(dst_reg, result);
        }
        Ok(StepOk::Continued)
    }

    fn alu_al_imm8(&mut self, mmu: &Mmu, f: AluFn8) -> Result<StepOk, Trap> {
        let imm = self.fetch_imm8(mmu)?;
        let lhs = self.regs.get8(Reg8::Al);
        let (result, write_back) = f(lhs, imm, &mut self.regs.flags);
        if write_back {
            self.regs.set8(Reg8::Al, result);
        }
        Ok(StepOk::Continued)
    }

    // ----- Group 2 (shifts) r/m8 ----------------------------------

    fn group2_rm8(&mut self, mmu: &mut Mmu, source: ShiftCount) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let (val, dst) = self.resolve_op8(mr, mmu)?;
        let count = match source {
            ShiftCount::One => 1u32,
            ShiftCount::Cl => u32::from(self.regs.get8(Reg8::Cl)) & 0x1F,
            ShiftCount::Imm8 => u32::from(self.fetch_imm8(mmu)? & 0x1F),
        };
        let val32 = u32::from(val);
        let result: u8 = match mr.reg {
            // 0=ROL 1=ROR 2=RCL 3=RCR 4=SHL 5=SHR 6=SAL 7=SAR
            4 | 6 => {
                let r = if count >= 8 { 0u32 } else { val32 << count };
                if count != 0 {
                    self.regs.flags.cf = if count <= 8 {
                        ((val32 >> (8 - count)) & 1) != 0
                    } else {
                        false
                    };
                    self.regs.flags.set_szp_8(r as u8);
                }
                r as u8
            }
            5 => {
                let r = if count >= 8 { 0u32 } else { val32 >> count };
                if count != 0 {
                    self.regs.flags.cf = ((val32 >> (count - 1)) & 1) != 0;
                    self.regs.flags.set_szp_8(r as u8);
                }
                r as u8
            }
            7 => {
                let signed = val as i8 as i32;
                let r = if count >= 8 {
                    if signed < 0 {
                        0xFFu8
                    } else {
                        0u8
                    }
                } else {
                    (signed >> count) as u8
                };
                if count != 0 {
                    self.regs.flags.cf = ((val32 >> (count - 1)) & 1) != 0;
                    self.regs.flags.set_szp_8(r);
                }
                r
            }
            0 => {
                // ROL r/m8
                let c = count % 8;
                let r = if c == 0 { val } else { val.rotate_left(c) };
                if count != 0 {
                    self.regs.flags.cf = (r & 1) != 0;
                }
                r
            }
            1 => {
                // ROR r/m8
                let c = count % 8;
                let r = if c == 0 { val } else { val.rotate_right(c) };
                if count != 0 {
                    self.regs.flags.cf = (r & 0x80) != 0;
                }
                r
            }
            other => {
                return Err(Trap::UndefinedOpcode {
                    eip: self.regs.eip,
                    opcode: 0xC000 | u32::from(other),
                });
            }
        };
        self.write_op8(dst, result, mmu)?;
        Ok(StepOk::Continued)
    }

    // ----- Group 3 r/m8 -------------------------------------------

    fn group3_rm8(&mut self, mmu: &mut Mmu, entry_eip: u32) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let (val, dst) = self.resolve_op8(mr, mmu)?;
        match mr.reg {
            0 | 1 => {
                // TEST r/m8, imm8
                let imm = self.fetch_imm8(mmu)?;
                let r = val & imm;
                self.regs.flags.cf = false;
                self.regs.flags.of = false;
                self.regs.flags.set_szp_8(r);
            }
            2 => {
                // NOT
                self.write_op8(dst, !val, mmu)?;
            }
            3 => {
                // NEG
                let r = 0u8.wrapping_sub(val);
                self.regs.flags.cf = val != 0;
                self.regs.flags.of = val == 0x80;
                self.regs.flags.set_szp_8(r);
                self.write_op8(dst, r, mmu)?;
            }
            4 => {
                // MUL al, r/m8 → ax
                let prod = u16::from(self.regs.get8(Reg8::Al)) * u16::from(val);
                self.regs.set16(Reg16::Ax, prod);
                let hi_nonzero = (prod & 0xFF00) != 0;
                self.regs.flags.cf = hi_nonzero;
                self.regs.flags.of = hi_nonzero;
            }
            5 => {
                // IMUL al, r/m8 → ax (signed)
                let prod = (self.regs.get8(Reg8::Al) as i8 as i16) * (val as i8 as i16);
                self.regs.set16(Reg16::Ax, prod as u16);
                let truncated = prod != prod as i8 as i16;
                self.regs.flags.cf = truncated;
                self.regs.flags.of = truncated;
            }
            6 => {
                // DIV ax / r/m8 → al = quotient, ah = remainder
                if val == 0 {
                    return Err(Trap::DivideByZero { eip: entry_eip });
                }
                let dividend = self.regs.get16(Reg16::Ax);
                let q = dividend / u16::from(val);
                let r = dividend % u16::from(val);
                if q > 0xFF {
                    return Err(Trap::DivideByZero { eip: entry_eip });
                }
                self.regs.set8(Reg8::Al, q as u8);
                self.regs.set8(Reg8::Ah, r as u8);
            }
            7 => {
                // IDIV ax / r/m8 (signed)
                if val == 0 {
                    return Err(Trap::DivideByZero { eip: entry_eip });
                }
                let dividend = self.regs.get16(Reg16::Ax) as i16;
                let divisor = val as i8 as i16;
                let q = dividend / divisor;
                let r = dividend % divisor;
                if !(i8::MIN as i16..=i8::MAX as i16).contains(&q) {
                    return Err(Trap::DivideByZero { eip: entry_eip });
                }
                self.regs.set8(Reg8::Al, q as u8);
                self.regs.set8(Reg8::Ah, r as u8);
            }
            _ => unreachable!(),
        }
        Ok(StepOk::Continued)
    }

    // ----- SHLD / SHRD --------------------------------------------

    fn shld_imm(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let op = self.seg_apply(op);
        let imm = u32::from(self.fetch_imm8(mmu)? & 0x1F);
        self.shld_apply(op, mr.reg, imm, mmu)
    }

    fn shld_cl(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let op = self.seg_apply(op);
        let count = u32::from(self.regs.get8(Reg8::Cl)) & 0x1F;
        self.shld_apply(op, mr.reg, count, mmu)
    }

    fn shld_apply(
        &mut self,
        op: Operand,
        reg_field: u8,
        count: u32,
        mmu: &mut Mmu,
    ) -> Result<StepOk, Trap> {
        let dest = read_operand32(op, &self.regs, mmu)?;
        let src = self.regs.get32(Reg32::from_bits(reg_field));
        if count == 0 {
            return Ok(StepOk::Continued);
        }
        // Per Intel SDM Vol 2A SHLD entry: bits shifted left in;
        // src provides the in-shifting bits from the right. Last
        // bit shifted out goes to CF.
        let combined: u64 = (u64::from(dest) << 32) | u64::from(src);
        let shifted = combined << count;
        let result = (shifted >> 32) as u32;
        self.regs.flags.cf = ((dest >> (32 - count)) & 1) != 0;
        self.regs.flags.set_szp_32(result);
        write_operand32(op, result, &mut self.regs, mmu)?;
        Ok(StepOk::Continued)
    }

    fn shrd_imm(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let op = self.seg_apply(op);
        let imm = u32::from(self.fetch_imm8(mmu)? & 0x1F);
        self.shrd_apply(op, mr.reg, imm, mmu)
    }

    fn shrd_cl(&mut self, mmu: &mut Mmu) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let op = self.seg_apply(op);
        let count = u32::from(self.regs.get8(Reg8::Cl)) & 0x1F;
        self.shrd_apply(op, mr.reg, count, mmu)
    }

    fn shrd_apply(
        &mut self,
        op: Operand,
        reg_field: u8,
        count: u32,
        mmu: &mut Mmu,
    ) -> Result<StepOk, Trap> {
        let dest = read_operand32(op, &self.regs, mmu)?;
        let src = self.regs.get32(Reg32::from_bits(reg_field));
        if count == 0 {
            return Ok(StepOk::Continued);
        }
        // SHRD: shift `dest` right with bits from `src` shifted in
        // from the left. Last bit shifted out goes to CF.
        let combined: u64 = (u64::from(src) << 32) | u64::from(dest);
        let shifted = combined >> count;
        let result = shifted as u32;
        self.regs.flags.cf = ((dest >> (count - 1)) & 1) != 0;
        self.regs.flags.set_szp_32(result);
        write_operand32(op, result, &mut self.regs, mmu)?;
        Ok(StepOk::Continued)
    }

    // ----- String operations --------------------------------------
    //
    // Reference: Intel SDM Vol. 2 — entries for MOVS, CMPS, STOS,
    // LODS, SCAS. The DF flag determines pre-step direction. REP
    // / REPE / REPNE prefixes are honoured here; we model the
    // single-step semantics in helpers and loop only for REP.

    fn string_movs(&mut self, mmu: &mut Mmu, sized_dword: bool) -> Result<StepOk, Trap> {
        let step = self.string_step(sized_dword);
        let do_one = |this: &mut Self, mmu: &mut Mmu| -> Result<(), Trap> {
            // Source uses DS (overridable); destination uses ES
            // (NOT overridable). seg_translate captures the
            // override on the source side; ES base is 0 in flat
            // 32-bit mode so the destination is unmodified.
            let src = this.seg_translate(this.regs.get32(Reg32::Esi));
            let dst = this.regs.get32(Reg32::Edi);
            if sized_dword {
                let v = mmu.load32(src)?;
                mmu.store32(dst, v)?;
            } else {
                let v = mmu.load8(src)?;
                mmu.store8(dst, v)?;
            }
            this.regs.set32(
                Reg32::Esi,
                this.regs.get32(Reg32::Esi).wrapping_add(step as u32),
            );
            this.regs.set32(
                Reg32::Edi,
                this.regs.get32(Reg32::Edi).wrapping_add(step as u32),
            );
            Ok(())
        };
        self.string_loop(mmu, do_one, /*compare*/ false)
    }

    fn string_stos(&mut self, mmu: &mut Mmu, sized_dword: bool) -> Result<StepOk, Trap> {
        let step = self.string_step(sized_dword);
        let do_one = |this: &mut Self, mmu: &mut Mmu| -> Result<(), Trap> {
            let dst = this.regs.get32(Reg32::Edi);
            if sized_dword {
                mmu.store32(dst, this.regs.get32(Reg32::Eax))?;
            } else {
                mmu.store8(dst, this.regs.get8(Reg8::Al))?;
            }
            this.regs.set32(Reg32::Edi, dst.wrapping_add(step as u32));
            Ok(())
        };
        self.string_loop(mmu, do_one, /*compare*/ false)
    }

    fn string_lods(&mut self, mmu: &mut Mmu, sized_dword: bool) -> Result<StepOk, Trap> {
        let step = self.string_step(sized_dword);
        let do_one = |this: &mut Self, mmu: &mut Mmu| -> Result<(), Trap> {
            let src = this.regs.get32(Reg32::Esi);
            if sized_dword {
                let v = mmu.load32(src)?;
                this.regs.set32(Reg32::Eax, v);
            } else {
                let v = mmu.load8(src)?;
                this.regs.set8(Reg8::Al, v);
            }
            this.regs.set32(Reg32::Esi, src.wrapping_add(step as u32));
            Ok(())
        };
        self.string_loop(mmu, do_one, /*compare*/ false)
    }

    fn string_cmps(&mut self, mmu: &mut Mmu, sized_dword: bool) -> Result<StepOk, Trap> {
        let step = self.string_step(sized_dword);
        let do_one = |this: &mut Self, mmu: &mut Mmu| -> Result<(), Trap> {
            let src = this.regs.get32(Reg32::Esi);
            let dst = this.regs.get32(Reg32::Edi);
            if sized_dword {
                let a = mmu.load32(src)?;
                let b = mmu.load32(dst)?;
                let _ = alu_sub_32(a, b, &mut this.regs.flags);
            } else {
                let a = mmu.load8(src)?;
                let b = mmu.load8(dst)?;
                let _ = alu_sub_8(a, b, &mut this.regs.flags);
            }
            this.regs.set32(Reg32::Esi, src.wrapping_add(step as u32));
            this.regs.set32(Reg32::Edi, dst.wrapping_add(step as u32));
            Ok(())
        };
        self.string_loop(mmu, do_one, /*compare*/ true)
    }

    fn string_scas(&mut self, mmu: &mut Mmu, sized_dword: bool) -> Result<StepOk, Trap> {
        let step = self.string_step(sized_dword);
        let do_one = |this: &mut Self, mmu: &mut Mmu| -> Result<(), Trap> {
            let dst = this.regs.get32(Reg32::Edi);
            if sized_dword {
                let v = mmu.load32(dst)?;
                let acc = this.regs.get32(Reg32::Eax);
                let _ = alu_sub_32(acc, v, &mut this.regs.flags);
            } else {
                let v = mmu.load8(dst)?;
                let acc = this.regs.get8(Reg8::Al);
                let _ = alu_sub_8(acc, v, &mut this.regs.flags);
            }
            this.regs.set32(Reg32::Edi, dst.wrapping_add(step as u32));
            Ok(())
        };
        self.string_loop(mmu, do_one, /*compare*/ true)
    }

    /// +1 or -1 depending on DF and operand size. (Sign-extended
    /// later via `as u32` so wrapping_add does the right thing.)
    fn string_step(&self, sized_dword: bool) -> i32 {
        let inc: i32 = if sized_dword { 4 } else { 1 };
        if self.regs.flags.df {
            -inc
        } else {
            inc
        }
    }

    /// Common REP/REPE/REPNE wrapper. `compare` selects the
    /// repeat-while-flag termination semantics for CMPS/SCAS.
    fn string_loop(
        &mut self,
        mmu: &mut Mmu,
        mut step: impl FnMut(&mut Self, &mut Mmu) -> Result<(), Trap>,
        compare: bool,
    ) -> Result<StepOk, Trap> {
        match self.rep_prefix {
            None => {
                step(self, mmu)?;
                Ok(StepOk::Continued)
            }
            Some(rep) => {
                while self.regs.get32(Reg32::Ecx) != 0 {
                    step(self, mmu)?;
                    let new_ecx = self.regs.get32(Reg32::Ecx).wrapping_sub(1);
                    self.regs.set32(Reg32::Ecx, new_ecx);
                    if compare {
                        let zf = self.regs.flags.zf;
                        let stop = match rep {
                            RepPrefix::Rep => !zf,  // REPE: stop if !ZF
                            RepPrefix::Repne => zf, // REPNE: stop if ZF
                        };
                        if stop {
                            break;
                        }
                    }
                }
                Ok(StepOk::Continued)
            }
        }
    }

    fn group5_rm32(&mut self, mmu: &mut Mmu, entry_eip: u32) -> Result<StepOk, Trap> {
        let mr = self.fetch_modrm(mmu)?;
        let bytes = self.peek_after_modrm(mmu, 16)?;
        let (op, consumed) = resolve_modrm32(mr, &bytes, &self.regs)?;
        self.regs.eip = self.regs.eip.wrapping_add(consumed as u32);
        let op = self.seg_apply(op);
        let val = read_operand32(op, &self.regs, mmu)?;
        match mr.reg {
            0 => {
                let r = val.wrapping_add(1);
                let cf = self.regs.flags.cf;
                set_flags_inc_dec_32(&mut self.regs.flags, val, 1, r, false);
                self.regs.flags.cf = cf;
                write_operand32(op, r, &mut self.regs, mmu)?;
            }
            1 => {
                let r = val.wrapping_sub(1);
                let cf = self.regs.flags.cf;
                set_flags_inc_dec_32(&mut self.regs.flags, val, 1, r, true);
                self.regs.flags.cf = cf;
                write_operand32(op, r, &mut self.regs, mmu)?;
            }
            2 => {
                // CALL r/m32 (near, absolute)
                self.push32(mmu, self.regs.eip)?;
                self.regs.eip = val;
            }
            3 | 5 => {
                return Err(Trap::PrivilegedOpcode {
                    eip: entry_eip,
                    mnemonic: "far call/jmp m",
                })
            }
            4 => {
                // JMP r/m32 (near, absolute)
                self.regs.eip = val;
            }
            6 => {
                // PUSH r/m32
                self.push32(mmu, val)?;
            }
            _ => {
                return Err(Trap::UndefinedOpcode {
                    eip: entry_eip,
                    opcode: 0xFF00 | u32::from(mr.reg),
                })
            }
        }
        Ok(StepOk::Continued)
    }
}

#[derive(Copy, Clone, Debug)]
enum Op8Dst {
    Reg(Reg8),
    Mem(u32),
}

#[derive(Copy, Clone, Debug)]
enum Op16Dst {
    #[allow(dead_code)]
    Reg(Reg16),
    #[allow(dead_code)]
    Mem(u32),
}

/// Shift-count source for Group-2 (rotate / shift) opcodes.
#[derive(Copy, Clone, Debug)]
enum ShiftCount {
    /// Implicit count of 1 (`D0` / `D1`).
    One,
    /// Count from the `CL` register (`D2` / `D3`).
    Cl,
    /// 8-bit immediate following the ModR/M (`C0` / `C1`).
    Imm8,
}

// Helper signature: (lhs, rhs, flags) -> (result, write_back).
type AluFn32 = fn(u32, u32, &mut Flags) -> (u32, bool);
type AluFn8 = fn(u8, u8, &mut Flags) -> (u8, bool);

fn alu_add_32(lhs: u32, rhs: u32, f: &mut Flags) -> (u32, bool) {
    let (result, carry) = lhs.overflowing_add(rhs);
    f.cf = carry;
    f.af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    f.of = (((lhs ^ result) & (rhs ^ result)) & 0x8000_0000) != 0;
    f.set_szp_32(result);
    (result, true)
}

fn alu_sub_32(lhs: u32, rhs: u32, f: &mut Flags) -> (u32, bool) {
    let (result, borrow) = lhs.overflowing_sub(rhs);
    f.cf = borrow;
    f.af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    f.of = (((lhs ^ rhs) & (lhs ^ result)) & 0x8000_0000) != 0;
    f.set_szp_32(result);
    (result, true)
}

fn alu_cmp_32(lhs: u32, rhs: u32, f: &mut Flags) -> (u32, bool) {
    let (result, _w) = alu_sub_32(lhs, rhs, f);
    (result, false)
}

fn alu_and_32(lhs: u32, rhs: u32, f: &mut Flags) -> (u32, bool) {
    let r = lhs & rhs;
    f.cf = false;
    f.of = false;
    f.set_szp_32(r);
    (r, true)
}

fn alu_or_32(lhs: u32, rhs: u32, f: &mut Flags) -> (u32, bool) {
    let r = lhs | rhs;
    f.cf = false;
    f.of = false;
    f.set_szp_32(r);
    (r, true)
}

fn alu_xor_32(lhs: u32, rhs: u32, f: &mut Flags) -> (u32, bool) {
    let r = lhs ^ rhs;
    f.cf = false;
    f.of = false;
    f.set_szp_32(r);
    (r, true)
}

fn alu_test_32(lhs: u32, rhs: u32, f: &mut Flags) -> (u32, bool) {
    let r = lhs & rhs;
    f.cf = false;
    f.of = false;
    f.set_szp_32(r);
    (r, false)
}

fn alu_adc_32(lhs: u32, rhs: u32, f: &mut Flags) -> (u32, bool) {
    let cf = f.cf as u32;
    let s1 = lhs.wrapping_add(rhs);
    let result = s1.wrapping_add(cf);
    let new_cf = result < lhs || (cf == 1 && rhs == u32::MAX);
    f.cf = new_cf;
    f.af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    f.of = (((lhs ^ result) & (rhs ^ result)) & 0x8000_0000) != 0;
    f.set_szp_32(result);
    (result, true)
}

fn alu_sbb_32(lhs: u32, rhs: u32, f: &mut Flags) -> (u32, bool) {
    let cf = f.cf as u32;
    let s1 = lhs.wrapping_sub(rhs);
    let result = s1.wrapping_sub(cf);
    // Borrow: lhs < rhs + cf (extended).
    let total = u64::from(rhs) + u64::from(cf);
    f.cf = u64::from(lhs) < total;
    f.af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    f.of = (((lhs ^ rhs) & (lhs ^ result)) & 0x8000_0000) != 0;
    f.set_szp_32(result);
    (result, true)
}

// ----- 8-bit ALU primitives -----------------------------------

fn alu_add_8(lhs: u8, rhs: u8, f: &mut Flags) -> (u8, bool) {
    let (result, carry) = lhs.overflowing_add(rhs);
    f.cf = carry;
    f.af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    f.of = (((lhs ^ result) & (rhs ^ result)) & 0x80) != 0;
    f.set_szp_8(result);
    (result, true)
}

fn alu_sub_8(lhs: u8, rhs: u8, f: &mut Flags) -> (u8, bool) {
    let (result, borrow) = lhs.overflowing_sub(rhs);
    f.cf = borrow;
    f.af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    f.of = (((lhs ^ rhs) & (lhs ^ result)) & 0x80) != 0;
    f.set_szp_8(result);
    (result, true)
}

fn alu_cmp_8(lhs: u8, rhs: u8, f: &mut Flags) -> (u8, bool) {
    let (_r, _w) = alu_sub_8(lhs, rhs, f);
    (0, false)
}

fn alu_and_8(lhs: u8, rhs: u8, f: &mut Flags) -> (u8, bool) {
    let r = lhs & rhs;
    f.cf = false;
    f.of = false;
    f.set_szp_8(r);
    (r, true)
}

fn alu_or_8(lhs: u8, rhs: u8, f: &mut Flags) -> (u8, bool) {
    let r = lhs | rhs;
    f.cf = false;
    f.of = false;
    f.set_szp_8(r);
    (r, true)
}

fn alu_xor_8(lhs: u8, rhs: u8, f: &mut Flags) -> (u8, bool) {
    let r = lhs ^ rhs;
    f.cf = false;
    f.of = false;
    f.set_szp_8(r);
    (r, true)
}

fn alu_adc_8(lhs: u8, rhs: u8, f: &mut Flags) -> (u8, bool) {
    let cf = f.cf as u8;
    let s1 = lhs.wrapping_add(rhs);
    let result = s1.wrapping_add(cf);
    let total = u16::from(rhs) + u16::from(cf);
    f.cf = u16::from(lhs) + total > 0xFF;
    f.af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    f.of = (((lhs ^ result) & (rhs ^ result)) & 0x80) != 0;
    f.set_szp_8(result);
    (result, true)
}

fn alu_sbb_8(lhs: u8, rhs: u8, f: &mut Flags) -> (u8, bool) {
    let cf = f.cf as u8;
    let s1 = lhs.wrapping_sub(rhs);
    let result = s1.wrapping_sub(cf);
    let total = u16::from(rhs) + u16::from(cf);
    f.cf = u16::from(lhs) < total;
    f.af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    f.of = (((lhs ^ rhs) & (lhs ^ result)) & 0x80) != 0;
    f.set_szp_8(result);
    (result, true)
}

/// /reg subop dispatch for 0x80 (r/m8, imm8). Returns (result,
/// write_back). 0=ADD 1=OR 2=ADC 3=SBB 4=AND 5=SUB 6=XOR 7=CMP.
fn group1_op_8(reg: u8, lhs: u8, rhs: u8, f: &mut Flags) -> (u8, bool) {
    match reg {
        0 => alu_add_8(lhs, rhs, f),
        1 => alu_or_8(lhs, rhs, f),
        2 => alu_adc_8(lhs, rhs, f),
        3 => alu_sbb_8(lhs, rhs, f),
        4 => alu_and_8(lhs, rhs, f),
        5 => alu_sub_8(lhs, rhs, f),
        6 => alu_xor_8(lhs, rhs, f),
        7 => alu_cmp_8(lhs, rhs, f),
        _ => unreachable!(),
    }
}

fn group1_op_32(reg: u8, lhs: u32, rhs: u32, f: &mut Flags) -> (u32, bool) {
    match reg {
        0 => alu_add_32(lhs, rhs, f),
        1 => alu_or_32(lhs, rhs, f),
        2 => alu_adc_32(lhs, rhs, f),
        3 => alu_sbb_32(lhs, rhs, f),
        4 => alu_and_32(lhs, rhs, f),
        5 => alu_sub_32(lhs, rhs, f),
        6 => alu_xor_32(lhs, rhs, f),
        7 => alu_cmp_32(lhs, rhs, f),
        _ => unreachable!(),
    }
}

fn set_flags_inc_dec_32(f: &mut Flags, lhs: u32, rhs: u32, result: u32, sub: bool) {
    f.af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    f.of = if sub {
        (((lhs ^ rhs) & (lhs ^ result)) & 0x8000_0000) != 0
    } else {
        (((lhs ^ result) & (rhs ^ result)) & 0x8000_0000) != 0
    };
    f.set_szp_32(result);
}

/// Evaluate an Intel `Jcc` condition (4-bit selector). Reference:
/// Intel SDM Vol. 2A §B.1 (EFLAGS Condition Codes for Jcc).
fn condition_holds(cc: u8, f: &Flags) -> bool {
    match cc & 0x0F {
        0x0 => f.of,                    // JO
        0x1 => !f.of,                   // JNO
        0x2 => f.cf,                    // JB / JC / JNAE
        0x3 => !f.cf,                   // JAE / JNB / JNC
        0x4 => f.zf,                    // JE / JZ
        0x5 => !f.zf,                   // JNE / JNZ
        0x6 => f.cf || f.zf,            // JBE / JNA
        0x7 => !f.cf && !f.zf,          // JA / JNBE
        0x8 => f.sf,                    // JS
        0x9 => !f.sf,                   // JNS
        0xA => f.pf,                    // JP / JPE
        0xB => !f.pf,                   // JNP / JPO
        0xC => f.sf != f.of,            // JL / JNGE
        0xD => f.sf == f.of,            // JGE / JNL
        0xE => f.zf || (f.sf != f.of),  // JLE / JNG
        0xF => !f.zf && (f.sf == f.of), // JG / JNLE
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::mmu::Perm;
    use super::*;

    fn make() -> (Cpu, Mmu) {
        let mut mmu = Mmu::new();
        mmu.map(0x1000, 0x2000, Perm::R | Perm::X); // code
        mmu.map(0x4000, 0x4000, Perm::R | Perm::W); // stack/data
        let mut cpu = Cpu::new();
        cpu.regs.eip = 0x1000;
        // Stack near top of stack page, growing down.
        cpu.regs.set_esp(0x7FF0);
        // Push the sentinel return address so the first ret stops.
        cpu.push32(&mut mmu, RET_SENTINEL).unwrap();
        (cpu, mmu)
    }

    fn write_code(mmu: &mut Mmu, addr: u32, bytes: &[u8]) {
        mmu.write_initializer(addr, bytes).unwrap();
    }

    #[test]
    fn xor_mov_add_ret_returns_0x43() {
        let (mut cpu, mut mmu) = make();
        // xor eax, eax       -> 0x33 0xC0
        // mov eax, 0x42      -> 0xB8 0x42 0x00 0x00 0x00
        // add eax, 1         -> 0x83 0xC0 0x01
        // ret                -> 0xC3
        write_code(
            &mut mmu,
            0x1000,
            &[
                0x33, 0xC0, 0xB8, 0x42, 0x00, 0x00, 0x00, 0x83, 0xC0, 0x01, 0xC3,
            ],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0x43);
        assert_eq!(cpu.regs.eip, RET_SENTINEL);
    }

    #[test]
    fn push_pop_roundtrip_through_stack() {
        let (mut cpu, mut mmu) = make();
        // mov eax, 0xCAFEBABE -> B8 BE BA FE CA
        // push eax            -> 50
        // pop ebx             -> 5B
        // ret                 -> C3
        write_code(
            &mut mmu,
            0x1000,
            &[0xB8, 0xBE, 0xBA, 0xFE, 0xCA, 0x50, 0x5B, 0xC3],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Ebx), 0xCAFE_BABE);
    }

    #[test]
    fn call_relative_then_ret_unwinds_correctly() {
        let (mut cpu, mut mmu) = make();
        // 0x1000: call rel32 +5 ; 0xE8 05 00 00 00 (target = 0x100A)
        // 0x1005: mov eax, 1   ; 0xB8 01 00 00 00
        // 0x100A: mov eax, 0x99 ; 0xB8 99 00 00 00
        //         ret           ; 0xC3
        write_code(
            &mut mmu,
            0x1000,
            &[
                0xE8, 0x05, 0x00, 0x00, 0x00, 0xB8, 0x01, 0x00, 0x00, 0x00, 0xB8, 0x99, 0x00, 0x00,
                0x00, 0xC3,
            ],
        );
        cpu.run(&mut mmu).unwrap();
        // The RET inside the callee returns to 0x1005, which sets
        // eax=1 and falls through to the bytes at 0x100A again
        // (re-setting eax=0x99) and rets to sentinel.
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0x99);
    }

    #[test]
    fn cmp_je_taken() {
        let (mut cpu, mut mmu) = make();
        // mov eax, 5   -> B8 05 00 00 00
        // cmp eax, 5   -> 83 F8 05
        // je +2        -> 74 02
        // mov eax, 0   -> B8 00 00 00 00 (skipped by JE)
        // ... but je +2 skips 2 bytes after the jump → lands on
        //     mov ebx, 7
        // Layout:
        //   0x1000: B8 05 00 00 00   (mov eax, 5)
        //   0x1005: 83 F8 05         (cmp eax, 5)
        //   0x1008: 74 03            (je +3)
        //   0x100A: B8 00 00 00 00   (mov eax, 0)  - skipped
        //   0x100F: BB 07 00 00 00   (mov ebx, 7)  - landing
        //   0x1014: C3                (ret)
        write_code(
            &mut mmu,
            0x1000,
            &[
                0xB8, 0x05, 0x00, 0x00, 0x00, 0x83, 0xF8, 0x05, 0x74, 0x05, 0xB8, 0x00, 0x00, 0x00,
                0x00, 0xBB, 0x07, 0x00, 0x00, 0x00, 0xC3,
            ],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 5);
        assert_eq!(cpu.regs.get32(Reg32::Ebx), 7);
    }

    #[test]
    fn lea_resolves_displacement_arithmetic() {
        let (mut cpu, mut mmu) = make();
        // mov ebx, 0x100 -> BB 00 01 00 00
        // lea eax, [ebx + 0x40] -> 8D 43 40
        // ret
        write_code(
            &mut mmu,
            0x1000,
            &[0xBB, 0x00, 0x01, 0x00, 0x00, 0x8D, 0x43, 0x40, 0xC3],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0x140);
    }

    #[test]
    fn imul_eax_sets_eax_only_low_32() {
        let (mut cpu, mut mmu) = make();
        // mov eax, 7 ; mov edx, 3 ; imul eax, edx ; ret
        // The 0x0F 0xAF form is two-operand IMUL; result low 32 in dst.
        write_code(
            &mut mmu,
            0x1000,
            &[
                0xB8, 0x07, 0x00, 0x00, 0x00, 0xBA, 0x03, 0x00, 0x00, 0x00, 0x0F, 0xAF, 0xC2, 0xC3,
            ],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 21);
    }

    #[test]
    fn cpuid_leaf_0_returns_genuineintel() {
        let (mut cpu, mut mmu) = make();
        // mov eax, 0 ; cpuid ; ret
        write_code(
            &mut mmu,
            0x1000,
            &[0xB8, 0x00, 0x00, 0x00, 0x00, 0x0F, 0xA2, 0xC3],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 1);
        let bytes = [
            cpu.regs.get32(Reg32::Ebx).to_le_bytes(),
            cpu.regs.get32(Reg32::Edx).to_le_bytes(),
            cpu.regs.get32(Reg32::Ecx).to_le_bytes(),
        ];
        let mut joined = Vec::new();
        for arr in bytes.iter() {
            joined.extend_from_slice(arr);
        }
        assert_eq!(&joined, b"GenuineIntel");
    }

    #[test]
    fn hlt_traps_as_privileged() {
        let (mut cpu, mut mmu) = make();
        write_code(&mut mmu, 0x1000, &[0xF4]); // hlt
        match cpu.run(&mut mmu) {
            Err(Trap::PrivilegedOpcode {
                mnemonic: "hlt", ..
            }) => (),
            other => panic!("expected hlt trap, got {other:?}"),
        }
    }

    #[test]
    fn unknown_opcode_traps() {
        let (mut cpu, mut mmu) = make();
        write_code(&mut mmu, 0x1000, &[0xD6]); // SALC: unimplemented in our table
        match cpu.run(&mut mmu) {
            Err(Trap::UndefinedOpcode { opcode: 0xD6, .. }) => (),
            other => panic!("expected undefined-opcode trap for 0xD6, got {other:?}"),
        }
    }

    #[test]
    fn ret_with_imm_pops_n_bytes() {
        let (mut cpu, mut mmu) = make();
        // mov eax, 0x77 ; push 0xAAAA ; ret 4
        // The ret 4 should skip the 0xAAAA argument off the stack.
        write_code(
            &mut mmu,
            0x1000,
            &[
                0xB8, 0x77, 0x00, 0x00, 0x00, 0x68, 0xAA, 0xAA, 0x00, 0x00, 0xC2, 0x04, 0x00,
            ],
        );
        // The retn 4 will pop 0xAAAA as return address, which is
        // not the sentinel, so the run loop will continue from that
        // bad eip. We expect the run to error with a memory fault.
        match cpu.run(&mut mmu) {
            Err(Trap::MemoryFault { .. }) | Err(Trap::ExecuteProtectFault { .. }) => (),
            other => panic!("expected fault after misdirected ret 4, got {other:?}"),
        }
    }

    #[test]
    fn shl_shifts_and_sets_flags() {
        let (mut cpu, mut mmu) = make();
        // mov eax, 1 ; shl eax, 4 ; ret  → 0x10
        write_code(
            &mut mmu,
            0x1000,
            &[0xB8, 0x01, 0x00, 0x00, 0x00, 0xC1, 0xE0, 0x04, 0xC3],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0x10);
    }

    #[test]
    fn condition_holds_cross_check() {
        let mut f = Flags {
            zf: true,
            ..Flags::default()
        };
        assert!(condition_holds(0x4, &f)); // JE
        assert!(!condition_holds(0x5, &f)); // JNE
        f.zf = false;
        f.cf = true;
        assert!(condition_holds(0x2, &f)); // JB
        assert!(!condition_holds(0x3, &f)); // JAE
    }

    #[test]
    fn instr_limit_enforced() {
        let (mut cpu, mut mmu) = make();
        // jmp -2 forever → "EB FE"
        write_code(&mut mmu, 0x1000, &[0xEB, 0xFE]);
        cpu.set_instr_limit(100);
        match cpu.run(&mut mmu) {
            Err(Trap::InstructionLimitExceeded { .. }) => (),
            other => panic!("expected instr-limit trap, got {other:?}"),
        }
    }

    // ----- round 5 opcode regression tests --------------------------

    #[test]
    fn add_al_imm8_wraps_and_sets_carry() {
        let (mut cpu, mut mmu) = make();
        // mov al, 0xFE   B0 FE
        // add al, 5      04 05  ; 0xFE + 5 = 0x103 → AL=0x03 CF=1
        // ret            C3
        write_code(&mut mmu, 0x1000, &[0xB0, 0xFE, 0x04, 0x05, 0xC3]);
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get8(Reg8::Al), 0x03);
        assert!(cpu.regs.flags.cf, "AL+imm8 carry not set");
    }

    #[test]
    fn cmp_al_imm8_sets_zf_when_equal() {
        let (mut cpu, mut mmu) = make();
        // mov al, 0x42 ; cmp al, 0x42 ; ret
        write_code(&mut mmu, 0x1000, &[0xB0, 0x42, 0x3C, 0x42, 0xC3]);
        cpu.run(&mut mmu).unwrap();
        assert!(cpu.regs.flags.zf);
        assert!(!cpu.regs.flags.cf);
    }

    #[test]
    fn imul_r32_imm8_signed() {
        let (mut cpu, mut mmu) = make();
        // mov ebx, 0xFFFFFFFF (-1) ; imul eax, ebx, 7 ; ret  → eax = -7 = 0xFFFFFFF9
        // BB FF FF FF FF       6B C3 07     C3
        write_code(
            &mut mmu,
            0x1000,
            &[0xBB, 0xFF, 0xFF, 0xFF, 0xFF, 0x6B, 0xC3, 0x07, 0xC3],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0xFFFF_FFF9);
    }

    #[test]
    fn rep_movs_d_copies_dword_per_step_until_ecx_zero() {
        let (mut cpu, mut mmu) = make();
        // Lay out two 8-byte buffers in the data page.
        mmu.write_initializer(0x4100, &[1, 2, 3, 4, 5, 6, 7, 8])
            .unwrap();
        // mov esi, 0x4100   BE 00 41 00 00
        // mov edi, 0x4200   BF 00 42 00 00
        // mov ecx, 2        B9 02 00 00 00
        // rep movsd         F3 A5
        // ret               C3
        write_code(
            &mut mmu,
            0x1000,
            &[
                0xBE, 0x00, 0x41, 0x00, 0x00, 0xBF, 0x00, 0x42, 0x00, 0x00, 0xB9, 0x02, 0x00, 0x00,
                0x00, 0xF3, 0xA5, 0xC3,
            ],
        );
        cpu.run(&mut mmu).unwrap();
        for i in 0..8 {
            assert_eq!(mmu.load8(0x4200 + i).unwrap(), (i + 1) as u8);
        }
        assert_eq!(cpu.regs.get32(Reg32::Ecx), 0);
    }

    #[test]
    fn fs_segment_translates_address() {
        let (mut cpu, mut mmu) = make();
        // Map a "TEB" page at 0x5000 and seed FS:[0]=0xCAFE.
        mmu.map(0x5000, 0x1000, Perm::R | Perm::W);
        mmu.write_initializer(0x5000, &0xCAFEu32.to_le_bytes())
            .unwrap();
        cpu.set_fs_base(0x5000);
        // mov eax, fs:[0]  64 A1 00 00 00 00
        // ret              C3
        write_code(
            &mut mmu,
            0x1000,
            &[0x64, 0xA1, 0x00, 0x00, 0x00, 0x00, 0xC3],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0xCAFE);
    }

    #[test]
    fn cmovz_copies_when_zf_set() {
        let (mut cpu, mut mmu) = make();
        // xor eax, eax    33 C0           ; eax=0, ZF=1
        // mov ebx, 0x77   BB 77 00 00 00
        // cmovz eax, ebx  0F 44 C3        ; ZF=1 → eax=ebx=0x77
        // ret             C3
        write_code(
            &mut mmu,
            0x1000,
            &[
                0x33, 0xC0, 0xBB, 0x77, 0x00, 0x00, 0x00, 0x0F, 0x44, 0xC3, 0xC3,
            ],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0x77);
    }

    #[test]
    fn bswap_swaps_byte_order() {
        let (mut cpu, mut mmu) = make();
        // mov eax, 0x11223344  ; bswap eax  ; ret  → 0x44332211
        write_code(
            &mut mmu,
            0x1000,
            &[0xB8, 0x44, 0x33, 0x22, 0x11, 0x0F, 0xC8, 0xC3],
        );
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0x4433_2211);
    }

    #[test]
    fn group2_rm8_shl_sets_flags() {
        let (mut cpu, mut mmu) = make();
        // mov al, 0x40  B0 40
        // shl al, 2     C0 E0 02   → al = 0x100 truncated = 0x00, CF = bit 6 (1)
        // ret           C3
        write_code(&mut mmu, 0x1000, &[0xB0, 0x40, 0xC0, 0xE0, 0x02, 0xC3]);
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get8(Reg8::Al), 0x00);
        assert!(cpu.regs.flags.cf);
    }

    #[test]
    fn pushad_popad_round_trip() {
        let (mut cpu, mut mmu) = make();
        cpu.regs.set32(Reg32::Eax, 0xAAA);
        cpu.regs.set32(Reg32::Ecx, 0xCCC);
        cpu.regs.set32(Reg32::Edx, 0xDDD);
        cpu.regs.set32(Reg32::Ebx, 0xBBB);
        cpu.regs.set32(Reg32::Ebp, 0x1BB);
        cpu.regs.set32(Reg32::Esi, 0x1EE);
        cpu.regs.set32(Reg32::Edi, 0x1DD);
        // pushad ; popad ; ret  →  60 61 C3
        write_code(&mut mmu, 0x1000, &[0x60, 0x61, 0xC3]);
        cpu.run(&mut mmu).unwrap();
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0xAAA);
        assert_eq!(cpu.regs.get32(Reg32::Ecx), 0xCCC);
        assert_eq!(cpu.regs.get32(Reg32::Edx), 0xDDD);
        assert_eq!(cpu.regs.get32(Reg32::Ebx), 0xBBB);
        assert_eq!(cpu.regs.get32(Reg32::Ebp), 0x1BB);
        assert_eq!(cpu.regs.get32(Reg32::Esi), 0x1EE);
        assert_eq!(cpu.regs.get32(Reg32::Edi), 0x1DD);
    }
}
