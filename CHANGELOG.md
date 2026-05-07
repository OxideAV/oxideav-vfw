# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned

- **Trace mode** (`trace` Cargo feature, off by default) —
  reverse-engineering aid documented in
  `OxideAV/docs/winmf/winmf-emulator.md` (§Trace mode + §Future
  extensions). Reframes the crate as having two co-equal
  end-uses: rare-codec compatibility (today) and clean-room
  reverse-engineering aid (post-round-5). The feature emits
  JSONL events for Win32 stub calls, memory watchpoints
  (`Sandbox::watch(addr, size, mode)`), and (with the
  `trace-exec` sub-feature) per-instruction execution. Sink
  configurable via `OXIDEAV_VFW_TRACE_FILE` env var or
  `Sandbox::set_trace_sink()` programmatic API. Intentionally
  not implemented yet — documented now so that round-5+ ISA
  growth and stub additions design the probe hooks in rather
  than retrofit them.

### Removed

- The `test-fixtures` Cargo feature is gone. The fixture-discovery
  helper handles every code path (env override, Wine prefix,
  Windows sys dirs, local cache, HTTPS fetch) on its own; the
  feature gate it used to provide is no longer needed. CI runs
  the staged-DLL tests every build.

### Added

- Round 7: **Real IV31 keyframe decode through `cubes.mov`**, plus
  MMX scaffolding for round 8. Twin deliverables:
  - **Part A — `cubes.mov` decode.** `tests/common/mod.rs` gains
    `fetch_or_load_ffmpeg_sample(fourcc, name)` for the
    `samples.oxideav.org/ffmpeg/V-codecs/<FOURCC>/<NAME>` corpus
    (HTTPS + cache + env-override tiers). New
    `tests/common/mov_extractor.rs` — a ~270 LOC test-side
    QuickTime / ISO BMFF chunk walker (authored from
    ISO/IEC 14496-12, §4 + §8) that parses
    `moov → trak → mdia → minf → stbl → {stsd, stco, stsz}` to
    locate sample 0's bytes from `cubes.mov` (160×120 yuv410p,
    Indeo 3, 40 frames, 121 KB). New `tests/round7_cubes_mov.rs`
    drives the full IC* sequence against the real keyframe;
    `ICDecompress` returns `ICERR_OK` and writes ~30 K non-zero
    RGB24 bytes (~52% of the 57.6 KB output) — the first real
    pixel decode through `IR32_32.DLL`.
  - **Bug fix**: `ICM_DECOMPRESS_BEGIN` was wrong since round 5,
    pointing at `ICM_USER + 16 = 0x4010` (an unmapped slot), so
    the codec's per-instance state initialiser never ran and
    `ICDecompress` always bailed at the `[state2_ptr] != 0`
    check (`mov eax, 0xffffff9c` at `eip=0x10002b5d`). Round 7
    fixes `ICM_DECOMPRESS_BEGIN = ICM_USER + 12 = 0x400C` — the
    canonical vfw.h value — disassembled from
    `IR32_32.DLL`'s dispatch table at `0x10001760`. While here,
    `ICM_DECOMPRESS_GET_FORMAT` corrected from
    `0x4008` → `0x400A`.
  - **Part B — MMX scaffolding for round 8.**
    - `Cpu::mmx: [u64; 8]` register file (mm0..mm7), per Intel
      SDM Vol. 1 §9.2.1. Aliases to FPU stack ST(0..7) on real
      hardware; we model them as a separate array.
    - New `Trap::UnimplementedMmx { eip, opcode, mnemonic_hint }`
      variant. Round-8 work-list reads the trap log.
    - `emulator::isa_int::dispatch_mmx` routes the MMX opcode
      space (`0F 60..6F`, `0F 70..7F`, `0F D0..FF`, per Intel
      SDM Vol. 2 Appendix A Table A-3) to the structured trap.
      ModR/M + (PSHUFW / group-12/13/14) imm8 are consumed so
      EIP advances past the full instruction.
    - SDM-derived mnemonic hints (`PXOR MMX`, `PADDB MMX`,
      `PSLLQ imm8 (group-14)`, `EMMS`, …) — round 8 lands them
      one at a time.
    - 14 new tests in `tests/round7_mmx_scaffold.rs`: register
      file zero-init / writability, every opcode-space block
      traps as `UnimplementedMmx` with the correct opcode +
      mnemonic, EIP advances correctly past ModR/M and imm8,
      `0F C8 BSWAP eax` (a non-MMX `0F` opcode) still works.
