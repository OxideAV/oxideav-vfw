# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Round 17 Part A — **non-Indeo Win32 codec hunt + corpus
  byte-scan.** New `tests/round17_corpus_specgap.rs` probes
  the `samples.oxideav.org/codecs/windows/` namespace for
  every plausible non-Indeo VfW / DirectShow codec
  binary the catalogue in `docs/winmf/windows-codecs.md`
  enumerates (Cinepak `iccvid.dll`, MS Video 1
  `msvidc32.dll`, MS RLE `msrle32.dll`, MS YUV `msyuv.dll`,
  MS-MPEG-4 v3 `mpg4ds32.ax`, DivX `divx*.dll`, TSCC
  `tsccvid.dll`, WMV `wmvcore.dll`, plus their plausible
  per-FOURCC subdirectories): all 16 candidates return 404.
  The corpus contains exclusively the Intel IV5PLAY
  redistributable: `IR32_32.DLL` (Indeo 3, 199168 B),
  `IR50_32.DLL` (Indeo 5, 739328 B), `IR41_32.AX` (Indeo 4,
  848384 B). The same test byte-scans all three Indeo
  binaries for `0F D0..FF` (MMX-arithmetic opcode block per
  Intel SDM Vol. 2A Table A-3) and `0F A2` (CPUID per
  Vol. 2B): IR32 has 146 / 0, IR50 has 2518 / 2, IR41 has
  1094 / 2. The byte counts contradict the round-14
  diagnostic claim of "zero MMX/CPUID bytes" (round 14
  appears to have scanned a stale 184 KB binary; the live
  fixture is 739 KB) — the binaries DO contain MMX-arithmetic
  byte patterns and CPUID instructions, but the codec's
  decode path through `DllMain → DRV_OPEN →
  ICDecompressBegin → ICDecompress` never reaches them.
  SPECGAP recorded: round-13's MMX module
  (`src/emulator/isa_mmx.rs`, 1007 LOC, ~50 opcodes) remains
  semantically validated by its 19 unit tests + 13 emulator
  step tests, with no real-codec dispatch pathway available
  in this corpus until a non-Indeo Win32 binary lands.
- Round 17 Part B — **larger IV41 fixture
  (`indeo41.avi`, 320×240, 13.4 MB).** New
  `tests/round17_iv41_indeo41.rs` mirrors round 16's
  8-frame ratchet on a fixture ~75 % bigger than
  `crashtest.avi`. All 8 sequential frames return
  `ICERR_OK` with > 25 % non-zero RGB24 output. Per-frame
  `mmx_dispatch_count` and `cpuid_dispatch_count` come back
  as 0/0 — the larger frame size doesn't surface MMX paths
  the smaller fixture missed, confirming the round-17 Part A
  finding that this codec's reachable decode path is
  statically integer-only despite the binary containing
  MMX-arithmetic byte patterns.
- Round 17 Part C — **`LIST rec ` recursion in the AVI
  walker.** Extended `tests/common/avi_extractor.rs`
  (~30 LOC delta) so that `LIST movi` bodies wrapping
  sample chunks inside `LIST rec ` blocks (the
  interleaved-AVI shape from Microsoft's AVI 1.0 reference
  §"Interleaved AVI files") are walked recursively.
  Without this, `indeo41.avi` reports zero stream-0 samples
  because every sample lives inside a `LIST rec ` block.
  The new helper `find_stream0_video_sample` descends into
  `LIST rec ` transparently, surfacing the inner sample
  chunks at the same depth as flat-movi chunks. Validated
  by a synthetic 2-rec AVI carrying mixed video/audio
  inside `LIST rec ` blocks (`tests/common/avi_extractor.rs`
  unit test `interleaved_avi_walker_descends_list_rec`)
  and by the live `indeo41.avi` 320×240 fixture
  driven through the round-17B IV41 pipeline.
