# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Round 1: "Load + DllMain + clean exit" milestone landed. The
  crate now ships:
  - `emulator::mmu` — flat 4 GiB virtual address space with
    sparse 4 KiB pages, R/W/X permissions per page, and
    `load{8,16,32,64}` / `store{8,16,32,64}` helpers all
    written via `from_le_bytes` / `to_le_bytes` so the entire
    MMU is `#![forbid(unsafe_code)]`.
  - `emulator::regs`, `emulator::decode`, `emulator::isa_int` —
    register file (eax..ebp + esp + eip + EFLAGS), instruction
    decoder for ModR/M + SIB + immediates, and a `match`-based
    interpreter for the i386 integer base ISA. `cpuid` returns
    the canned Pentium-class response (vendor "GenuineIntel",
    no SSE, no AMD ext); privileged opcodes + far calls +
    segment loads trap. MMX is deferred to round 2.
  - `pe` — PE32-only loader: DOS + PE header parse, section
    mapping into the MMU, base-relocation walk, IAT resolution
    against the Win32 stub registry, export-by-name lookup.
    Rejects PE32+, .NET / managed PE, and import-by-ordinal.
  - `win32::kernel32` — minimum stub set to satisfy a
    Cinepak-class DLL: `GetProcessHeap`, `HeapAlloc` /
    `HeapFree` / `HeapReAlloc`, `LocalAlloc` / `LocalFree`,
    `OutputDebugStringA`, `GetTickCount`,
    `InterlockedIncrement` / `InterlockedDecrement`,
    `LoadLibraryA`, `GetProcAddress`. All stdcall.
  - `runtime::Sandbox` — the public end-to-end entry point.
    Owns the MMU, CPU, registry, and host state; `load(...)`
    + `call_dll_main(...)` drive a DLL through
    `DLL_PROCESS_ATTACH`.
  - 67 tests across all components: 12 MMU, 7 regs, 5 decode,
    13 ISA, 13 kernel32 stubs, 3 registry, 6 PE loader, 2
    runtime, 1 PE reloc, 1 sections, 1 PE header, 2
    integration. The integration test at
    `tests/m1_load_dll_main.rs` builds a minimal valid PE32
    DLL byte-by-byte (no fixtures committed) and runs its
    `DllMain` end-to-end through every round-1 layer. A
    `test-fixtures`-gated companion test loads a real legacy
    codec DLL from `tests/fixtures/iccvid.dll` if the user has
    staged one (silently skipped otherwise so CI does not
    block on fixture presence).
  - `#![forbid(unsafe_code)]` is enforced at the crate root.

- Round 0 scaffold (already present in the previous tag) —
  see entry below.

### Notes on scope

- The crate's purpose is **rare-codec compatibility**, not
  day-to-day playback. Modern codecs (H.264, HEVC, AV1, VP9, Opus,
  AAC, …) all have pure-Rust decoders elsewhere in the workspace.
  This crate exists for codecs the project would otherwise
  permanently shelve: Indeo 4/5, Sorenson Video 1/3, MS-MPEG-4 v3,
  Cook, On2 VP3-pre-Theora, MS speech codecs, etc.
- 32-bit x86 only. Every target codec ships a 32-bit version; many
  never had a 64-bit port.
- Safety > performance. Pure interpreter, no JIT, the entire crate
  aimed at `#![forbid(unsafe_code)]`. Codec runs through a
  bounded-MMU sandbox; never executes on the host CPU.