- Round 6: "Drive the full IC* decode pipeline end-to-end against
  Intel IR32_32.DLL" milestone landed. The
  `ICDecompressQuery → ICDecompressBegin → ICDecompress →
  ICDecompressEnd` sequence now walks against a synthetic
  Indeo 3 (IV31) keyframe at 64×48 without tripping a single
  ISA opcode or Win32 stub gap beyond round 5. No new opcodes,
  no new stubs; round 5's coverage was sufficient for the full
  decode-call cycle to enter and exit cleanly.
  - **SPECGAP**: the `IV5PLAY` redistributable bundle in
    `samples.oxideav.org/codecs/windows/IV5PLAY/` ships only the
    codec DLLs, no `.avi` payloads. Round 6 builds a synthetic
    IV31 keyframe whose 16-byte frame header + 48-byte bitstream
    header layout matches the public Indeo 3 spec mirrored in
    `docs/video/indeo/indeo3/wiki/Indeo_3.wiki` (multimedia.cx,
    CC-BY-SA), with `data_size = 128` (bits) which the wiki
    documents as a NULL/sync frame. The codec accepts the input
    and output formats (`ICDecompressQuery` → `ICERR_OK`),
    sets up its internal state (`ICDecompressBegin` → `ICERR_OK`),
    rejects the synthetic NULL-data-size frame at the bitstream-
    header validation step (`ICDecompress` → `ICERR_BADIMAGE` =
    `-100` = `0xFFFFFF9C`), and tears down cleanly
    (`ICDecompressEnd` → `ICERR_OK`).
  - The contract of `tests/m2_indeo3_driverproc.rs::indeo3_decompress_one_keyframe`
    is therefore: the IC* sequence runs without trapping; the
    output buffer is intact at the requested capacity; the
    `ICDecompress` LRESULT is non-positive (any positive value
    would be a fault sentinel, not a documented vfw error code).
    Round 7+ swaps the synthetic input for a real keyframe
    extracted from a bundled `.avi` once one is available, at
    which point the test would also assert non-zero output.
  - No emulator changes — the `0x69 0x6B IMUL`, `0x86 0x87 XCHG`,
    REP-prefixed string ops, segment-override prefixes, and
    `0F xx` extension opcodes round 5 added are sufficient for
    Indeo 3's `ICDecompress*` body. MMX is still deferred to
    round 7+ when the test corpus expands to MMX-using codecs
    (Indeo 5, Cinepak, etc.).