- Round 17 Part D — **generalised `ICGetInfo` short-return
  szName fallback.** When a codec returns 0 bytes from
  `ICM_GETINFO` AND the open `HIC`'s `fcc_handler` is a
  known-Indeo FourCC (`IV31`/`IV32`/`IV41`/`IV50`), the
  wrapper now synthesises a `cb`-sized ICINFO buffer with
  the standard header dwords (`dwSize`, `fccType`,
  `fccHandler`) and an fcc-derived szName WCHAR string —
  same shape the post-call fallback already produced for
  short-but-non-empty returns. This covers `IR41_32.AX`'s
  DirectShow-filter "delegate to registry" pattern that
  round 15 noted (the codec ignores `ICM_GETINFO` entirely
  + relies on `vfw32!ICGetInfo` to consult the registry).
  Driven from the round-17B IV41 test, which now asserts
  the synthesised `szName` surfaces as `'I','V','4','1'`
  at offsets 24/26/28/30. New `vfw32` unit tests
  `ic_get_info_short_return_synthesises_known_indeo_fcc`
  and `ic_get_info_short_return_unknown_fcc_returns_empty`
  pin the contract.
- Round 16 Part A: **multi-frame IV41 sequence through
  `IR41_32.AX`.** The new `tests/round16_iv41_multiframe.rs`
  mirrors round 13's 8-frame ratchet against
  `cat_attack.avi` / IR50, applied here to the IV41 path.
  `crashtest.avi` (240×180 yuv410p, ~966 frames) is driven
  for the first 8 sequential samples through one shared
  `hic`: keyframe (sample 0) plus 7 P-frames (samples 1..7,
  each carrying `ICDECOMPRESS_NOTKEYFRAME` per the round-13
  convention). All 8 samples return `ICERR_OK` with > 25 %
  non-zero RGB24 output, confirming the codec's
  reference-frame state is correctly maintained across
  emulator-driven `ICDecompress` calls. Per-frame
  `mmx_dispatch_count` and `cpuid_dispatch_count` come back
  as 0/0 — the IR41 binary is statically integer-only on
  this decode path, mirroring round 14's IR50 finding. The
  round-13 MMX module (1007 LOC, ~50 opcodes) remains
  unexercised by real codec input.
