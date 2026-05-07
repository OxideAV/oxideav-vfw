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