- Round 5: "DllMain + ICOpen + ICGetInfo + ICClose against
  Intel IR32_32.DLL" milestone landed.
  - `emulator::isa_int` learnt all 8-bit primary ALU opcodes
    (`0x00..=0x05` ADD, `0x08..=0x0D` OR, `0x10..=0x15` ADC,
    `0x18..=0x1D` SBB, `0x20..=0x25` AND, `0x28..=0x2D` SUB,
    `0x30..=0x35` XOR, `0x38..=0x3D` CMP) — group-1 (`0x80`)
    + group-3 r/m8 (`0xF6`) + group-4 INC/DEC r/m8 (`0xFE`).
  - `emulator::isa_int` learnt group-2 (rotate/shift) `r/m8`
    forms (`0xC0` imm8, `0xD0` 1, `0xD2` cl) and `r/m32` 1/cl
    counts (`0xD1` / `0xD3`).
  - `emulator::isa_int` learnt `IMUL r32, r/m32, imm32`/`imm8`
    (`0x69` / `0x6B`), `XCHG r/m, r` (`0x86` / `0x87`), `SAHF`
    / `LAHF` (`0x9E` / `0x9F`), `CMC` (`0xF5`), `PUSHAD` /
    `POPAD` (`0x60` / `0x61`), `ENTER` (`0xC8`), and the full
    string-instruction family with REP semantics: `MOVSB` /
    `MOVSD` / `STOSB` / `STOSD` / `LODSB` / `LODSD` / `CMPSB` /
    `CMPSD` / `SCASB` / `SCASD` (`0xA4..=0xA7`, `0xAA..=0xAF`).
  - `emulator::isa_int::dispatch_0f` learnt `CMOVcc r32, r/m32`
    (`0F 40..4F`), `BT r/m32, r32` (`0F A3`), `BTS r/m32, r32`
    (`0F AB`), `SHLD` (`0F A4` imm8 / `0F A5` cl), `SHRD`
    (`0F AC` / `0F AD`), group-8 BT/BTS/BTR/BTC imm8 (`0F BA`),
    `CMPXCHG` (`0F B1`), `XADD` (`0F C1`), and `BSWAP r32`
    (`0F C8..CF`).
  - `Cpu` now models segment-override prefixes properly. The
    prefix-loop sets `seg_override`, and effective-address
    resolution applies a per-segment linear base
    (`set_fs_base` / `set_gs_base`). The `Sandbox` runtime maps
    a 4 KiB TEB at `0x7FFD_E000`, primes `FS:[0]` (SEH chain
    end-of-list `-1`) + `FS:[0x18]` (TEB self-pointer), and
    points FS there. This lets the codec's `_try` / `__except`
    init read `mov eax, fs:[0]` without a memory fault.
  - `win32::vfw32` corrected the `ICM_*` numeric values
    (round-4 used `ICM_USER + N` for several N; the canonical
    SDK header has `ICM_GETINFO = ICM_RESERVED + 2 = 0x5002`,
    `ICM_DECOMPRESS_QUERY = ICM_USER + 11 = 0x400B`,
    `ICM_DECOMPRESS = ICM_USER + 13 = 0x400D`,
    `ICM_DECOMPRESS_END = ICM_USER + 14 = 0x400E`,
    `ICM_DECOMPRESS_BEGIN = ICM_USER + 16 = 0x4010`).
    `vfw32::ICM_RESERVED = 0x5000` is now the documented base.
  - `win32::vfw32::ic_open` now stages a real 36-byte `ICOPEN`
    structure (`dwSize / fccType / fccHandler / dwVersion /
    dwFlags / dwError / pV1Reserved / pV2Reserved / dnDevNode`)
    in the sandbox arena and passes it as `lParam2`. Round-4
    passed NULL, which prompted Indeo 3 to return the magic
    sentinel `0xFFFF_0000` (not a real per-instance pointer).
  - `win32::vfw32::ic_get_info` falls back to a fcc-derived
    ASCII rendering when the codec leaves `szName` NUL —
    real `vfw32!ICGetInfo` populates `szName` /
    `szDescription` / `szDriver` from registry data BEFORE
    posting `ICM_GETINFO`, but the sandbox has no registry.
    Indeo 3 doesn't write `szName` itself.
  - One bug-fix in the round-4 `0xC6` (MOV r/m8, imm8)
    handler: the immediate fetch ran BEFORE the operand
    resolution, so any displacement was misread as the
    immediate. Round-5 swaps the order, matching the SDM.
  - `tests/m2_indeo3_driverproc.rs::indeo3_driverproc_open_getinfo_close_smoke`
    now asserts the round-5 outcome: `DllMain` returns
    cleanly, `ICOpen('VIDC','IV31',2)` mints a HIC,
    `ICGetInfo` returns a 568-byte `ICINFO` whose
    `dwSize / fccType` reflect the codec's reply, `szName`
    decodes to a non-empty ASCII-printable string, and
    `ICClose` returns without trapping. (The round-4
    expected-trap branch is gone.)
  - 9 new lib unit tests covering the round-5 ISA additions
    (ALU 8-bit, IMUL imm, REP MOVS, FS segment, CMOV, BSWAP,
    group-2 r/m8, PUSHAD/POPAD).

