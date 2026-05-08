# oxideav-vfw

Pure-Rust 32-bit x86 emulator + PE loader + Video for Windows host
that lets [oxideav](https://github.com/OxideAV/oxideav-workspace)
delegate decoding (and eventually encoding) to legitimately-licensed
Windows codec DLLs on **any** platform — Linux, macOS, FreeBSD,
Windows itself. The codec never executes on the host CPU; it runs
through a software-interpreter sandbox.

## Status

**Round 23 — MSMPEG4 v3 ffmpeg-oracle keyframe cross-check +
I+P 2-frame decode.** Round 22 landed the first MP43 keyframe
decode but only checked "any non-zero output". Round 23
raises the bar:

* **A — ffmpeg-oracle PSNR cross-check.** A new
  `tests/round23_mp43_pframe_and_oracle.rs::mp43_keyframe_matches_ffmpeg_oracle_psnr`
  spawns `ffmpeg -frames:v 1 -pix_fmt bgr24 -f rawvideo -` as
  a black-box validator and compares mpg4c32's BGR24 output
  against ffmpeg's. Today's run reports **PSNR 42.90 dB** on
  the solid-blue 176×144 keyframe — well above the 30 dB
  pass floor. Drift is YUV→BGR matrix difference (mpg4c32
  output `ff 02 04`, ffmpeg swscale `ff 01 01`); no
  structural decode mismatch. Skipped gracefully when ffmpeg
  is not on `PATH`.
* **B — sequential I + P decode.** The
  `i-frame-then-p-frame-176x144` fixture (a `-vtag DIV3`
  2-frame I+P encode whose elementary bitstream is plain
  MSMPEG4 v3) decodes through both frames: `ICERR_OK` on
  both, I-frame writes 67 639 / 76 032 non-zero bytes,
  P-frame writes 67 698 / 76 032 — mpg4c32 maintains its
  reference-frame state across calls. The 97-byte P-frame
  consumes 1.13 M emulator instructions (vs. 13 M for the
  keyframe).
* **C — state-field audit.** The round-22 wrapper-handshake
  plant at `[+0xb4..+0xc8]` survives `ICDecompressBegin`
  intact; the disasm-flagged copy target at
  `[+0x15b0..+0x15c4]` stays zero throughout BEGIN +
  keyframe DECOMPRESS — the relocation path does not fire
  under the current plant.

**Round 22 — MSMPEG4 v3 ICDecompressBegin + first keyframe
decode unblock.** Round 21 left ICDecompressBegin returning
`ICERR_INTERNAL` (`-100`). Static disasm of
`mpg4c32!DriverProc+0x14e2` traced the failure to a private
v3-only handshake: when DRV_OPEN tags `[esi+0x18]=3` for
fccHandler `MP43`, the begin path checks for a 20-byte
`{ DWORD == 1, GUID b4c66e30-0180-11d3-bbc6-006008320064 }`
record at `state[+0xb4..+0xc8]` — fields that no public
ICM_* message writes; they're populated by the wrapping
DirectShow / DMO codec factory layer real WMP hosts the
codec inside. `vfw32::ic_decompress_begin` now plants the
wrapper's contribution directly. Five new x87 D9 reg-form
sub-forms (FSIN, FCOS, FPREM, FSCALE, and FRNDINT relocated
to the correct `(7, 4)` slot) unlock the IDCT trig-table
init the begin path runs after the GUID gate clears.

| Codec | DLL | Test fixture | Round | `ICDecompress` |
|-------|-----|--------------|-------|----------------|
| Indeo 3 (IV31) | `IR32_32.DLL` | `cubes.mov` 160×120 | 7 | `ICERR_OK` |
| Indeo 5 (IV50) | `IR50_32.DLL` | `cat_attack.avi` 320×240 (+3 more in r14) | 12 / 13 / 14 / 20 | `ICERR_OK` (8/8 frames; **MMX kernels active**) |
| Indeo 4 (IV41) | `IR41_32.AX` | `crashtest.avi` 240×180 + `indeo41.avi` 320×240 | 15 / 16 / 17 / 20 | `ICERR_OK` (8/8 frames each; **MMX kernels active**) |
| MSMPEG4 v3 (DIV3/MP43) | `mpg4c32.dll` (VfW) | `fourcc-MP43` keyframe + `i-frame-then-p-frame` I+P | 22 / 23 | `ICERR_OK` (I+P 2/2 frames; **PSNR 42.9 dB vs ffmpeg oracle**) |
| WMV1/2 (WMV1/WMV2) | `wmvds32.ax` | TBD | 21 | PE-load ✓ (`mpg4ds32.ax` + `wmvds32.ax` DS filters); DriverProc unexplored |

Round 20 sub-goal A localised the MMX gate to a registry
probe: the codec calls
`RegOpenKeyExA(HKLM, "HARDWARE\DESCRIPTION\System\FloatingPointProcessor", …)`
and only sets `[ebp-8] = 1` (which propagates into
`[0x1c4a9a38] = 1` "use MMX kernels") if the call returns
ERROR_SUCCESS. We synthesise the FloatingPointProcessor key
as present (every Win9x/NT machine had it). After the unblock
the IV50 IR50 8-frame `indeo5.avi` decode reports
**11.5M MMX dispatches** total (1.5M/frame) and the IV41
8-frame pipeline reaches 138/1032 (13%) of the codec's MMX
opcode bytes. RCL/RCR instruction forms (group-2 reg=2/3 in
`C0/C1/D0/D1/D2/D3`) needed implementation in the integer
ISA — they were unreached pre-round-20.

Round 20 sub-goal B closes the 13 PE-load blockers for
`mpg4c32.dll` (kernel32 CreateEventA / CreateThread / SetEvent
+ msvcrt new / delete / _adjust_fdiv / _except_handler3 /
_initterm / malloc / free + user32 GetScrollPos /
SetScrollPos / SetScrollRange + winmm GetDriverModuleHandle).
A new `Registry::register_data` channel handles
`_adjust_fdiv`, which is a 4-byte data symbol the codec reads
through `mov reg, [iat]; mov reg, [reg]` — putting a thunk
there fails on the second deref. We pre-allocate a 4 KiB R/W
region at `0x70100000` and patch the IAT slot to point inside
it (initial value 0 = "no Pentium-FDIV fix-up needed").
`Sandbox::call_dll_main` now falls back to the PE
`AddressOfEntryPoint` when no `DllMain` named export is
present (mpg4c32 ships only `DriverProc`). With this in
place, mpg4c32's CRT entry runs through `malloc` + `_initterm`
to completion.

Round 21 closes the DRV_OPEN gate for `mpg4c32.dll`: real
x87 FPU semantics in a new `emulator::isa_fpu` module
(eight ST(i) slots + TOP + status word, full m32/m64/m80
load+store + arithmetic + condition codes) light up the
`_initterm` static-ctor path, and `vfw32::ic_open`
canonicalises the ICOPEN fcc fields to lower case to match
the `vfw.h ICTYPE_VIDEO = mmioFOURCC('v','i','d','c')`
ABI. ICOpen('VIDC','MP43') now returns hic=1; the next
blocker is `ICDecompressBegin`'s `ICERR_INTERNAL`. Sub-goal
B adds three more msvcrt stubs (`_onexit`, `__dllonexit`,
`sprintf`) that close the PE-load gate for `mpg4ds32.ax` +
`wmvds32.ax`.

The full design contract lives in
[`OxideAV/docs/winmf/winmf-emulator.md`](https://github.com/OxideAV/docs/blob/master/winmf/winmf-emulator.md).

This round delivers:

* `emulator::mmu` — flat 4 GiB R/W/X-permissioned MMU with
  sparse 4 KiB pages.
* `emulator::regs` + `emulator::decode` + `emulator::isa_int`
  — i386 integer ISA interpreter (CPUID returns canned
  Pentium-class response; privileged + far calls + segment
  loads trap; MMX deferred to round 2).
* `pe` — PE32 loader: DOS + PE header parse, section mapping,
  base relocation, IAT resolution, export-by-name. Rejects
  PE32+ / .NET / packed binaries.
* `win32::kernel32` — 12 stubs (heap + atomics +
  OutputDebugStringA + GetTickCount + LoadLibraryA +
  GetProcAddress).
* `runtime::Sandbox` — public entry point: load a DLL, call
  `DllMain(DLL_PROCESS_ATTACH)`, return cleanly.

Round 2 will add: MMX ISA + the `vfw32` stubs (`ICOpen` /
`ICDecompress*` / `ICClose` / `ICGetInfo`) + cdecl plumbing,
and a "decode one Cinepak frame" end-to-end test.

## Why this exists

The crate has **two co-equal end-uses**:

### 1. Rare-codec compatibility

Run codecs the project would otherwise permanently shelve.
Modern codecs (H.264, HEVC, AV1, VP9, Opus, AAC, …) have
pure-Rust decoders elsewhere in the oxideav workspace, but some
old codecs were never published with a public spec and never had
a clean-room reverse-engineering writeup defensible enough for
the project's standard:

* Indeo 4 / Indeo 5 (Intel)
* Sorenson Video 1 / 3
* MS-MPEG-4 v1 / v2 / v3 (DivX-:-) era)
* On2 VP3-pre-Theora variants, VP4, VP5
* Cook (RealAudio)
* Various Microsoft speech codecs (ACELP, GSM 6.10 MS variant,
  TrueSpeech, Voxware, Lernout-and-Hauspie, …)
* DivX 3/4/5 early versions
* 3ivx and other early MPEG-4 variants

For these formats, the original Win32 codec DLLs are
legitimately redistributable (shipped in K-Lite codec packs, in
Microsoft Windows Media Player redistributables, in QuickTime
installers, in old Linux `vfw_codecs` packages). The bridge says
"we don't decode them ourselves; we delegate to the user's
licensed codec running in our sandbox".

### 2. Reverse-engineering aid

Once a codec runs in the sandbox, the same emulator becomes a
clean-room **research instrument**: every guest memory access,
every Win32 stub call, and (optionally) every guest instruction
crosses a Rust boundary, so the emulator can faithfully record
what the codec is doing on a target bitstream. Output is JSONL
events; downstream tooling (Python/jq) post-processes them into
the kind of behavioural traces the workspace's
specifier→extractor→implementer round procedure consumes when
producing clean-room codec specs from scratch.

This is gated behind the `trace` Cargo feature (off by default
because it adds branches on emulator hot paths). See the
**Trace mode** section of `docs/winmf/winmf-emulator.md` for the
event schema and the programmatic API
(`Sandbox::watch(addr, size, mode)` /
`Sandbox::set_trace_sink(...)`). Lands post-round-5 — the
infrastructure is documented today so that round-5+ work
designs around it rather than retrofits.

The two end-uses share the same compatibility-track machinery
(MMU, ISA interpreter, PE loader, Win32 stubs) — the
research-track is a layered set of probes on top, not a fork.

## How

Four layers, all pure Rust, all aimed at
`#![forbid(unsafe_code)]`:

1. **Emulator** — flat 4 GiB virtual MMU, i386 integer +
   eventually MMX, page-grained R/W/X permissions, every memory
   access bounds-checked. No JIT, no host-CPU dependence.
2. **PE loader** — maps a PE32 DLL into emulator memory, applies
   base relocations, resolves the import address table against
   our Win32 stub surface, finds the entry point, calls
   `DllMain(DLL_PROCESS_ATTACH)`.
3. **Win32 stubs** — Rust functions exposed to the loaded DLL
   through the IAT. `kernel32` essentials (heap, atomics, TLS),
   `vfw32` for `ICDecompress*` dispatch, `msacm32` for the
   audio milestone. DirectShow / DMO / Media Foundation are
   later milestones if a target codec demands them.
4. **Codec wrapper** — `Box<dyn Decoder>` / `Box<dyn Encoder>`
   that drives the VfW `DriverProc` message dispatch and
   marshals data buffers across the sandbox boundary.

Performance: ~50–200 M instructions/sec interpreter throughput
on a modern CPU, which gives 10–40× realtime for
Cinepak-shaped codecs and 1.5–7× realtime for MS-MPEG-4 family.
Modern codecs (WMV9, H.264) would not be realtime in the
interpreter; that's why this crate is scoped to legacy / rare
codecs only. JIT is a future optimisation.

## Reading order

1. **`docs/winmf/winmf-emulator.md` §Goals and non-goals** —
   what this crate is and is not for.
2. **`docs/winmf/winmf-emulator.md` §Provenance and allowed
   references** — the IP discipline. Microsoft PE/COFF spec +
   MSDN + Intel x86 manual are allowed; Wine, Bochs, QEMU,
   ReactOS are forbidden.
3. **`docs/winmf/winmf-emulator.md` §Architectural overview**
   — the four layers and their responsibilities.
4. **`docs/winmf/winmf-emulator.md` §Milestones** — what
   round-1 ships, what later rounds add.
5. **`docs/winmf/winmf-emulator.md` §Safety boundary** — why
   the codec is fundamentally safer to run through this
   crate than any native-execution alternative.

## Cargo features

* **`registry`** (default): wire the crate into `oxideav-core`'s
  codec registry. Disable for standalone builds (`oxideav-vfw =
  { default-features = false }`) that just want the emulator
  + PE loader + Win32 host as a library, without the framework
  integration.

## Licence

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 Karpelès Lab Inc.