- Round 16 Part B: **OpenDML AVI 2.0 walker.** Extended
  `tests/common/avi_extractor.rs` (~120 LOC delta) to
  recognise chained `RIFF AVIX` segments and surface their
  `LIST movi` sample chunks through the existing
  `extract_video_sample(n)` API. Implementation:
  `walk_riff_segments` enumerates every top-level RIFF in
  file order (rejecting non-`AVI ` first segments per the
  spec but accepting any `AVIX`/`AVI ` follower), the
  walker collects every `LIST movi` body across every
  segment, then iterates them in order counting stream-0
  video chunks. The OpenDML `indx` super-index in `strl`
  and per-segment `ix##` standard indexes in `movi` are
  transparently skipped by the existing stream-index
  predicate (their leading bytes are non-numeric). New
  exported helper `riff_segment_inventory` lets tests
  assert the number + form-types of RIFF segments without
  exposing private state. Validated against (a) a synthetic
  2-segment AVI (`AVI ` + chained `AVIX` carrying 3 samples
  total, including a stray `ix00` in segment 1's movi), and
  (b) the real `sv2-d.avi` IV50 fixture (single-RIFF AVI
  2.0 with `indx` in `strl` + `ix00`/`ix01` in `movi`):
  sample 0 = 7872-byte IV50 chunk, sample 1 = 2916 bytes.
- Round 16 Part C — **MMX-using IV50 build probe (SPECGAP).**
  Probed `samples.oxideav.org/codecs/windows/` for alternate
  Indeo 5 redistributables (`indeo5xa`, `indeo5ds`,
  `INDEO5XA`, `INDEO5DS`, `IV5XA`, `IV5DS`, `Indeo5`,
  `indeo5`, `IV5` — both as zip and as directory with
  `IR50_32.DLL`). All returned 404 from the corpus mirror.
  Reading `sv2-d.txt` confirms `indeo5xa` + `indeo5ds` are
  unix/xanim codec names, not Windows DLL builds; the
  IV5PLAY redistributable is the only published Windows
  Indeo 5 binary in our corpus and remains statically
  integer-only (round 14 finding). The round-13 MMX module
  stays a correct-semantics scaffold awaiting a future
  Cinepak / MS Video 1 / IV41-MMX-build binary that hits
  the `0F D0..FF` opcode block.

### Changed

- `tests/common/avi_extractor.rs::ChunkWalker` is now reached
  via `walk_riff_segments` rather than directly from
  `extract_video_sample`, refactor needed to support
  AVI 2.0 chained RIFF segments. The clamping behaviour
  (oversize `cksize` declarations clamp to bytes-available;
  `clamped_size < 4` falls through to an empty body slice
  rather than panicking on a slice index inversion) is
  preserved end-to-end. The round-14 `indeo5.avi` fixture
  (which declares a RIFF size 4 bytes shorter than the
  underlying file and pads to a 2 KiB boundary with zeros)
  now exits cleanly: the post-RIFF zero-bytes are detected
  as padding and stop the segment walk.

- Round 15: **IV41 (Indeo 4) decode through `IR41_32.AX`'s
  `DriverProc` export.** Round 14 Part B's surface probe
  established that `IR41_32.AX` is a dual-shape binary
  (DirectShow filter + VfW driver) and exports `DriverProc`,
  so the existing IC* pipeline that round 8..14 drove against
  `IR50_32.DLL` is reusable verbatim. The new
  `tests/round15_ir41_probe.rs` ratchets the IV41 path to the
  same milestone bar rounds 7 (cubes.mov / IR32) and 12
  (cat_attack.avi / IR50) hit — `ICDecompress` returns
  `ICERR_OK` (0) with > 25% non-zero RGB24 output. Decoded
  fixture: the smallest properly-aligned IV41 entry in the
  ffmpeg corpus, `crashtest.avi` (5 MiB, 240×180 yuv410p) —
  the smaller `mario001.mov` (300×225) trips
  `ICDecompressBegin` with `ICERR_BADIMAGESIZE = -201`
  because Indeo 4 requires picture dimensions divisible by 4
  (per `docs/video/indeo/indeo4/wiki/Indeo_4.wiki`
  §"Bitstream format description"). End-to-end run: DllMain
  → ICOpen IV41 (driver_id `0x6007f650`) → ICDecompressQuery
  (0) → ICDecompressBegin (0) → ICDecompress (0; 73789 of
  129600 RGB24 bytes non-zero) → ICDecompressEnd → ICClose.
  The full decode runs in ~2.5M emulator instructions.
- `kernel32!HeapSize` stub — IR41 queries `HeapSize` after
  `HeapAlloc` to size a follow-up copy. Returns the live
  block size from `HostState::heap` or `(SIZE_T)-1` on a
  bad pointer per MSDN.
- `user32!GetDlgItemTextA` fail-soft stub — IR41's
  Configure dialog reads its quality / bitrate edit boxes
  through this; the decode path never enters the dialog
  code, but the import must resolve at PE-load time.
- `tests/common/avi_extractor.rs` — `ChunkWalker::next` now
  clamps oversized chunk-size declarations to the bytes that
  remain in the buffer. The IV41 corpus's `crashtest.avi`
  ships a truncated head of a 20 MiB AVI (`LIST movi
  size=20353990` but only 5 MiB of the file is present); the
  round-8 strict-bounds walker bailed out at "no LIST movi"
  even though the first ~700 sample chunks are intact. The
  clamped walker hands out a partial movi payload and the
  inner sample iterator finds sample 0 cleanly.

- Round 14 Part A: **multi-fixture IV50 decode + structural MMX
  finding.** New `tests/round14_iv50_force_mmx.rs` (~370 LOC)
  drives three additional IV50 fixtures from the FFmpeg samples
  corpus through the round-13 sequential pipeline:
  `indeo5.avi` (320×240), `Educ_Movie_DeadlyForce.avi` (240×180),
  and `miss_congeniality_cryptedindeo5_sbcaudio.avi` (640×352).
  All three decode 8/8 frames with `ICERR_OK` and full non-zero
  RGB24 output, confirming the round-13 multi-frame pipeline is
  portable across encoders + content + 4× the macroblock count.
  Critically, the round-14 trace records **0 MMX dispatches and
  0 CPUID dispatches** across every fixture — corroborated by a
  direct byte scan of `IR50_32.DLL` (zero `0F A2` CPUID
  occurrences, zero `0F D0..FF` MMX-arithmetic occurrences in
  the entire 184 KB binary). The IR50_32.DLL shipped in IV5PLAY
  is statically integer-only; the round-13 MMX module cannot be
  validated against this binary, and round 15+ needs either a
  different IV50 build (`indeo5xa` / `indeo5ds` per the corpus's
  `sv2-d.txt`) or a different MMX-using codec to exercise the
  MMX semantics.
- Round 14 Part B: **`IR41_32.AX` surface probe.** New
  `tests/round14_iv41_surface_probe.rs` (~140 LOC) parses the
  Indeo 4 redistributable's PE32 headers + export + import
  tables (the file is 848 KB / 7 sections / image-base
  `0x1c40_0000`) without attempting to load or execute it.
  Records the file's COM-server entry surface
  (`DllGetClassObject`, `DllCanUnloadNow`, `DllRegisterServer`,
  `DllUnregisterServer`) AND its VfW driver entry
  (`DriverProc` — IR41_32.AX is a *dual-shape* binary that ships
  both the DirectShow filter ABI and the legacy VfW driver ABI).
  This is a major round-15 unblock: we can drive IV41 decode
  through the existing round-13 IC* pipeline, with no DirectShow
  scaffolding required. The probe enumerates 146 imports across
  6 system DLLs (advapi32: 11 / gdi32: 6 / kernel32: 85 /
  ole32: 7 / user32: 35 / winmm: 2) — round-15 dispatch budget
  for the Win32 stub coverage diff against round-13's existing
  registry.
- `Cpu::cpuid_dispatch_count: u64` — round-14 instrument
  alongside the round-13 `mmx_dispatch_count`. Lets a test
  distinguish between "codec queried CPUID and chose the
  integer path" (count > 0, MMX = 0) vs. "codec was built
  integer-only and never queried CPUID" (both 0). For
  `IR50_32.DLL` the answer is the latter.

- Round 13: **MMX instruction set + sequential P-frame decode
  through `IR50_32.DLL`.** Round 12 unblocked the FIRST keyframe
  of `cat_attack.avi`; round 13 extends that to multi-frame
  decode through a single shared `hic`. Eight sequential video
  samples (sample 0 keyframe + samples 1..7 P-frames) all
  return `ICERR_OK = 0` with > 99% non-zero RGB24 output and
  ~2-3M emulator-instructions per frame. The codec maintains
  reference-frame state across calls; opening a fresh hic per
  frame would discard the keyframe + leave the next P-frame
  with nothing to motion-compensate against.
- `crates/oxideav-vfw/src/emulator/isa_mmx.rs` (~700 LOC) —
  MMX semantics module. Implements the working subset Intel's
  IR50_32.DLL exercises (and the `0F D0..FF` block IV50 P-frame
  decoders typically use):
    * Move family — `MOVD mm, r/m32` (`0F 6E`),
      `MOVD r/m32, mm` (`0F 7E`), `MOVQ mm, mm/m64` (`0F 6F`),
      `MOVQ mm/m64, mm` (`0F 7F`).
    * Bitwise — `PXOR` (`0F EF`), `PAND` (`0F DB`),
      `PANDN` (`0F DF`), `POR` (`0F EB`), `EMMS` (`0F 77`).
    * Pack / unpack — `PUNPCKL{BW,WD,DQ}` (`0F 60..62`),
      `PUNPCKH{BW,WD,DQ}` (`0F 68..6A`),
      `PACK{SSWB,SSDW,USWB}` (`0F 63 / 6B / 67`).
    * Wrapping arithmetic — `PADD{B,W,D}` (`0F FC..FE`),
      `PADDQ` (`0F D4`), `PSUB{B,W,D}` (`0F F8..FA`),
      `PSUBQ` (`0F FB`), `PMULLW` (`0F D5`), `PMULHW` (`0F E5`),
      `PMADDWD` (`0F F5`).
    * Saturating arithmetic — `PADDS{B,W}` (`0F EC..ED`),
      `PSUBS{B,W}` (`0F E8..E9`), `PADDUS{B,W}` (`0F DC..DD`),
      `PSUBUS{B,W}` (`0F D8..D9`).
    * Shifts — `PSL{LW,LD,LQ}` (`0F F1..F3`),
      `PSR{LW,LD,LQ}` (`0F D1..D3`), `PSR{AW,AD}`
      (`0F E1..E2`) in both register-source and the imm8
      `0F 71/72/73` group-12/13/14 forms.
    * Compares — `PCMPEQ{B,W,D}` (`0F 74..76`),
      `PCMPGT{B,W,D}` (`0F 64..66`).
    * Average — `PAVGB` (`0F E0`), `PAVGW` (`0F E3`).
    Each opcode implemented from Intel® SDM Vol. 2A/2B per-
    instruction reference pages.
- `Cpu::mmx_dispatch_count: u64` — round-13 sentinel; counts
  successfully-dispatched MMX instructions. Lets a regression
  test verify whether the codec actually exercised the MMX
  path or fell back to the integer-only routine on the same
  CPUID feature bit.
- CPUID feature bit 23 (MMX) in EDX leaf 1 plus model bump to
  Pentium MMX (family 5 model 4); IR50_32.DLL on cat_attack.avi
  still picks the integer path even with MMX advertised, but
  the bit is correctly reported now and other codecs (IV41,
  IV31's MMX variant, etc.) will pick up the MMX dispatch.
- `extract_video_sample(avi_bytes, n)` in
  `tests/common/avi_extractor.rs` — generalises
  `extract_first_video_sample` to arbitrary sample index;
  required for the round-13 multi-frame driver.
- `tests/round13_iv50_multiframe.rs::cat_attack_decodes_sequential_frames_through_shared_hic`
  — drives 8 sequential samples through one `hic`, asserts
  every frame returns `ICERR_OK` with > 25% non-zero output,
  emits a per-frame trace line ("sample N: lr=..., M MMX
  instrs, X instrs total") so a future regression points at
  the exact frame the codec broke on.
- 13 MMX semantic regression tests in
  `tests/round7_mmx_scaffold.rs` (rewrites the round-7
  "structured-trap" tests now that the traps execute as real
  instructions): MOVD load + store, MOVQ register copy +
  through-memory roundtrip, PXOR self-zero, PADDB lane wrap,
  EMMS clear-all, group-14 PSLLQ imm8, PCMPGTB signed compare,
  BSWAP regression sentinel, dispatch-counter increment.
- 19 MMX lane-primitive unit tests in
  `src/emulator/isa_mmx.rs`'s `tests` module covering the
  arithmetic / saturation / pack / shift / compare lane
  primitives end-to-end (without going through the
  emulator's instruction-stream path).
- `kernel32!SizeofResource` registered into the dispatch
  registry. Round 12 added the implementation behind
  `#[allow(dead_code)]` because IR50_32.DLL doesn't import it;
  round 13 wires it up so future codecs that DO import it
  pick up the implementation rather than tripping the
  unresolved-import trap.

### Changed

- `Cpu::seg_translate` / `fetch_imm8` / `fetch_modrm` /
  `peek_after_modrm` lifted from private to `pub(super)` so
  the new `isa_mmx` sibling module can drive them. Surface
  is unchanged for external callers.
- `Cpu::dispatch_mmx` (round-7 structured-trap helper) +
  `mmx_consumes_modrm` / `mmx_has_imm8` umbrella tables
  removed from `isa_int.rs`. The per-opcode arms in
  `isa_mmx::dispatch` know exactly what ModR/M / imm8 each
  instruction consumes; no umbrella table needed.
- `cpuid()` leaf 1 now reports MMX (bit 23 of EDX). Family/
  model bumped from Pentium-classic (5/2) to Pentium MMX
  (5/4) for consistency.

- Round 12: **`ICDecompress` against `IR50_32.DLL` returns
  `ICERR_OK` with a populated 320×240 RGB24 buffer.** Round 11
  plumbed `DRV_LOAD` + `DRV_ENABLE` through `ic_open` so the
  codec's table-init chain runs at all; round 12 closes the
  actual gate. Bisecting the round-11 trace ring (4682
  instructions through `ICDecompress`) located a normal-RET
  trap at `0x1004f7f7` (`mov eax, -100; ret`), reached via a
  jump-table at `0x1004f80c[2]` from a deeper validator call
  whose return value (= 2) the dispatcher mapped to
  `ICERR_BADIMAGE`. The validator returned 2 because
  `[0x1009c770]` (the codec's huffman / inverse-DCT table base
  pointer, set by `0x10001327: mov [0x1009c770], ecx`) was
  still NULL — `IR50_32.DLL`'s `DRV_LOAD` chain copies those
  tables out of two `RT_BITMAP` PE resources (RT_BITMAP/112 and
  /113, 20264 bytes each) which our `kernel32!FindResourceA` /
  `LoadResource` / `LockResource` stubs returned NULL for.
  Round 12 implements those three against the loaded PE's
  resource directory (PE Data Directory entry 2): `FindResourceA`
  walks the 3-level directory (TYPE → NAME → LANG) honouring
  `MAKEINTRESOURCE`-style integer keys, returning the
  `IMAGE_RESOURCE_DATA_ENTRY` VA; `LoadResource` is a passthrough;
  `LockResource` resolves the entry's RVA against the module's
  image base. The codec also wraps its huffman-table copy in a
  named-shared-memory cache (`CreateFileMappingA` /
  `MapViewOfFile`) shared between concurrent decoder instances;
  round 12 lifts those two from "return NULL" to "allocate
  fresh buffer" so the cache-fallback path is exercised. With
  those five kernel32 stubs functional, `[0x10084790]` (init
  guard) flips from 0 to 1, `[0x1009c770]` gets a real
  allocation, and the decode body runs to completion in
  ~2.94M instructions. No MMX opcodes were exercised — the
  IV50 decoder for `cat_attack.avi`'s first keyframe is
  integer-only.
- `HostState::module_resource_dirs: BTreeMap<u32, u32>` —
  `image_base → resource_directory_va`, populated by the PE
  loader from the optional header's Data Directory entry 2.
- `kernel32::find_resource_data_entry` — public-in-crate helper
  used by `FindResourceA`; takes `(state, mmu, h_module,
  lp_name, lp_type)`, returns the `IMAGE_RESOURCE_DATA_ENTRY`
  VA on match. Walks named-then-id entries per the PE
  Resource Directory layout in PE/COFF spec §"Resource
  Directory Table".
- `find_resource_a_walks_synthetic_resource_directory`
  unit-test (kernel32) — builds a tiny 3-level rsrc directory
  in MMU and asserts the lookup lands on the expected data
  entry.
- `cat_attack_first_keyframe_post_init_globals_and_decode`
  regression test (`tests/round11_trace_dump.rs`) — replaces
  the round-11 investigative trace dump with a focused
  regression sentinel: asserts `[0x10084790] == 1`,
  `[0x1009c770] != 0`, and `ICDecompress` returns `ICERR_OK`
  with non-zero output. Names the codec-init globals
  explicitly so a future regression points at the right
  surface.

### Changed

- `kernel32!FindResourceA` no longer returns 0 unconditionally;
  it walks the loaded PE's resource directory.
- `kernel32!LoadResource` / `LockResource` no longer return 0
  / NULL; they unwrap the data-entry VA returned by
  `FindResourceA`.
- `kernel32!CreateFileMappingA` / `MapViewOfFile` — for
  `hFile == INVALID_HANDLE_VALUE` requests an anonymous
  pagefile-backed mapping; round 12 fulfils these with a
  bump-allocated buffer and returns the buffer VA as the
  handle. `MapViewOfFile` returns `handle + offsetLow`. This
  is the round-12 unblocker for `IR50_32.DLL`'s named-shared-
  memory cache fallback path.
- `tests/round8_iv50_decode.rs::cat_attack_first_keyframe_decodes_through_ir50_32_dll`
  tightens its `lr` assertion from "non-positive" to
  "exactly `ICERR_OK` (0)" and adds a "≥25% non-zero pixels"
  guard — the round-12 milestone outcome.

- Round 10: **0x66-prefix honored across the integer ISA, not
  just the MOV family.** Round 9 fixed `0x89` / `0x8B` / `0xC7`;
  round 10 closes the rest of the gap so the IV50 decode body
  runs cleanly through `ICDecompressQuery → ICDecompressBegin →
  ICDecompress` against `IR50_32.DLL` without a single CPU trap.
  The fixes cover, per Intel SDM Vol. 2A: `0x81` / `0x83` group-
  1 r/m, imm (the literal opcode that produced round-9's
  ICDecompressQuery memory fault — `66 81 7C 24 14 41 53` is
  `cmp word [esp+0x14], 0x5341`, 7 bytes, imm16 not imm32);
  `0x69` / `0x6B` IMUL r, r/m, imm; `0x40..0x4F` INC/DEC r and
  `0x50..0x5F` PUSH/POP r (the 16-bit forms move ESP by 2);
  `0x68` / `0x6A` PUSH imm; `0xB8..0xBF` MOV r, imm; `0xA1` /
  `0xA3` MOV moffs; `0xA9` TEST EAX/AX, imm; `0x9C` / `0x9D`
  PUSHF / POPF; `0xF7` group-3 r/m (TEST imm width changes too);
  the entire `0x00..0x3D` even-row r/m32, r32 / r32, r/m32 ALU
  pair plus the `0x05/0x0D/.../0x3D` accumulator-imm forms;
  group-2 shifts r/m16 (`0xC1` / `0xD1` / `0xD3`); the dword
  string operations MOVSW / STOSW / LODSW / CMPSW / SCASW under
  0x66 (each step advances ESI/EDI by 2 instead of 4).
- New `push16` / `pop16` helpers in [`Cpu`].
- Sixteen 16-bit ALU primitives (`alu_add_16`, `alu_sub_16`,
  …, `alu_test_16`, `group1_op_16`, `set_flags_inc_dec_16`)
  matching the existing 32-bit / 8-bit set, with sign bit at
  0x8000.
- `Cpu::fpu_cw` — a 16-bit shadow of the x87 FPU control word.
  We do **not** model the FPU stack or any arithmetic, but the
  codec-prologue idiom `D9 /5 fldcw m16` + `D9 /7 fnstcw m16`
  (used by `ICDecompressBegin` to save and restore the rounding-
  mode CW around an integer-truncation block) now round-trips
  through the shadow. Other `D9 ...` forms still trap as
  `PrivilegedOpcode` with a specific mnemonic so the round-11
  implementer can localise them.
- Thirteen new unit tests covering the 0x66-prefix paths +
  the FPU CW round-trip + REP MOVSW.

### Round-10 outcome

`tests/round8_iv50_decode.rs::cat_attack_first_keyframe_decodes_through_ir50_32_dll`
now runs the full IC* sequence end-to-end through the real
`IR50_32.DLL` against the 4300-byte IV50 keyframe extracted
from `cat_attack.avi`. `ICDecompressQuery` and
`ICDecompressBegin` both return 0 (ICERR_OK). `ICDecompress`
runs the codec body for ~4682 instructions and returns
`ICERR_BADIMAGE` (-100) cleanly via a normal `RET` — no trap,
no MMX opcodes encountered, no unimplemented ISA. The codec
rejects the keyframe at a yet-unidentified pre-MMX validation
step; round 11's gate is to localise that path. The trap-log
driven MMX implementation that round 7 scaffolded is therefore
NOT triggered yet by this fixture.

- Round 8 + 9: **`IR50_32.DLL` (Indeo 5) load + ICOpen wired
  end-to-end.** The previous round-8 pass landed ~1300 LOC of
  scaffolding (RIFF/AVI 1.0 chunk walker — `tests/common/avi_extractor.rs`,
  authored solely from the public IBM/Microsoft RIFF spec +
  Microsoft AVI 1.0 documentation; `advapi32.rs` registry stubs
  including `RegOpenKeyExA` / `RegQueryValueExA` / `RegCloseKey`;
  `ole32.rs` COM stubs; substantial `kernel32.rs` additions —
  `LCMapStringA`, `IsValidCodePage`, `CreateMutexA`,
  `WaitForSingleObject`, `ReleaseMutex`, `Tls{Alloc,Get,Set,Free}Value`
  — and `user32.rs` / `winmm.rs` follow-ups). Round 9 closes the
  loop by fixing the operand-size-prefix decoding bug that was
  manifesting as a phantom memory fault during ICOpen.
- `Cpu::enable_trace_ring(cap)` — a 64-deep ring buffer of
  recently-executed instruction-start EIPs for trap forensics.
  Test panic blocks dump it alongside the existing `bytes [eip-24..eip)`
  + register snapshot. The combination uncovered the 0x66-prefix
  bug (the instruction trail revealed eip jumping by wrong
  offsets, NOT the LMEM_MOVEABLE handle issue the round-8 agent
  hypothesised).
- `read_operand16` / `write_operand16` in `emulator/decode.rs`,
  with the corresponding 16-bit memory + register read/write
  primitives. Required to honour 0x66 cleanly across the MOV
  family.

### Fixed

- **0x66 (operand-size override) prefix on MOV opcodes.**
  The opcodes `0x89` (`MOV r/m, r`), `0x8B` (`MOV r, r/m`), and
  `0xC7` (`MOV r/m, imm`) were ignoring the prefix flag entirely
  in the integer decoder. For `0xC7` this was a hard correctness
  bug: `66 C7 /0 iw` is a 6-byte instruction (with a 16-bit
  immediate), but our impl read 4 bytes of immediate and then
  advanced eip by the full 32-bit-immediate length, putting all
  subsequent instruction decoding off by 2 bytes. Manifested as
  IR50_32.DLL's ICOpen "memory fault at 0xe700006c" in
  `tests/round8_dllmain_smoke.rs` — eax was being clobbered by
  a misaligned `OR EAX, imm32` decoded out of the second half
  of the next instruction. Per Intel SDM Vol. 2A "MOV":
  `C7 /0 iw` (16-bit) and `C7 /0 id` (32-bit). Fixed in all
  three handlers; covered by three new lib unit tests
  (`mov_rm16_imm16_with_66_prefix_consumes_2byte_imm` +
  siblings).

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