- Round 4: "Close the 49 round-3 import gaps" milestone landed.
  The 49 stubs round-3 surfaced from Intel's `IR32_32.DLL`
  (Indeo 3) are all implemented; `Sandbox::load(IR32_32.DLL)`
  succeeds end-to-end. 32 lib stub-level tests + 1 integration
  test round 4 added; full crate now ships 85 lib + 5
  integration tests, all green.
  - `win32::gdi32` (8 stubs) — `BitBlt` (no-op TRUE),
    `CreateCompatibleDC` (sentinel HDC `0xDEADC011`), `DeleteDC`
    (live-set validating), `GetDeviceCaps` (32 BPP / 1 plane /
    sensible RASTERCAPS / `LOGPIXELS{X,Y}=96`), `GetNearestColor`
    (identity), `GetObjectA` (0), `GetSystemPaletteEntries` (0),
    `SelectObject` (identity).
  - `win32::kernel32` round-4 additions (24 stubs) —
    `ExitProcess` (sets `host.exit_requested`, run-loop
    converts to clean RET_SENTINEL), `GetACP` (1252), `GetOEMCP`
    (437), `GetCPInfo` (`MaxCharSize=1`, default `'?'`),
    `GetCommandLineA` (canned `"oxideav-vfw\0"`),
    `GetEnvironmentStrings` (`"\0\0"`), `GetFileType`
    (`FILE_TYPE_UNKNOWN=0`), `GetLastError` / `SetLastError`
    (per-Sandbox `last_error: u32` slot), `GetModuleFileNameA`
    / `GetModuleHandleA` (NULL → primary loaded DLL base),
    `GetStartupInfoA` (`cb=68`, rest zero), `GetStdHandle`
    (`INVALID_HANDLE_VALUE`), `GetSystemInfo`
    (single-Pentium / 4 KiB pages), `GetVersion` (`0x0A04` =
    Win98), `GlobalAlloc` / `GlobalFree` / `GlobalLock` /
    `GlobalUnlock` (Local* alias), `MultiByteToWideChar` /
    `WideCharToMultiByte` (zero-extend / low-byte-or-default
    conversion, honours `cb=-1`), `RtlUnwind` (no-op SEH stub),
    `VirtualAlloc` / `VirtualFree` (uses MMU `find_free_range`
    + `unmap`; reserved region
    `0xA000_0000..0xC000_0000`), `WriteFile` (FALSE +
    `ERROR_INVALID_HANDLE`).
  - `win32::user32` (16 stubs) — fail-soft for the dialog /
    paint / window surface; `MessageBoxA` logs to stderr +
    `host.message_box_log`; `wsprintfA` is a real cdecl
    variadic implementation (`%d` / `%u` / `%x` / `%X` / `%s` /
    `%c` / `%%`, no width / precision / `%f`).
  - `win32::winmm` (1 stub) — `DefDriverProc` returning 0 for
    every `DRV_*` message except `DRV_CONFIGURE` (`DRVCNF_OK=1`).
  - `MMU::unmap(addr, size)` and `MMU::find_free_range(lo, hi,
    size)` plumbing for `VirtualAlloc` / `VirtualFree`.
  - `HostState` gained `last_error`, `primary_module_base`,
    `message_box_log`, `exit_requested`, const-arena cursor +
    `arena_const_alloc()` for canned strings, `gdi_hdcs` live
    set.
  - `Registry` gained `register_gdi32` / `register_user32` /
    `register_winmm` / `register_all`. `Sandbox::new` now wires
    the full set + maps a const-arena region at
    `[0x7000_0000, 0x7010_0000)` for canned strings.
  - `tests/m1_load_dll_main.rs::staged_codec_dll_resolves_every_import`
    asserts the stub registry covers **every** import
    `IR32_32.DLL` declares (zero-miss assertion). Renamed from
    `staged_codec_dll_lists_round_four_todo_imports`.
  - `tests/m2_indeo3_driverproc.rs::indeo3_driverproc_open_getinfo_close_smoke`
    flipped from "load is rejected" to "load succeeds, DllMain
    walks until the first un-decoded ISA opcode": round-4
    outcome is `Trap::UndefinedOpcode { opcode: 0x04, eip:
    0x1000_612A }` — that's `ADD AL, imm8`, the round-5 todo
    list. The test asserts on the exact (opcode, eip) pair so
    any drift is loud + names round 5's first hand-off.
  - 32 new lib unit tests covering the new stub families
    (5 gdi32 + 4 user32 + 2 winmm + 21 kernel32 round-4).

- Round 3: "Real-codec smoke test against Intel IR32_32.DLL"
  milestone landed.
  - `tests/common/mod.rs` — fixture-discovery helper:
    `fetch_or_load(name)` resolves codec DLL bytes via env-var
    override, Wine prefix (`~/.wine/drive_c/windows/{system32,
    syswow64}/`), Windows system32 / SysWOW64, on-disk cache
    (`$CARGO_TARGET_DIR/test-fixture-cache/`), and finally
    HTTPS fetch from `samples.oxideav.org`. CI=true bypasses
    the cache so air-gapped staleness can never mask a regression.
  - `tests/common/list_pe_imports` — PE32-imports parser used
    to enumerate the round-4 stub-registry todo list before
    the loader's fail-fast import resolution short-circuits.
  - `tests/m1_load_dll_main.rs::staged_codec_dll_lists_round_four_todo_imports`
    — fetches Intel's `IR32_32.DLL` (Indeo 3) and asserts the
    exact set of 49 Win32 imports the round-1 + round-2 stub
    set does not satisfy: 8 gdi32, 24 kernel32, 16 user32, 1
    winmm. That set is round 4's deliverable.
  - `tests/m2_indeo3_driverproc.rs` (renamed from
    `m2_cinepak_decode.rs`) — synthetic-codec walkthrough
    coverage retained; plus a forward-compatible Indeo 3
    walkthrough that runs `DllMain → ICOpen('VIDC','IV31',
    DECOMPRESS) → ICGetInfo → ICClose` once the loader can
    satisfy the imports. End-of-round-3 path: assert the load
    is rejected with `UnknownImportFunction`. Round-4 path:
    walk the IC* sequence, read `szName` from `ICINFO`, and
    assert the codec name is non-empty + ASCII-printable.
  - `[dev-dependencies] ureq = "2"` for the HTTPS fetch.
- Round 2: "Decode one Cinepak frame" milestone landed.
  - `Sandbox::call_export(image, name, args)` — generic stdcall
    guest-call helper. Pushes args right-to-left + the synthetic
    `RET_SENTINEL`, runs until the callee returns, reports `eax`.
    `call_dll_main` is now a one-liner over `call_export`.
  - `win32::run_until_sentinel` and `win32::call_guest` are
    free functions usable from anywhere — the round-2 vfw32 host
    surface uses them re-entrantly to dispatch `DriverProc`
    inside an outer IC* call.
  - `win32::vfw32` — `Bih` (`BITMAPINFOHEADER`),
    `host_bih_to_guest`/`guest_bih_to_host` marshalling, the
    `ICDECOMPRESS` field layout, and host wrappers for
    `ICOpen` / `ICClose` / `ICGetInfo` / `ICDecompressQuery` /
    `ICDecompressBegin` / `ICDecompress` / `ICDecompressEnd`.
    Each wrapper allocates the message-specific scratch in the
    sandbox arena, populates the struct, calls `DriverProc(_, _,
    msg, lparam1, lparam2)` via `call_guest`, reads back the
    output, and reports the `LRESULT` to the caller.
  - `Sandbox::install_codec(image)` records the codec's
    `DriverProc` VA so subsequent `Sandbox::ic_*` calls dispatch
    against it. Codec-id registration ("one
    `CodecImplementation` per loaded DLL with a generic
    `vfw_<fcc>` codec_id") is deferred to round 3.
  - `HostState` gained an `hics: BTreeMap<u32, HicEntry>` table
    + `arena_alloc(n)` helper.
  - `lib.rs` now exposes a `register(ctx)` shim + the
    `oxideav_core::register!` macro call (gated on the default-on
    `registry` cargo feature). The shim is a no-op for round 2
    and exists so `oxideav-meta` can wire the crate without a
    special case.
  - 8 new vfw32 unit tests + 2 new integration tests
    (`tests/m2_cinepak_decode.rs`) covering the synthetic-codec
    `IC*` pipeline and (`test-fixtures`-gated) the staged-DLL
    Cinepak decode path.
  - MMX deferred to round 3 — Cinepak does not use it; the
    deferral is documented in `lib.rs`.

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
