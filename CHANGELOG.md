# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`data_rate` per-frame byte-ceiling knob on `SandboxedVfwEncoder`
  (round 178).** A third optional `CodecParameters.options` knob
  alongside the round-112 `quality` / `keyint` pair: `"data_rate"`
  (u32 bytes) is parsed once at construction time and threaded into
  `ICCompress`'s `dwFrameSizeLimit` slot on every per-frame call.
  `0` (default, and the value returned when the knob is absent or
  malformed) preserves the historical "codec chooses" behaviour;
  non-zero hints a per-frame byte cap, which an RTP / AVI muxer
  can use to bound MTU pressure on a fixed-rate transport without
  resorting to fragmentation. The knob is u32-verbatim (no clamp —
  unlike `quality`'s VfW-defined `0..10000` range, `data_rate` is a
  raw byte count whose only invariant is fitting in a `u32`; the
  codec is the arbiter of plausibility for outsized values).
  Replaces the pre-r178 hard-coded `frame_size_limit = 0` argument
  to `ic_compress` with `self.data_rate`.
  - New unit tests (`encoder_reads_data_rate_option_verbatim`,
    `encoder_data_rate_is_not_clamped_unlike_quality`,
    `encoder_tolerates_malformed_data_rate`) + the round-178
    integration test file (`tests/round178_encoder_data_rate_knob.rs`)
    cover the verbatim pass-through, the malformed-value fallback,
    and the all-three-knobs-coexist case. The default-knob test
    (`encoder_defaults_quality_and_keyint_to_zero`) now also asserts
    `data_rate == 0` to lock the additive contract.

- **Per-frame P-frame reference + quality / keyframe-interval
  knobs on `SandboxedVfwEncoder` (round 112).** The encoder now
  threads the previous raw input frame through `ICCompress`'s
  `lpbiPrev` / `lpPrev` slots on non-keyframe encodes — the
  inter-frame reference wiring the round-107 entry flagged as a
  bounded follow-up. After each successful `ICCompress` the
  bottom-up BGR24 input bytes are stashed in `prev_input_bytes`;
  the next P-frame passes them (with the input BIH) as the
  reference so the codec can encode a delta. This is the
  no-decoder-feedback-loop contract: we use the previous *raw*
  input as the reference (not the codec's reconstructed previous
  frame, which would require driving a parallel decoder); MS VfW
  codecs historically accept this, and codecs that demand the
  reconstructed reference still produce valid keyframe-only output
  because the keyframe path bypasses `prev_*` entirely.
  - Two optional `CodecParameters.options` bridge knobs, read once
    at `make_encoder` time: `"quality"` (u32, clamped to the VfW
    `0..10000` range; `0` = "codec chooses") is passed to
    `ICCompress`'s `quality` slot, and `"keyint"` (u32 frames; `0`
    = disabled) forces every Nth frame to a keyframe (frame 0 is
    always a keyframe). A malformed knob value falls back to the
    default rather than failing construction (best-effort policy:
    these are bridge knobs, not codec invariants).
  - New unit tests (`parse_option_u32_reads_decimal_and_falls_back,
    encoder_reads_quality_and_keyint_options_clamped,
    encoder_defaults_quality_and_keyint_to_zero,
    is_keyframe_honours_frame0_and_keyint`) + integration tests
    (`tests/round112_encoder_pframe_and_knobs.rs`) cover the
    option-parsing fallback, the clamp, the keyframe-cadence
    predicate, and the public `make_encoder` knob-wiring path.

- **Encode side of the ud-emulator bridge.** VfW (`Kind::Vfw`)
  codecs now register an `oxideav_core::Encoder` factory
  (`discovery::make_encoder`) alongside the existing decoder. The
  new `SandboxedVfwEncoder` is the encode-side mirror of
  `SandboxedVfwDecoder`: on the first `send_frame` it lazily loads
  the DLL, opens the codec in `ICMODE_COMPRESS`, and drives the
  `ICCompressQuery → ICCompressGetFormat → ICCompressGetSize →
  ICCompressBegin` setup handshake; each `receive_packet` flips the
  caller's top-down BGR24 plane into the codec's bottom-up input
  BIH, calls `ICCompress` (keyframe requested on frame 0), and
  surfaces the encoded bytes as a `Packet` carrying the
  codec-returned keyframe flag. `Drop` runs `ICCompressEnd +
  ICClose`. `register_codec_info` advertises `with_encode()` +
  the encoder factory only for `Kind::Vfw` records — DirectShow
  filters stay decode-only (no `ICCompress*` path through this
  bridge), and `make_encoder` rejects non-VfW kinds defensively.
  Per-frame P-frame reference state (`prev_bih`/`prev_bytes`) is
  not yet threaded — every frame encodes as an independent unit;
  inter-frame reference wiring is a bounded follow-up.
  - New unit tests (`make_encoder_{vfw_constructs_lazily,
    dshow_kind_is_unsupported,unknown_id_errors_cleanly}`) +
    integration tests
    (`tests/round107_encoder_trait_integration.rs`) cover the
    factory wiring, the video-only `send_frame` guard, and the
    missing-dims error path.

### Changed

- **BREAKING** — rewrite `oxideav-vfw` as a thin bridge over
  [`ud-emulator`](https://crates.io/crates/ud-emulator)`= "0.1"`
  (currently v0.1.3). Removed ~28k LOC of in-tree emulator / PE
  loader / Win32 stubs / DirectShow COM scaffolding / msadds32
  forensic test harnesses — that surface now lives upstream in
  `ud-emulator`, which was built as a near-verbatim mirror of
  this crate specifically to absorb it.
  - The two oxideav-specific layers (`discovery/` — FS walk +
    cache + per-DLL probe; the `Codec` / `Decoder` trait adapter
    inside `discovery::codec`) are retained verbatim and now
    call into `ud_emulator::*` paths instead of `crate::*`.
  - `register()` + the `oxideav_core::register!` invocation are
    unchanged; codec priority stays at 200.
  - Downstream consumers that historically wrote
    `oxideav_vfw::Sandbox` / `oxideav_vfw::Guid` /
    `oxideav_vfw::Bih` / `oxideav_vfw::DLL_PROCESS_ATTACH` /
    `oxideav_vfw::IID_*` / `oxideav_vfw::{TraceState,
    WatchMode, Watchpoint}` continue to compile via re-exports.
  - Forensic round-1 through round-70 integration tests
    (msadds32 ea3a, mp43 encode determinism, gdi32 cascade, …)
    are deleted from this crate; equivalents live in
    `ud-emulator`'s own corpus + the `ud vfw {probe,decode,
    encode}` CLI.
  - `examples/gen_msmpeg4_traces.rs` removed — trace generation
    is `ud vfw` territory.
  - Public modules `com`, `emulator`, `pe`, `runtime`, `trace`,
    `win32` are gone. Reach for them at
    `ud_emulator::{com,emulator,pe,runtime,trace,win32}`
    instead.

### Added

- vfw r70 (piece B / task #829): `Sandbox::ic_get_state(handle,
  &mut buf) -> Result<u32>` and `Sandbox::ic_set_state(handle, &buf)
  -> Result<()>` — host-side wrappers for the VfW `ICM_GETSTATE`
  (`0x5009`) and `ICM_SETSTATE` (`0x500A`) messages, mirroring the
  existing `ic_compress_*` family.  Required by oxideav-tracevfw
  to drive the codec encoder's per-quality-knob round-trip.
  `ic_get_state` returns the byte count the codec actually wrote
  (or its raw `LRESULT` for codecs that report `ICERR_UNSUPPORTED`
  — empirical finding for `mpg4c32.dll`, which reports `-1`
  meaning "no per-instance state to serialise via the VfW state
  surface").  `ic_set_state` returns `Ok(())` on `ICERR_OK` or
  surfaces the codec's failure `LRESULT` via `Win32Error::
  InvalidArgument`.  3-test integration harness at
  `tests/round70_ic_get_set_state.rs` (probe / round-trip /
  canned-driver smoke), 5 in-module unit tests in
  `src/win32/vfw32.rs`.

- vfw r70: `tests/round70_msadds32_ea3a_forensic.rs` — 4-test
  harness traces into `msadds32.ax`'s `0xea3a` helper called from
  RVA `0xe13c` inside `0xe0f4`, and **re-pins** the actual bail
  JCC that reaches the `0x80004005` E_FAIL stamp at `0xe2bb`.
  Round 69 hypothesised the bail was the `jne` at `0xe148` after
  `cmp [ebx+0x468], 0`; round 70 enumerates all 9 JCCs inside
  `0xe0f4`'s body that target `0xe2bb` (linear-byte scan) and
  identifies the actual bail as `0xe282: jge +0x37` after `cmp
  edi, [ebp+0x10]`.  At the bail moment `edi = 0x748`,
  `[ebp+0x10] = 0x748` — the codec walked its output-sample
  emission counter up to the declared sample-count bound and
  bailed via the loop-overflow path, NOT via the
  `[outer_this+0x468]` flag check (which empirically reads zero
  at every snapshot of `0xe141`).  Round 70 also re-confirms
  the round-63 `helper_addref_patch` is **retirable** on the
  ffmpeg-extradata path: phase 2's A/B (with-patch / no-patch)
  produces identical reach-sets across all armed sentinel sites.
  The `Sandbox::msadds32_patch_helper_addref` API is preserved
  for prior-round (r63/r64/r65/r68/r69) test backwards
  compatibility; round 70 phases 1, 3, and 4 all run without the
  patch.  Round-71 hand-off documented at
  `docs/codec/msadds32-receive-e-unexpected.md` §"Round 70".

### Changed

- Trace events of `kind=win32_call` now carry the cdecl size /
  pointer argument for the `msvcrt.dll` heap surface — `malloc`,
  `free`, `??2@YAPAXI@Z` (operator new), `??3@YAXPAX@Z`
  (operator delete), and the future `calloc` / `realloc`
  registrations. Previously the `args` field was always `[]` for
  these cdecl entries (the registry's `arg_dwords` field is `0`
  because the *caller* cleans the stack), forcing trace
  consumers to differentiate via call-site EIP. A new
  `cdecl_trace_arg_count(dll, name)` table in `src/win32/mod.rs`
  declares the per-call dword count for known cdecl shapes; the
  dispatch site reads them off the guest stack at call time and
  emits them as decimal values in the JSONL `args` array.
  Required by Auditor P1 — see
  `docs/video/msmpeg4/audit/06-sandbox-O3-quant-init.md` §5.2.3
  for the codec-context allocation localisation use-case.

### Added

- vfw r69: `tests/round69_msadds32_inner_decode_watch.rs` — five
  `Cpu::add_register_watchpoint` snapshots inside `msadds32.ax`'s
  inner-decode body at RVA `0xc887..0xc973` empirically falsify
  round-68's "one of the four NULL-arg guards fires" hypothesis.
  All four guards (`0xc898 / 0xc8a3 / 0xc8ac / 0xc8b7`) PASS;
  `arg0 = 0x60281010`, `arg2 = 0x900ffe9c`, `arg5 = 0x900ffecc`
  pinned non-NULL.  The E_FAIL bail at `0xc969` is NEVER reached
  on the round-68 trajectory.  The actual `0x80004005` HRESULT is
  sourced from RVA `0xe2bb` inside function `0xe0f4`, reached
  via the inner-inner call at `0xc92c → 0xc975`.  Round 69 also
  catches two transcription errors in the round-64 hand-off:
  `0xc933` is `mov [ebp+0x1c], eax` (round-64 doc missed this
  3-byte instruction), and the `jnz` is at `0xc936` not `0xc935`.
  Round-70 hand-off documented at
  `docs/codec/msadds32-receive-e-unexpected.md` §"Round 69".

## [0.1.1](https://github.com/OxideAV/oxideav-vfw/compare/v0.1.0...v0.1.1) - 2026-05-13

### Other

- vfw r67: discovery probe propagates the round-24 ICINFO_SIZE strict-codec gate; mpg4c32 identity card flows through
- vfw r66: MS-MPEG-4 v3 LUT-read trace corpus committed; workspace task #303 unblocked
- vfw r65: msadds32.ax JoinFilterGraph driven before Pause; round-64 candidate (1) FALSIFIED
- vfw r64: msadds32.ax IMemInputPin::Receive E_UNEXPECTED pinned to inner-decode-no-output guard at RVA 0x172f
- vfw r63: msadds32.ax Receive NULL+0x20 trap cleared via surgical helper_addref workaround
- vfw r62: msadds32.ax IMemInputPin::Receive NULL+0x20 trap clean-room forensics
- vfw r61: msadds32.ax input-pin IMemAllocator handshake fully lands S_OK; output-pin PCM ReceiveConnection probe surfaces a NULL-deref blocker for r62
- vfw r60: msadds32.ax ReceiveConnection lands S_OK for criteria-passing WMA1/WMA2 AMTs after clean-room disassembly of the CompleteConnect validator at RVA 0x2057
- vfw r59: ASF/WMA extractor lifts real WAVEFORMATEX + extradata from ffmpeg fixtures; msadds32.ax ReceiveConnection still E_FAIL but now with spec-grounded headers
- vfw r58: msadds32.ax audio splitter walks EnumPins + Pause/Run/GetState into FILTER_STATE_RUNNING; WAVEFORMATEX AMT staging lands; ReceiveConnection blocked on codec extradata blob
- vfw r57: msadds32.ax audio splitter spawns IBaseFilter through DllGetClassObject + CreateInstance — zero new ole32/oleaut32 stubs
- vfw r56: msvcrt!_CIpow real impl drains the final msadds32.ax PE-load blocker — audio splitter now fully PE-loaded
- vfw r55: msvcrt!{rand,srand} + seedable Sandbox PRNG API for reproducible encode
- vfw r54: AVI 1.0 muxer + ffmpeg cross-decode validates encoded MSMPEG4 v3 bytes end-to-end
- vfw r53: P-frame quality-regime probe — mpg4c32 clears keyframe flag but residual on 8-px translation exceeds I-frame
- vfw r52: msvcrt!_ftol real impl advances msadds32.ax PE-load past CRT FP-truncation edge
- vfw r51: encode side of IC* surface lands end-to-end against mpg4c32.dll
- vfw r50: msvcrt!_beginthreadex stub advances msadds32.ax PE-load past splitter CRT thread-creation edge
- vfw r49: msvcrt!_strnicmp stub advances msadds32.ax PE-load past splitter case-insensitive bounded-compare edge
- Round 48 — msvcrt!_endthreadex stub unblocks msadds32.ax PE-load further
- Round 47 — gdi32!StretchDIBits stub unblocks msadds32.ax PE-load further
- Round 46 — user32!{SetTimer, KillTimer} stubs unblock msadds32.ax further
- Round 45 — user32!MapDialogRect stub unblocks msadds32.ax PE-load
- Round 44 — entire MS-MPEG-4 v3 fixture corpus exercised end-to-end
- Round 43 — full 6-frame GOP decode at 352×288 (sample-release cycle closed)
- Round 42 — first multi-frame DShow decode (1→2 frames I+P end-to-end)
- Round 41 — IMemAllocator::GetBuffer arg-count fix unblocks MP43 decode
- Round 40 — register-snapshot watchpoints localise stack imbalance in Transform
- Round 39 — IID_IMediaSample2 QI support; Transform success-tail reached
- Round 38 — identify codec C++ class base; [filter_base+0x8c] proven non-NULL
- Round 37 — IPin::QueryPinInfo + ConnectedTo + IBaseFilter::QueryFilterInfo
- Round 36 — diagnose IMemInputPin::Receive NULL+0x1c trap site
- Round 35 — register host CLSID_MemoryAllocator class factory
- Round 34 — codec-allocator negotiation via IMemInputPin::GetAllocator
- Round 33 — real MP43 keyframe + IMediaFilter::GetState drive + IMemAllocator::SetProperties capture
- Round 32 — IMediaFilter::Run + HostIMemAllocator::Commit state-machine + IPin::QueryDirection filter
- Round 31 — IPin::EnumMediaTypes walk + downstream HostIPin::Receive capture
- Round 30 — DShow IMemAllocator+IMediaSample stubs + dim probe + Indeo/Cinepak trait tests
- Round 29 — wire oxideav_core::Decoder for VfW codecs end-to-end
- Round 28 — codec auto-discovery at register() time
- Round 27 — IFilterGraph + IPin host stubs land; ReceiveConnection S_OK
- Round 26 — user32!CreateWindowExA cascade + IPin::ReceiveConnection probe
- Round 25 — DirectShow IBaseFilter scaffolding (Stages 1-5 land)
- Round 24 — multi-frame MP43 + WMV verdict + ICGetInfo + UnregisterClassA
- Round 23 — MP43 ffmpeg-oracle PSNR + I+P 2-frame decode
- Round 22 — MSMPEG4 v3 ICDecompressBegin + first keyframe decode unblock
- Round 21 — x87 FPU executor + MSMPEG4 v3 DRV_OPEN unblock
- Round 20 — MMX kernels dispatch + MSMPEG4 v3 PE-load unblock
- extend codec status table with MSMPEG4 v3 + WMV1/2
- Round 19 — Lead A: trace-coverage analysis identifies EFLAGS.ID-bit gap

### Added

- Round 68 — **`AmtBlueprint::wma_with_ffmpeg_extradata_prefix`
  shifts `msadds32.ax`'s Receive HRESULT from `E_UNEXPECTED` to
  `E_FAIL` — the inner-decode-no-output guard at RVA `0x172f`
  (round-64 forensic surface) is BYPASSED.**  Round 60's
  `wma_criteria_passing` constructor populated the WAVEFORMATEX
  cbSize tail as `[4 or 10 zero bytes] ++ 37-byte magic CLSID`
  — enough to satisfy the `CompleteConnect` validator at RVA
  `0x2057` but the leading zero bytes are NOT realistic codec-
  private-data.  Round 68 adds a successor constructor that
  preserves the 37-byte CLSID suffix the validator demands but
  prefixes it with the empirical bytes ffmpeg emits in real
  fixtures (`00 00 01 00` for WMA1, `00 00 00 00 01 00 00 00 00
  00` for WMA2; per `tests/fixtures/audio/wma{1,2}_440hz_mono_1s.
  wma`).
  - **Empirical outcome** (per `tests/round68_msadds32_real_
    extradata.rs`, 5 phases):
    1. Phase 4 (baseline: zero preamble + round-63 patch)
       reproduces round 64's `0x8000FFFF` (`E_UNEXPECTED`).
    2. Phase 3 (ffmpeg preamble + round-63 patch) now returns
       `0x80004005` (`E_FAIL`) — the HRESULT shifted; the
       inner-decode-no-output bail at `0x172f` is no longer
       reached; the codec now bails earlier from the inner
       decode itself at RVA `0xc96a`.
    3. Phase 2 (ffmpeg preamble, NO round-63 patch) also yields
       `0x80004005` AND notably no longer traps at the
       `0x00000020` site — the round-63 patch may now be
       retirable; round 69 should confirm with an explicit
       retirement assertion.
  - **Architectural significance**: this is the first round
    since round 60 where the codec emits a different HRESULT
    on the same Receive entry, just by changing the
    WAVEFORMATEX-tail bytes — proving the inner decode reads
    codec-private-data at init time, and that round 64's
    structural blocker has been bypassed.
  - **Next blocker** identified: one of the 4 inner-decode arg-
    NULL guards at `0xc887` (offsets `0xc898 / 0xc8a3 / 0xc8ac
    / 0xc8b7`) OR the inner-inner-decode failure check at
    `0xc935`.  Round 69 should arm a watchpoint inside the
    inner decode and capture register state at entry to
    identify which guard fires.
  - Forensics doc at
    `docs/codec/msadds32-receive-e-unexpected.md` extended with
    a "Round 68" section enumerating the per-phase outcome,
    the interpretation, and the round-69 hand-off.
  - Source: new public API
    `oxideav_vfw::com::AmtBlueprint::wma_with_ffmpeg_
    extradata_prefix(format_tag, n_channels, n_samples_per_sec,
    n_avg_bytes_per_sec, n_block_align)` at
    `src/com/asf_amt.rs`.

- Round 67 — **`discovery::probe` now honours the round-24
  `ICINFO_SIZE = 568` strict-codec gate; `mpg4c32.dll` identity
  card flows through.**  Round 24 added the strict-size
  precondition to `win32::vfw32::ic_get_info` after pinning the
  rejection gate at `mpg4c32!DriverProc+0x999..0x99c`
  (`cmp [ebp+0x10], 0x238 / jb .return_zero`), but the
  auto-discovery probe still passed `cb = 112` (a value chosen
  for the Indeo family, which is lenient about short reads).
  Result: the discovery probe burned an `ICOpen → ICGetInfo`
  round-trip against `mpg4c32.dll` and threw away a silent
  0-byte response, never seeing the codec's identity card.
  Round 67 fixes the call site to pass `ICINFO_SIZE`, matching
  what real `vfw32!ICGetInfo` passes per MSDN.
  - **Recovered ICINFO record** (per
    `tests/round67_mpg4c32_icgetinfo.rs`):
    `dwSize = 0x238` (568), `fccType = 'vidc'`,
    `fccHandler = 'MP43'`, `dwFlags = 0x28`, `dwVersion = 1`,
    `dwVersionICM = 0x104`.
  - **Empirical finding on string fields**: mpg4c32 leaves
    `szName` / `szDescription` / `szDriver` ALL all-NUL inside
    the codec.  MSDN documents these as delegated to the
    registry HKEY `\Software\Microsoft\Windows NT\
    CurrentVersion\drivers32`; our sandbox has no registry, so
    the bytes stay zero.  The `ic_get_info` wrapper's existing
    round-17 fcc-derived fallback fills `szName = "MP43"` from
    the handler FourCC; `szDescription` and `szDriver` remain
    empty.
  - Regression-pinned at
    `tests/round67_mpg4c32_icgetinfo.rs` — 3 tests covering
    (1) `cb = 112` → 0-byte gate rejection, (2) `cb = 568` →
    full ICINFO with string-field decode, (3) discovery probe
    uses `ICINFO_SIZE` rather than a magic number.
  - Source fix: `src/discovery/probe.rs:122` now reads
    `let _ = sb.ic_get_info(hic, crate::win32::vfw32::ICINFO_SIZE);`.

- Round 66 — **MS-MPEG-4 v3 trace artifacts unblocking the
  msmpeg4 docs collaborator on workspace task #303.**  The
  msmpeg4 video crate has been blocked since round 7 on a docs
  gap: G0..G3 packed-Huffman tables + alternate-MV VLC tables.
  Round 66 walks `mpg4c32.dll`'s `.data` section against the
  Microsoft PE/COFF spec alone, identifies 13 candidate VLC
  / scan-permutation LUT regions covering ~32 KB of bytes,
  arms `Sandbox::watch` on every region, and drives the full
  10-fixture MS-MPEG-4 v3 corpus through `ic_decompress`.  Per-
  fixture JSONL traces of every LUT memory-read event are now
  committed at
  `docs/codec/msmpeg4-traces/<fixture>.jsonl[.gz]` for the
  docs collaborator to derive the table contents from.  The
  10 fixtures together drove 6/6 multi-frame fixtures cleanly
  through all 6+5+4+1+1+1+1+1+1+2 = 22 frames at ICERR_OK.
  - **Empirical finding** (per
    `docs/codec/msmpeg4-traces/README.md`): the MP43 decode
    hot loop reads ONLY from the small scan-permutation tables
    at RVAs `0x57860` / `0x58230` / `0x5844c`; the two big
    AC-coefficient LUTs at `0x4f938` (16 376 B) and `0x545c0`
    (12 288 B) are NEVER touched at decode time across any
    fixture.  Three hypotheses for the docs collaborator to
    falsify: (1) entropy-decode hot loop reconstructs symbols
    arithmetically from packed code-length bytes inline (so the
    G0..G3 tables live in `.text` at the disassembly-point
    EIPs `0x16e42` / `0x15f33` / `0x16ea8` / `0x16f2f`), (2)
    LUTs are memcpy'd to heap at codec-instance init time and
    the decode-time reads are heap-VA (round-67 would re-arm
    the watch on the heap arena), (3) the big tables are dead
    linker leftovers from a sibling encoder path.
  - Section map at
    `docs/codec/msmpeg4-mpg4c32-rdata-map.md` lists every
    candidate's RVA + size + confidence + first 16 decoded
    entries.
  - Trace generator binary at `examples/gen_msmpeg4_traces.rs`
    re-creates the artifacts from a fresh checkout
    (`cargo run --release --features trace --example
    gen_msmpeg4_traces`).
  - Regression-guard test at
    `tests/round66_msmpeg4_trace_corpus.rs` re-drives 5 of the
    multi-frame fixtures and asserts that ≥ 50 scan-region
    `mem_read` events plus ≥ 1 `win32_call` event still fire
    end-to-end, so a future change to the trace probe sites
    can't silently break LUT-region detection.

- Round 65 — **`msadds32.ax` `IBaseFilter::JoinFilterGraph` driven
  before `Pause`; round-64 candidate (3) FALSIFIED.**  Round 64
  pinned the `Receive` `E_UNEXPECTED` bail-out to the inner-
  decode-no-output guard at RVA `0x172f` and named three round-65
  candidates: (1) drive proper `JoinFilterGraph` / `Pause` /
  `IFilterGraph::Run` init so the codec populates `[esi+0xa4]` +
  `helper_struct[+0x20]`, (2) install codec-private-data in the
  `WAVEFORMATEX` tail, (3) strip ASF Payload Parsing Information
  framing.  Round 65 drives candidate (1) end-to-end against
  `msadds32.ax` using the host `IFilterGraph` stub already minted
  by [`Sandbox::mint_host_filter_graph`] (round 27).
  - **JoinFilterGraph returns `S_OK`** and **Pause returns `S_OK`**;
    the codec ACCEPTS the back-pointer at vtable slot 13.
  - **The codec NEVER calls back through the IFilterGraph
    back-pointer.**  Phase 5's trace-ring scan finds zero hits on
    every one of the 11 IFilterGraph thunk addresses across the
    full JoinFilterGraph + Pause window (176 instructions total,
    96 unique EIPs).  The codec stashes the pointer but performs
    no Pause-time graph queries.
  - **`helper_struct[+0x3c]` (= round-63 `[ecx+0x20]`
    "initialised" flag) stays `0x0`** after Pause completes
    (phase 1 introspection across `unk+0x90`, `filter+0x90`,
    `input_pin+0x90`, `mip+0x90`).  JoinFilterGraph does NOT
    drive the helper-struct setter.
  - **`Receive` without the round-63 patch STILL traps at
    `0x00000020`** (phase 2) — the round-63 workaround is NOT
    retirable through this path.
  - **`Receive` with patch + JoinFilterGraph returns the same
    `0x8000ffff`** as round 64's baseline (phase 3) — JoinFilterGraph
    does not unblock the inner-decode-no-output bail-out.
  - **ASF Payload-Parsing-Information strip experiment** (phase 4,
    candidate (3) from round-64 hand-off) also yields `0x8000ffff` —
    the failure isn't an input-framing mismatch at this layer.
  - **Conclusion**: round-64 candidate (1) "drive proper
    JoinFilterGraph" is FALSIFIED.  The codec's inner-decode-
    context initialisation is driven by something other than the
    filter-graph back-pointer.  Round-66 hand-off candidates
    (documented in `docs/codec/msadds32-receive-e-unexpected.md`):
    (a) disassemble the `helper_addref` SETTER's callers (RVA
    `0x5cf7..0x5d12`) to find the natural init path, and
    (b) snapshot registers at the inner-decode entry (RVA
    `0xc887`) to determine whether `[esi+0xa4]` itself is NULL
    or merely stale.
  - 6-test harness at `tests/round65_msadds32_join_filter_graph.rs`
    pins JoinFilterGraph S_OK + Pause S_OK (phases 1-3),
    workaround-retirement falsification (phase 6), IFilterGraph
    callback-count empirical sentinel (phase 5), and ASF strip
    forensics (phase 4).  Updated forensics writeup at
    `docs/codec/msadds32-receive-e-unexpected.md`.

- Round 64 — **`msadds32.ax` `IMemInputPin::Receive` E_UNEXPECTED
  forensics: bail-out pinned to the inner-decode-no-output guard
  at RVA `0x172f`.**  Round 63 cleared the NULL+0x20 trap via
  [`Sandbox::msadds32_patch_helper_addref`] and surfaced `eax =
  0x8000ffff` as the new failure surface.  Round 64 walks the
  trace ring forensically and proves the value isn't emitted by
  any of the 10 `mov eax, 0x8000FFFF` (`b8 ff ff 00 80`) sites
  visible in a linear `.text` scan — NONE of them is reached at
  all during the patched run.  Instead the codec executes
  `c7 45 08 ff ff 00 80` (`mov dword [ebp+0x08], 0x8000FFFF`) at
  RVA `0x172f`, which stamps `E_UNEXPECTED` into its caller's
  HRESULT out-slot before falling through to the cleanup tail at
  `0x1736..0x176c` that loads `eax = [ebp+0x08]` and returns.
  - **Failing check**: at RVA `0x165b` the codec branches
    `jnz +0xce → 0x172f` when `cmp [ebp-0x24], ebx` is non-zero.
    `[ebp-0x24]` is the "we already drained one input frame
    without producing output" flag, set to `1` at RVA `0x1661`
    after the first no-output inner-decode call.  On the SECOND
    consecutive no-output iteration of the outer loop (back-edge
    at `0x172a`), the codec bails with `E_UNEXPECTED`.
  - **Inner decode**: the call at RVA `0x1643` lands at RVA
    `0xc887` (a `__thiscall` taking 9 stack args; the 6th arg
    `[ebp+0x1c]` is the `&samples_produced` out-pointer).  It
    returns `eax = 0` to the outer loop yet leaves `*samples_produced
    = 0` — i.e. the codec accepted our input frame as well-formed
    but emitted zero PCM bytes.  The structural sentinels at RVAs
    `0x1658` (cmp), `0x165b` (jnz), `0x172a` (loop back), and
    `0x172f` (bail-out) are pinned by phase 5a so round 65 can
    replay without re-disassembling.
  - **IMediaSample-side ruled out**: phase 5 sweeps 6 combinations
    of `SetSyncPoint`, `SetMediaTime`, and `SetDiscontinuity`
    setters; all return the same `hr = 0x8000ffff` from the same
    trace pattern.  The failing check is NOT about the sample's
    presence-of-set bits.
  - **Round-65 candidates** (documented in
    `docs/codec/msadds32-receive-e-unexpected.md`):
    1. Drive the proper `JoinFilterGraph` / `Pause` /
       `IFilterGraph::Run` init path so the codec's own
       initialisation populates `[esi+0xa4]` (inner context) and
       `helper_struct[+0x20]` (retiring the round-63 patch).
    2. Install codec-private-data (extradata) in the
       `WAVEFORMATEX` tail of the `AmtBlueprint`.
    3. Strip ASF Payload Parsing framing from the input bytes
       before passing them to `Receive`.
  - 6-test harness at `tests/round64_msadds32_e_unexpected.rs`
    pins the candidate-RVA scan (`phase1`), live-site discovery
    (`phase2`), trace-tail disassembly (`phase3`), workaround
    regression guard (`phase4`), structural-sentinel pinning of
    the bail-out path (`phase5a`), and IMediaSample-setter panel
    (`phase5`).
  - Forensics writeup at
    `docs/codec/msadds32-receive-e-unexpected.md`.

- Round 63 — **`msadds32.ax` `IMemInputPin::Receive` NULL+0x20 trap
  resolved by clean-room workaround for the missing `helper_addref`
  initialisation.**  Round 62 traced the trap chain to
  `populator → buffer_pool_init → operator_new(0)`, where the
  zero size comes from `(h * 10) / size_calc` and `h` is what
  `helper_addref` (RVA `0x5cea`) returns on a fresh codec instance.
  Round 63 disassembles `helper_size_calc` (RVA `0x6ced..0x6d92`)
  and `helper_addref` end-to-end against Intel SDM Vol. 2 + the raw
  bytes of `msadds32.ax`:
  - **`helper_size_calc` formula**: `frame_samples = (kind == 0 ? 1 : 32) << shift`
    where `shift ∈ {9, 10, 11}` is a sample-rate / channel-count
    lookup against the tier boundaries
    `{8000, 11025, 16000, 22050, 32000, 44100, 48000}`, followed by
    a doubling loop that ensures `(frame_samples * wbps + sps/2) / sps ≥ 8`.
    For the round-62 WMA2 AMT (sps=44100, wbps=16, ch=1, kind=2)
    this returns `65536`.
  - **`helper_addref` body** (`0x5cea..0x5cf6`, 13 bytes):
    `cmp [ecx+0x20], 0; jz +4; mov eax, [ecx+0x28]; ret; xor eax,eax; ret`.
    A trivial getter — returns `helper_struct[+0x28]` if the
    `[+0x20]` "initialised" flag is set, else returns 0.  On a
    fresh codec instance both fields are zero, so `h = 0`,
    `(0 * 10) / 65536 = 0`, `operator_new(0)` returns NULL,
    `buffer_pool_init` fails, and Receive trips the LIFO-push trap.
    Real DirectShow hosts set the flag during
    `IFilterGraph::JoinFilterGraph` / `Pause`, which our scaffold
    doesn't yet wire.
  - **Surgical workaround**:
    [`Sandbox::msadds32_patch_helper_addref`] overwrites the first
    6 bytes of `helper_addref` with
    `b8 XX XX XX XX c3` (`mov eax, imm32; ret`) so `helper_addref`
    unconditionally returns the caller-supplied value.  Patching
    with any `value ≥ 6554` empirically clears the trap and lets
    Receive run to completion: HRESULT changes from a
    `memory fault at 0x00000020` to `0x8000ffff` (E_UNEXPECTED
    from the inner decode body), which is the round-64
    investigation surface.
  - **Test harness** at `tests/round63_msadds32_buffer_size_calc.rs`
    pins both the disassembly (`phase1*`), the formula
    (`phase2*`), the codec's helper-state at construction
    (`phase3_inspect_helper_state_before_receive`), and the
    cleared trap on patched runs (`phase4_patch_helper_addref_panel`
    + `phase5_regression_guard`).
  - Forensics writeup updated in
    `docs/codec/msadds32-receive-null-0x20.md` to document the
    resolved formula and the round-64 hand-off (drive the proper
    initialisation path so the workaround can be retired).
- Round 62 — **clean-room forensics on the `msadds32.ax`
  `IMemInputPin::Receive` NULL+0x20 trap that closed round 61.**
  Round 61's phase 5 surfaced a memory fault at `0x00000020`
  inside the codec's own decode path after the full input-pin
  allocator handshake AND output-pin `ReceiveConnection` had
  landed `S_OK`.  Round 62 captures the trap state precisely and
  reverse-engineers the failure mode from raw `msadds32.ax`
  byte inspection against Intel SDM Vol. 2 opcode tables (no
  Wine / ReactOS / Microsoft DShow base-class source consulted):
  - **Faulting instruction**: `89 72 20` =
    `mov [edx + 0x20], esi` at RVA `0x256a` (image_base
    `0x1c400000`, eip `0x1c40256a`).  `edx = 0x00000000`,
    `esi = 0x00000000` → store to `[0x20]` traps.
  - **Trap function**: RVA `0x2548..0x257f`, a list-prepend /
    LIFO-push helper on `this[0x160]`.  Reads
    `*[ebp+0x08]` (caller's out-slot) into `edx` with NO NULL
    check; that out-slot is the caller's local
    `[ebp_caller-0x04]`.
  - **Caller**: the input-pin `Receive` implementation at RVA
    `0x1501`.  The trap is on its **cleanup path** when the
    main decode body finished without producing a buffer
    (specifically: `[ebp-0x04]` left NULL by the populator,
    `[ebp-0x28]` left 0 because the insert-into-sorted-list
    branch was skipped).
  - **Populator function**: RVA `0x235e`, a buffer-pool POP
    helper that either pops from `this->lifo_head_160` or
    `malloc`+`init`s a new 40-byte node.  `init` (RVA `0x25ac`)
    chains into `operator new(edi_count)` where `edi_count` is
    `(addref_result * 10) / helper_size_calc`.  When that
    quotient rounds to 0 (which our run hits), `operator new(0)`
    returns NULL → `init` returns `E_OUTOFMEMORY` → populator
    returns failure → caller's `[ebp-0x04]` stays NULL → cleanup
    trap.
  - **IAT resolution**: `0x1c40f088 = ??2@YAPAXI@Z (operator new)`
    pinned via the thunk-jmp at codec RVA `0x6ae6` (called by
    the populator at `0x23d4`).  Adjacent IAT slots resolve to
    `??3@YAXPAX@Z (operator delete)`, `sprintf`, `_strnicmp`,
    `_purecall`, `_beginthreadex`, `_ftol`, `rand`, `_CIpow`
    — all from `msvcrt.dll`.
  - **`IPin::NewSegment` (slot 17) probe** under
    `R62_DRIVE_NEW_SEGMENT` env var traps immediately by
    dereferencing the rate-double's high dword `0x3FF00000` as
    a pointer.  Either the codec's IPin vtable doesn't have
    NewSegment at slot 17 OR the rate encoding our test uses
    doesn't match.  Documented for round 63.
  - 7-test forensics harness in
    `tests/round62_msadds32_null_0x20_forensics.rs` (phase 1
    register + EIP capture, phase 2 disassembly windows,
    phase 2b caller + populator + ctor + init dumps + IAT
    resolution, phase 2c function-entry walk, phase 2d visited-
    EIP existence checks, phase 3 regression guard against
    `VFW_E_NOT_COMMITTED`, phase 4 `NewSegment` probe).
  - Three new `IPin` vtable slot constants in
    `oxideav_vfw::com`: `SLOT_PIN_END_OF_STREAM` (14),
    `SLOT_PIN_BEGIN_FLUSH` (15), `SLOT_PIN_END_FLUSH` (16),
    `SLOT_PIN_NEW_SEGMENT` (17) — surfaced for round-63 use.
  - Full reverse-engineered analysis in
    `docs/codec/msadds32-receive-null-0x20.md` (pseudo-C
    transcription of both the trap function and the populator
    + identification of the three independent failure
    conditions that gate the trap).
  - Round-63 blocker: pin the runtime values of
    `edi_addref_result` (return of `helper_addref` at RVA
    `0x5ce8`) and `helper_size_calc` (return of helper at RVA
    `0x6ceb`) in our run to confirm the `(... * 10) / ...`
    quotient is rounding to 0, then either drive the missing
    `JoinFilterGraph`/`Pause`-time wiring that populates
    `this->helper_90` correctly, or pre-seed
    `this->lifo_head_160` with a host-minted node.

- Round 61 — **`msadds32.ax` input-pin `IMemAllocator` handshake
  lands `S_OK` on every step
  (`GetAllocator → SetProperties → Commit → NotifyAllocator`).**
  Round 60 closed by demonstrating
  `IMemInputPin::Receive(WMA2 bytes)` returns
  `VFW_E_NOT_COMMITTED` (`0x80040209`) because the codec's
  preferred allocator was still in the *decommitted* state.
  Round 61 replays the round-25..43 handshake the video path
  established for `mpg4ds32.ax`, now for the audio splitter:
  - `IMemInputPin::GetAllocator(&ppAllocator)` →
    `HRESULT 0x00000000`, `*ppAllocator = 0x60000650`
    (`msadds32` exposes its own preferred allocator).
  - `IMemInputPin::GetAllocatorRequirements(&props)` →
    `E_NOTIMPL (0x80004001)` (codec accepts whatever the
    upstream offers).
  - `IMemAllocator::SetProperties(cBuffers=4, cbBuffer=8192,
    cbAlign=1, cbPrefix=0)` on the codec's allocator →
    `HRESULT 0x00000000`, request mirrored verbatim into
    `pActual`.
  - `IMemAllocator::Commit()` on the codec's allocator →
    `HRESULT 0x00000000`.
  - `IMemInputPin::NotifyAllocator(alloc, FALSE)` →
    `HRESULT 0x00000000`.
  - Per-`HostState` `SetProperties` capture log
    (`oxideav_vfw::com::all_set_properties`) records 1 entry
    after the handshake — our own call, which the codec did
    not intercept or re-shape.
  - **Phase 5 BREAKTHROUGH** — empirically established that
    the codec's audio decode path requires its OUTPUT pin to
    be connected to a downstream `IMemInputPin` (analogous to
    the round-31 video path).  Driving
    `IPin::ReceiveConnection` on the codec's output pin with
    a PCM `WAVEFORMATEX` (mono 44.1 kHz 16-bit, no extradata)
    + the round-31 host downstream pair returns
    `HRESULT 0x00000000` — the codec accepts PCM as its
    downstream format.  After this connection, post-handshake
    `IMemInputPin::Receive` no longer surfaces
    `VFW_E_NOT_COMMITTED`; it now traps with a memory fault
    at `0x00000020` (page unmapped).  This is the round-62
    blocker: a NULL field dereference inside the codec's
    decode path, likely a missing import or a needed
    pre-`Receive` setup step (e.g., `IPin::EndOfStream` /
    `NewSegment` to seed the bitstream parser).
  - 5-test harness in
    `tests/round61_msadds32_allocator_handshake.rs` (phases 1
    discovery, 2 full handshake assertion, 3 receive
    observation, 4 SetProperties capture, 5 output-pin
    connect probe).  Every phase is replayable against the
    live splitter and records its empirical reaction on
    stderr for r62 baselining.
  - No new emulator scaffolding was needed — the
    `HostIMemAllocator` + Commit/Decommit state machine + 96-byte
    layout established for video in rounds 30–43 generalises
    cleanly to the audio path.  Clean-room methodology
    preserved: MSDN `IMemAllocator` / `IMemInputPin`
    documentation + COM IUnknown ABI only; no Wine /
    ReactOS / Microsoft DShow base-class source consulted.

- Round 60 — **`IPin::ReceiveConnection` now returns `S_OK` against
  the real `msadds32.ax` audio splitter for criteria-passing
  WMA1 and WMA2 `AM_MEDIA_TYPE`s.**  Round 59 closed by observing
  the splitter rejects every ffmpeg-encoded WMA1 / WMA2 fixture
  with `HRESULT 0x80004005` (`E_FAIL`).  Round 60 disassembled
  the input-pin validator chain end-to-end from raw byte
  inspection of `msadds32.ax` against Intel SDM Vol. 2 opcode
  tables (no Wine / ReactOS / Microsoft DShow / ffmpeg source
  consulted) and pinned the rejection to a single gate inside
  `CompleteConnect` (inner.vtable[12] at RVA `0x2057`):
  - `IPin::ReceiveConnection` (vtable[4] @ RVA `0x476f`) walks
    four pre-gates (`pmt`/`pConnector` NULL → `E_POINTER`;
    `m_pConnected` non-NULL → `VFW_E_ALREADY_CONNECTED`; not
    `State_Stopped` → `VFW_E_NOT_STOPPED`; same-direction pin →
    `VFW_E_INVALID_DIRECTION`).
  - Calls `inner.CheckConnect(pConnector)` (vtable[10] @ RVA
    `0x5623` → helper at RVA `0x4743` which only verifies
    opposite-direction connectivity via
    `pConnector->QueryDirection()`).
  - Calls `inner.CheckMediaType(pmt)` (vtable[8] @ RVA `0x568a`)
    — a near-no-op that returns `S_OK` after delegating to a
    stub on the BaseFilter at RVA `0x4a19`
    (`xor eax, eax; ret 8`).
  - Calls `inner.SetMediaType(pmt)` then
    `inner.CompleteConnect(pConnector)` (vtable[12] @ RVA
    `0x2057`).  This is where the real validation runs: the
    callee re-fetches the AMT via
    `pConnector->ConnectionMediaType(&amt)`, inspects
    `pbFormat`'s `wFormatTag`, and runs a `memcmp` of a fixed
    37-byte ASCII CLSID string against `extradata[4..41]` (WMA1)
    or `extradata[10..47]` (WMA2).
  - The 37-byte magic string lives at `.rdata` RVA `0x11138`
    and decodes as `"1A0F78F0-EC8A-11d2-BBBE-006008320064\0"` —
    the Microsoft Windows Media Audio Decoder's own component
    CLSID.  The splitter requires this exact string to be
    embedded in every accepted stream's `WAVEFORMATEX`
    extradata.  ffmpeg's WMA encoders have no reason to emit
    it, which is why every round-58 / round-59 fixture failed.
  - `wFormatTag` constraint: `0x0160` (WMA1) or `0x0161` (WMA2);
    any other tag returns `E_UNEXPECTED` (`0x8000FFFF`).
  - `cbSize` constraint: `>= 0x29` (41) for WMA1 or `>= 0x2F`
    (47) for WMA2.  Falling below either threshold returns the
    raw `E_FAIL` round 59 observed (the
    `E_FAIL→VFW_E_TYPE_NOT_ACCEPTED` remap in
    `ReceiveConnection` is bypassed because `CompleteConnect`'s
    failure path jumps directly to the function exit, skipping
    the remap block).
  - New `oxideav_vfw::com::AmtBlueprint::wma_criteria_passing`
    constructor builds a criteria-satisfying AMT in one call:
    populates the `WAVEFORMATEX` with caller-supplied codec
    parameters, sets `cbSize` to the exact minimum threshold
    for the requested format tag (41 for WMA1, 47 for WMA2),
    and appends the 37-byte magic CLSID at the correct
    in-extradata offset.  The `phase4_criteria_passing_*` tests
    verify both WMA1 (`0x0160`, 41-byte extradata) and WMA2
    (`0x0161`, 47-byte extradata) variants land `HRESULT
    0x00000000` (S_OK) on the live splitter.
  - **Phase 5 stretch** — after criteria-passing
    `ReceiveConnection` lands `S_OK`, pushing 4 KiB of the WMA2
    fixture's first ASF data packet through
    `IMemInputPin::Receive` (with `IMediaFilter::Pause + Run(0)`
    in between) returns `HRESULT 0x80040209`
    (`VFW_E_NOT_COMMITTED`).  The codec's internal
    `IMemAllocator` has not been committed via the
    `GetAllocator → SetProperties → Commit → NotifyAllocator`
    handshake; that is round 61's anchor task.  No PCM bytes
    surface on the host sink in round 60.
  - 16-test disassembly + validator-passing harness:
    `tests/round60_msadds32_query_accept_disasm.rs` (phases 1,
    1b, 2, 2b–2i, 3, 4 ×3, 5).  Every phase is replayable
    against the live splitter and records its empirical
    reaction on stderr for r61 baselining.
  - Clean-room documentation: new
    `docs/codec/msadds32-query-accept-validation.md` captures
    the full validator decoding (RVAs, opcode trace,
    magic-string extraction, recipe for a passing AMT).
  - New public constant `oxideav_vfw::com::SLOT_PIN_QUERY_ACCEPT
    = 11` (the vtable slot the round-60 reverse-engineering
    pass initially targeted before tracing the rejection to
    `CompleteConnect`).

- Round 59 — **real `WAVEFORMATEX` + extradata lifted from a 1-s
  440 Hz ffmpeg-generated ASF/WMA fixture; `IPin::ReceiveConnection`
  still returns `E_FAIL` against `msadds32.ax`'s input pin but
  with empirically-grounded codec headers rather than synthetic
  zeros, surfacing the next splitter blocker.**  New
  `oxideav_vfw::com::asf_amt` module walks the ASF Header Object
  (`{75B22630-668E-11CF-A6D9-00AA0062CE6C}`, ASF spec §11.1),
  locates the Stream Properties Object
  (`{B7DC0791-A9B7-11CF-8EE6-00C00C205365}`, §3.3) whose Stream
  Type GUID equals `ASF_Audio_Media`
  (`{F8699E40-5B4D-11CF-A8FD-00805F5C442B}`), and decodes the
  Type-Specific Data field — for an audio stream this IS the
  `WAVEFORMATEX` struct followed by `cbSize` bytes of
  codec-specific extradata — into the new
  `oxideav_vfw::com::AmtBlueprint`.  Test fixtures
  `tests/fixtures/audio/wma1_440hz_mono_1s.wma` (6904 B,
  `wFormatTag=0x0160` / cbSize=4 / extradata=`00 00 01 00`) and
  `tests/fixtures/audio/wma2_440hz_mono_1s.wma` (6944 B,
  `wFormatTag=0x0161` / cbSize=10 /
  extradata=`00 00 00 00 01 00 00 00 00 00`) are checked in;
  ffmpeg 8.1 invocation command documented in
  `tests/fixtures/audio/HOWTO.md`.  Both fixtures encode at
  44 100 Hz mono / 32 kbit/s / `nBlockAlign=185` /
  `wBitsPerSample=16`.
  - **Phase 1 result — ASF-spec parser surfaces canonical WMA1 /
    WMA2 headers.**  `extract_wma_amt_from_asf` returns the
    correct `wFormatTag` (`0x0160` / `0x0161`), `nChannels`,
    `nSamplesPerSec`, `nAvgBytesPerSec`, `nBlockAlign`,
    `wBitsPerSample`, and the full `cbSize`-bytes extradata
    blob for each fixture.  The blueprint round-trips into a
    guest-staged AM_MEDIA_TYPE (Phase 2 test).
  - **Phase 3 result — `IPin::ReceiveConnection` still returns
    `E_FAIL` (`0x80004005`) for both WMA1 and WMA2 even with
    REAL fixture extradata.**  Splitter's `QueryAccept` is
    validating against something more specific than the
    standard ffmpeg-emitted bootstrap header — likely the
    encoder-class byte / bitstream-version byte that the
    Microsoft WMA encoder embeds.  Next blocker is to either
    (a) probe the splitter's QueryAccept disassembly at the
    AMT-check site to identify the exact byte(s) it validates,
    or (b) source a Microsoft-encoded WMA fixture rather than
    ffmpeg's wmav1/wmav2 output.  Phase 4 (push first ASF data
    packet through `IMemInputPin::Receive`) is gated on
    Phase 3 acceptance and skips cleanly when E_FAIL is
    returned.
  - **`com::asf_amt` is clean-room from the public ASF
    specification only — no Wine / ReactOS / MinGW / Microsoft
    DShow / ffmpeg WMA source consulted.**  ffmpeg is used as
    an opaque byte-stream generator (it writes the bytes we
    read); we do not read any line of its source.  The parser
    refuses non-ASF inputs (`AsfParseError::NotAnAsfFile`),
    truncated buffers (`TruncatedHeader`), inconsistent
    sub-object sizes (`InvalidSubObjectSize`,
    `SubObjectOverflowsHeader`), audio-stream-less files
    (`NoAudioStream`), and malformed `WAVEFORMATEX::cbSize`
    relative to Type-Specific Data Length
    (`WaveFormatExtraOverflow`).

- Round 58 — **`msadds32.ax` audio splitter walks `EnumPins` +
  `Pause` + `Run(0)` cleanly into `FILTER_STATE_RUNNING`; full
  encoded-audio AMT staging surface (WAVEFORMATEX) lands; only
  the codec-specific extradata blob remains as the r59
  blocker.**  Round 57 closed by demonstrating the splitter spawns
  out-of-the-box (DllGetClassObject + CoCreateInstance + QI for
  every documented base interface).  Round 58 takes the next step
  on the audio decode path: discover what media-type families
  the splitter advertises, build a `WAVEFORMATEX`-shaped
  `AM_MEDIA_TYPE`, and drive the splitter through
  `ReceiveConnection + Pause + Run(0)`.
  - **Phase 1 result — splitter exposes 2 pins.**
    `IBaseFilter::EnumPins → IEnumPins::Next` discovers a
    matched pair: INPUT pin at `0x6000_027c` (encoded-audio
    receive side), OUTPUT pin at `0x6000_038c` (PCM emit side).
    `IPin::QueryDirection` confirms `PIN_INPUT (0)` and
    `PIN_OUTPUT (1)` respectively.  `IPin::EnumMediaTypes` on
    the input pin returns ZERO offered AMTs — the splitter
    negotiates purely through `IPin::QueryAccept` rather than
    pre-enumerating.  The supported subtype pair was instead
    extracted by reading the splitter's `.rdata` AMT
    registration table at RVA `0xf268..0xf288` (clean-room
    inspection of raw bytes): TWO consecutive
    `MEDIASUBTYPE` GUIDs in audio fourcc-base family —
    `{00000160-0000-0010-8000-00AA00389B71}` (`WMAUDIO1`,
    `wFormatTag=0x0160`) and
    `{00000161-0000-0010-8000-00AA00389B71}` (`WMAUDIO2`,
    `wFormatTag=0x0161`).  Output pin's PCM subtype is
    `{00000001-0000-0010-8000-00AA00389B71}` (`WAVE_FORMAT_PCM`).
  - **Phase 2 result — full `WAVEFORMATEX` AMT staging
    surface.**  `stage_audio_am_media_type` lays out a 72-byte
    `AM_MEDIA_TYPE` at one arena address and an 18+extradata
    `WAVEFORMATEX` immediately after, with all the canonical
    field offsets (`majortype=MEDIATYPE_Audio` @ +0;
    `subtype=MEDIASUBTYPE_<format_tag>` @ +16;
    `bTemporalCompression=1` @ +36;
    `formattype=FORMAT_WaveFormatEx` @ +44; `cbFormat` @ +64;
    `pbFormat` @ +68; `wFormatTag` @ +0; `nChannels` @ +2;
    `nSamplesPerSec` @ +4; `nAvgBytesPerSec` @ +8;
    `nBlockAlign` @ +12; `wBitsPerSample` @ +14; `cbSize` @ +16;
    extradata @ +18).  Every field round-trips through guest
    memory.
  - **Phase 3 result — `IPin::ReceiveConnection` reachable but
    rejects synth AMT.**  Both synthesized `MSAUDIO1` and
    `WMAUDIO2` AMTs (carrying 2-channel / 44_100 Hz / 16-bit /
    10-byte zero extradata) are rejected with HRESULT
    `E_FAIL (0x80004005)`.  No new ole32 / msacm32 / msvcrt
    stubs surfaced — the splitter's rejection happens entirely
    in its own validation path, not in any unresolved import.
    The likely r59 blocker is the `cbSize` extradata blob:
    `MSAUDIO1` typically expects a 4-or-10-byte codec-specific
    initialisation header (sample-rate-class index, denoise
    table seeds, etc.) that the codec uses to bootstrap its
    internal decoder state.  Real WMA1-encoded fixtures pin
    those bytes; a synthetic zero block fails the splitter's
    `AVI/ASF`-header replay check.
  - **Phase 4 result — full state machine works.**
    `IMediaFilter::Pause()` → `S_OK`;
    `IMediaFilter::Run(0)` → `S_OK`;
    `IMediaFilter::GetState(1000ms)` → `S_OK` with
    `FILTER_STATE=2` (`FILTER_STATE_RUNNING`).  The splitter's
    state machine is fully functional regardless of whether
    ReceiveConnection has succeeded.
  - **Phase 5 result — push-sample path blocked by Phase 3.**
    `IMemInputPin::Receive` is reachable (`QueryInterface(input_pin,
    IID_IMEMINPUTPIN)` succeeds), but no encoded sample can be
    pushed until ReceiveConnection accepts an AMT.  Smoke test
    skips gracefully.
  - **Zero new host stubs needed.**  The audio splitter's
    EnumPins / EnumMediaTypes / ReceiveConnection / Pause / Run
    / GetState surface is fully satisfied by the round-25..40
    DirectShow scaffolding the video path already established.
    No new ole32, msacm32, or msvcrt entries were drained — the
    splitter's decoding stays self-contained (does NOT delegate
    to `msacm32!acmStream*` as some speculation suggested).
  - `tests/round58_msadds32_audio_amt_walk_and_connect.rs` —
    six integration tests across the four phases.  `phase1` +
    `phase3` + `phase4` + `phase5` are gracefully-skipping on
    fixture absence; `phase2` is unconditional unit-tests of
    the AMT staging helper + the `mmreg`-compatible audio
    subtype constructor.
  - **Next critical-path target (round 59):**  the `MSAUDIO1`
    AMT extradata blob.  Two approaches: (a) capture a real
    WMA1-encoded `.asf` fixture and replay the splitter's
    AMT-build sequence against it to extract the byte pattern;
    (b) reverse-engineer the splitter's `IPin::QueryAccept`
    validation path (presumably a bytewise compare against an
    expected header signature in `.rdata`) and derive the
    minimum acceptable extradata content from the spec.

- Round 57 — **`msadds32.ax` audio splitter spawns through
  `DllGetClassObject` + `IClassFactory::CreateInstance` —
  IUnknown/IPersist/IMediaFilter/IBaseFilter all QI cleanly with
  ZERO new ole32/oleaut32 stubs.**  Round 56 closed by FULLY
  PE-loading `msadds32.ax`; round 57 drives the audio splitter
  through the same DirectShow co-create scaffolding the round-25
  video splitter already exercised, and discovers the existing
  COM / DirectShow surface is RICH ENOUGH to spawn an IBaseFilter
  instance from the audio decoder out-of-the-box.  No new ole32
  (`CoTaskMemAlloc` / `CoCreateInstance` / `StringFromGUID2`) or
  oleaut32 (`SysAllocString` / `VariantInit`) stubs were needed —
  the audio splitter shares the `CBaseFilter` / `CTransformFilter`
  scaffolding shape with the video splitter rounds 25-44 already
  drove through `IBaseFilter::Run` + `IPin::ReceiveConnection`.
  - **Audio decoder CLSID discovered** —
    `MSADDS_AUDIO_DECODER_CLSID = {22E24591-49D0-11D2-BB50-006008320064}`
    (Data4 suffix `006008320064` is in the Windows Media Audio
    family).  Reverse-engineered from `msadds32.ax`'s
    `DllGetClassObject` prologue at RVA `0x3635`: prologue
    contains a `repe cmpsd` loop walking a 20-byte-stride CLSID
    table at RVA `0x11000` (count word at RVA `0x11028`, value
    = 2).  Entry 0's CLSID pointer (RVA `0xf248`) decodes to the
    audio decoder CLSID; entry 1's (RVA `0xf298`) decodes to
    `MSADDS_AUDIO_PROPERTY_PAGE_CLSID =
    {8FE7E181-BB96-11D2-A1CB-00609778EA66}` — the audio
    decompressor control property page (UI vestige, not on the
    decode path).  Clean-room: disassembled from raw opcode bytes
    against Intel SDM Vol. 2A; no Wine / ReactOS / MinGW
    consulted.  The CLSIDs themselves are public installation
    metadata that the splitter's `DllRegisterServer` writes to
    `HKCR\CLSID\{...}\InprocServer32`.
  - `src/com/mod.rs` — new public `pub const
    MSADDS_AUDIO_DECODER_CLSID: Guid` + `pub const
    MSADDS_AUDIO_PROPERTY_PAGE_CLSID: Guid`.  Both are pinned
    against their canonical braced-string forms by unit tests
    `msadds_audio_decoder_clsid_round_trips` and
    `msadds_audio_property_page_clsid_round_trips`.
  - `src/lib.rs` — re-exports the two new CLSID constants
    alongside the existing IID family so consumer crates can
    name them without reaching into `com::`.
  - `src/discovery/probe.rs` — `DSHOW_CLSID_CANDIDATES` grows
    from 1 to 2: the audio decoder CLSID lands as a probe
    candidate so `oxideav_vfw::discovery` lights up `msadds32.ax`
    as `Kind::DirectShow` on auto-discovery.  New companion test
    `guid_from_le_bytes_matches_msadds_audio_clsid` pins the
    little-endian wire form against the high-level `Guid`
    constant.
  - `tests/round57_msadds32_dll_get_class_object.rs` — 7
    integration tests across 4 phases:
    1. **Phase 1** — pins both CLSID constants round-trip
       through their MIDL braced strings.
    2. **Phase 2** — drives
       `Sandbox::dll_get_class_object(img,
       MSADDS_AUDIO_DECODER_CLSID, IID_IClassFactory)` →
       `Ok(factory)` at `0x6000_0060` with plausible vtable.
       Companion: same path for `MSADDS_AUDIO_PROPERTY_PAGE_CLSID`
       also succeeds (informational stretch); and a bogus
       random CLSID surfaces `CLASS_E_CLASSNOTAVAILABLE` cleanly
       rather than crashing.
    3. **Phase 3** — drives
       `Sandbox::co_create_instance(MSADDS_AUDIO_DECODER_CLSID,
       IID_IUNKNOWN)` → `Ok(0x6000_0090)`.  The audio splitter's
       internal CBaseFilter constructor runs to completion
       without surfacing any unresolved imports.
    4. **Phase 4 (stretch)** — `QueryInterface` for IUnknown /
       IPersist / IMediaFilter / IBaseFilter ALL return non-NULL
       interface pointers with plausible vtables; the splitter
       satisfies the IBaseFilter contract out of the box.
  - **Next critical-path target (round 58):**  drive the audio
    splitter through `IBaseFilter::Run` + `IPin::ReceiveConnection`
    against an audio AMT.  The shape will diverge from the
    video path because audio uses different `MEDIASUBTYPE_*`
    GUIDs (`MEDIASUBTYPE_MSAUDIO1` / `MEDIASUBTYPE_WMAUDIO2` —
    discovered identically by walking the splitter's output-pin
    `EnumMediaTypes` enumeration), `WAVEFORMATEX` instead of
    `VIDEOINFOHEADER` for the format block, and PCM/S16 output
    buffer shape rather than YV12.  Round 58 will surface a new
    set of host stubs gated on the audio-format-block layout
    (likely `msacm32!acmStream*` if the splitter delegates
    bitstream-to-PCM through the ACM, or first-class PCM output
    if the splitter is fully self-contained).

- Round 56 — **`msvcrt!_CIpow` real impl drains the final
  `msadds32.ax` PE-load blocker — the audio splitter is now FULLY
  PE-loaded.**  Round 55 pinned the next blocker as
  `msvcrt!_CIpow` — MSVC's compiler-intrinsic
  `pow(double, double)` helper.  Like `_ftol` (r52), the `_CI*`
  family passes args on the **x87 stack** (not the cdecl integer
  stack) and returns the result on the x87 stack as the new
  `ST(0)`: `arg_dwords = 0`.
  - `src/win32/msvcrt.rs` — new `stub_ci_pow`.
    `double __cdecl _CIpow(double base, double exp)` pops `exp`
    (top of stack) then `base` (was `ST(1)`) off the x87 stack,
    computes `base.powf(exp)` via Rust's `f64::powf` (bit-correct
    by construction per IEEE 754), and pushes the result back
    onto the x87 stack as the new `ST(0)`.  Returns 0 in `eax`
    per the documented `_CI*` convention (the result lives in
    `ST(0)`, not `eax`).  Clean-room references: MSDN `pow`
    function page; Intel SDM Vol. 1 §8 + Vol. 2A "FLD" / "FSTP"
    for x87 stack semantics; IEEE 754-2008 for corner cases.  No
    Wine / ReactOS / MinGW / Microsoft CRT source consulted.
  - `tests/round56_msvcrt_cipow.rs` — 13 integration tests
    pinning: stub registered; canonical `2 ** 10 = 1024`;
    fractional exponent matches `sqrt(2)` within 1e-10; negative
    base with integer exp returns real `9.0`; negative base with
    non-integer exp returns NaN; zero base with positive exp
    returns 0.0; IEEE 754 `0.0 ** 0.0 = 1.0`; NaN propagation;
    `NaN ** 0.0 = 1.0` (IEEE 754 powf-of-NaN exception);
    `∞ ** 0.0 = 1.0`; `1.0 ** NaN = 1.0` (the other powf-of-NaN
    exception); x87 stack invariant (2 in, 1 out, net -1 depth);
    and the round-56 headline asserting `Sandbox::load
    ("msadds32.ax")` advances past `_CIpow`.
  - `tests/round56_msadds32_pe_load_complete.rs` — milestone
    reproducibility check.  After r56 the audio splitter PE-load
    surface has **every named import resolved**: this test
    asserts `Sandbox::load("msadds32.ax")` returns `Ok(_)`
    cleanly with `DllGetClassObject` exported.  Image base
    `0x1c40_0000`, entry point `0x1c40_233d`.  Pins the milestone
    so any regression that re-introduces an unresolved-import
    blocker on this codec surfaces loudly.
  - `README.md` — round counter bumped to 56; status block
    re-written to announce the PE-load milestone.
  - **Next critical-path target:** drive `msadds32.ax` through
    `DllGetClassObject` to instantiate the DirectShow filter
    factory, then exercise `DriverProc(DRV_LOAD)` /
    `IPin::ReceiveConnection` to start an actual audio decode.
    Those will surface a new round of stubs in the COM /
    DirectShow surface (`ole32!CoTaskMemAlloc`,
    `oleaut32!SysAllocString`, audio pin negotiation, …) — NOT
    more `msvcrt!_CI*` math helpers.

- Round 55 — **`msvcrt!{rand, srand}` real impl + seedable
  `Sandbox` PRNG API for reproducible encode output.**  Round 52
  pinned the next `msadds32.ax` PE-load blocker as `msvcrt!rand`;
  round 55 wires both `rand` and the seed companion `srand` AND
  exposes a host-side seedable PRNG API on `Sandbox` so callers
  can drive the codec with a deterministic LCG sequence — a
  one-time architectural addition that protects encode output
  reproducibility against any future codec call that consults
  `rand`.
  - `src/win32/msvcrt.rs` — new `stub_rand` and `stub_srand`.
    `int __cdecl rand(void)` implements MSVC's documented Knuth-
    style linear-congruential generator:
    `state = state * 214013 + 2531011 (mod 2^32)`,
    `rand = (state >> 16) & 0x7FFF` (RAND_MAX = 0x7FFF).  The
    multiplier (214013), increment (2531011), and output-bit
    mask are public number-theory constants from many LCG
    references (Knuth Vol. 2, Numerical Recipes table, …); no
    Microsoft CRT source was consulted.  `void __cdecl
    srand(unsigned int seed)` stores `seed` directly into the
    same state field (no XOR / no scrambling — the documented
    convention).  Both stubs are cdecl, `arg_dwords = 0`.
    Reference: MSDN `rand` / `srand` topic pages
    (`learn.microsoft.com/en-us/cpp/c-runtime-library/reference/rand`).
  - `src/win32/mod.rs` — `HostState` grows a `rand_state: u32`
    field, default `1` (MSVC's documented "no `srand` called
    yet" initial state).  Both `stub_rand` / `stub_srand` and
    the host-side `Sandbox` API read / write this single field,
    so host-staged seeds and guest `srand` calls flow through
    the same state.
  - `src/runtime.rs` — three new public methods on `Sandbox`
    (the user-requested architectural addition):
    `with_rand_seed(seed) -> Self` (builder),
    `set_rand_seed(&mut self, seed)` (runtime setter),
    `rand_seed(&self) -> u32` (reader).  Documented contract:
    two sandboxes seeded identically produce identical `rand`
    sequences, which makes encode regression tests
    deterministic across runs.
  - `tests/round55_msvcrt_rand_seedable.rs` — 12 integration
    tests pinning: both stubs registered; default-seed
    reproducibility; same-seed reproducibility; different-seed
    divergence; output bounded by RAND_MAX; 1000-sample bucket
    coverage (rules out a degenerate / short-period LCG);
    guest `srand` overrides host seed; host seed and guest
    `srand` to the same value produce identical sequences;
    known-vector LCG model match (first 16 outputs); `rand_seed`
    read-back tracks the post-call state; `set_rand_seed`
    mid-flight resets the sequence; round-55 headline
    (`Sandbox::load("msadds32.ax")` advances past `rand`).
  - `tests/round55_encode_determinism.rs` — end-to-end
    validation of the seedable-Sandbox-API contract.  Builds
    two sandboxes both `with_rand_seed(42)`, drives the
    round-51 MP43 encode path with the same 176×144 BGR24
    input, asserts encoded byte streams are **byte-for-byte
    identical** (architectural contract verified).  Then
    re-encodes at seed 43 and reports whether outputs differ:
    **finding — mpg4c32 encode output is IDENTICAL at seed 42
    vs seed 43 over the same input**.  The codec's VfW encode
    path does not consult `msvcrt!rand`, so the architectural
    addition is protection-only on this codec: it pins
    reproducibility today (vacuously) and pre-empts any future
    code path that introduces randomness.
  - `README.md` — new "Reproducible encode" section showing the
    builder + runtime-setter API surface and documenting the
    `mpg4c32` empirical finding.
  - **Next msadds32.ax PE-load blocker:** `msvcrt!_CIpow` (the
    MSVC x87 helper for `pow(double, double)` with both args
    on the x87 stack — same calling convention quirk as `_ftol`).

- Round 54 — **AVI 1.0 muxer for vfw-encoded MSMPEG4 v3 output +
  `ffmpeg` cross-decode validation.**  Round 51 produced raw
  MSMPEG4 v3 elementary bytes that self-roundtrip at 27.83 dB
  PSNR-BGR24 through the same `mpg4c32.dll` decode path.  Round
  54 validates the bytes through a SECOND independent decoder:
  wrap them in a minimal AVI 1.0 RIFF container, invoke `ffmpeg`
  to decode the AVI back to raw BGR24, and compare to the
  original input.
  - `tests/round54_avi_wrap_ffmpeg_decode.rs` — inline AVI muxer
    built with raw byte construction (no `oxideav-avi` dev-dep —
    cross-crate dev-deps trap consumer crates in producer-release
    lockstep, per the project memory).  Builds the standard AVI
    1.0 layout: `RIFF AVI ` outer chunk → `LIST hdrl` containing
    `avih` (MainAVIHeader, 56 bytes) + `LIST strl` containing
    `strh` (AVIStreamHeader, 56 bytes; fccType='vids',
    fccHandler='MP43', dwRate=25, dwScale=1) + `strf`
    (BITMAPINFOHEADER, 40 bytes; biCompression='MP43',
    biWidth=176, biHeight=144, biBitCount=24).  Then `LIST movi`
    containing N × `00dc` chunks (one per encoded frame,
    word-aligned with optional pad byte for odd-length payloads),
    then `idx1` chunk with one 16-byte AVIINDEXENTRY per frame
    (ckid='00dc', dwFlags=AVIIF_KEYFRAME=0x10,
    dwChunkOffset relative to start of 'movi' LIST payload,
    dwChunkLength).
  - **Findings (all green):**
    - `ffprobe -of json -show_format -show_streams` ACCEPTS the
      AVI (rc=0, structural validation passes).
    - `ffmpeg -i <avi> -f rawvideo -pix_fmt bgr24 -frames:v 5`
      decodes ALL 5 frames cleanly (rc=0, exactly 380160 bytes =
      5 × 176 × 144 × 3).
    - `mpv --vo=null --ao=null --frames=5` decode probe rc=0
      (mpv accepts the AVI).
    - Mean PSNR-BGR24 across 5 frames = **20.86 dB** comparing
      ffmpeg's BGR24 output (vertically flipped to BMP
      bottom-up convention) to our original BGR24 input.  At
      `quality=5000` this is consistent with the codec's
      documented lossy regime; the headline is that ffmpeg
      successfully decoded our codec's bytes end-to-end.
  - Fail-soft envelope: if `ffmpeg`/`ffprobe`/`mpv` are absent
    from PATH, the test reports the skip with `println!` and
    returns OK (NOT `#[ignore]` — the test runs unconditionally
    and surfaces the tool absence as a discovery).
  - Reference: Microsoft AVI RIFF File Reference
    (`learn.microsoft.com/en-us/windows/win32/directshow/avi-riff-file-reference`)
    + `winsdk-10/Include/.../um/Aviriff.h` (`MainAVIHeader`,
    `AVIStreamHeader`, `AVIINDEXENTRY`, `AVIIF_*`) +
    `winsdk-10/Include/.../um/Vfw.h` (`BITMAPINFOHEADER`,
    `streamtypeVIDEO = 'vids'`).  No external muxer / demuxer
    library code consulted.

- Round 53 — **P-frame quality-regime probe; mpg4c32 clears the
  keyframe flag for non-keyframe requests, but the residual on an
  8-pixel horizontal translation is LARGER than the I-frame across
  the probed quality range (P/I ratio = 1.386 at all five quality
  levels).**  Round 51 found that at `quality=5000` the codec
  emits keyframes for both I and P-tagged frames when content is
  *identical* (frame 0 == frame 1).  Round 53 probes whether
  truly differing content (frame 1 = frame 0 shifted right by 8
  pixels) + a sweep of quality settings `{1000, 2000, 3000, 5000,
  8000}` changes that.
  - `tests/round53_pframe_quality_probe.rs` — for each quality
    level open a fresh encoder HIC, encode frame 0 with
    `ICCOMPRESS_KEYFRAME`, encode frame 1 with `flags = 0` and
    `prev_bih` / `prev_bytes` pointing at frame 0's input bytes,
    then record `(I-size, P-size, returned_flags &
    ICCOMPRESS_KEYFRAME, P/I ratio)`.
  - **Finding:** the codec DOES clear the keyframe flag for every
    P-frame request in the range (so it acknowledges the
    non-keyframe request) — but the actual P-frame bytes are
    LARGER than the corresponding I-frame (P = 1344 bytes vs
    I = 970 bytes), invariant across all five quality settings.
    Per-quality breakdown:
    `q=1000` I=970 P=1344 P/I=1.386 codec_cleared_keyframe=true;
    `q=2000` I=970 P=1344 P/I=1.386 codec_cleared_keyframe=true;
    `q=3000` I=970 P=1344 P/I=1.386 codec_cleared_keyframe=true;
    `q=5000` I=970 P=1344 P/I=1.386 codec_cleared_keyframe=true;
    `q=8000` I=970 P=1344 P/I=1.386 codec_cleared_keyframe=true.
    The codec's motion compensation under the bare VfW path does
    not shrink the residual below the I-frame size on an 8-pixel
    horizontal translation at any quality regime we probed; the
    motion residual + new-content cost together exceed the
    intra-only I-frame cost.  This is the round's reportable
    finding — real P-frame *compression* (P < I) may require
    either richer motion estimation (DirectShow encode path) or a
    fixture with greater temporal redundancy (less spatial
    content, more zero motion).

- Round 52 — **`msvcrt!_ftol` real impl + `msadds32.ax` PE-load
  surface advance past the CRT FP-truncation edge.**  The MSMPEG4
  audio-side splitter's import walk reached `msvcrt!_ftol` after
  r50 (`_beginthreadex`).  Unlike the r48/r50 fail-soft no-op pair,
  `_ftol` is actively called from filter-coefficient init paths and
  needed a real implementation — a constant 0 or wrong-sign
  truncation would scramble every conversion of a precomputed float
  coefficient back to the i32 the splitter's FIR loops expect.
  - `src/win32/msvcrt.rs` — new `stub_ftol`:
    `long __cdecl _ftol(double)`.  Per the MSVC ABI the `double`
    argument is on the x87 stack (caller emits `FLD qword ptr [arg]`
    before the CALL); the stub reads `ST(0)`, truncates toward zero
    via `f as i32` (Rust 2018+ semantics), pops the x87 slot, and
    returns the i32 in `eax`.  Saturation: NaN → `i32::MIN`
    (the MSVC "indefinite integer" sentinel `0x8000_0000`),
    `f >= 2^31` → `i32::MAX`, `f <= -2^31-1` → `i32::MIN`.  Registered
    with `arg_dwords = 0`: the *argument* is on the x87 stack and not
    on the regular cdecl stack at all.
  - `tests/round52_msvcrt_ftol.rs` — 14 integration tests pinning
    truncation toward zero (positive & negative fractions),
    sub-unit fractions rounding toward zero (`0.5 → 0`,
    `-0.5 → 0`), the saturation envelope (`±∞`, `±1e20`, NaN), the
    exact `i32::MAX` boundary, exact-integer passthrough, and the
    "x87 stack depth decreases by exactly 1" contract.  Headline:
    `Sandbox::load("msadds32.ax")` advances past `_ftol` to surface
    the **next blocker — `msvcrt!rand`**.

- Round 51 — **Encode side of the IC* surface lands end-to-end
  against `mpg4c32.dll`; `quality=5000` BGR24 → MP43 → BGR24
  self-roundtrip at 27.83 dB PSNR.** Previous rounds (21..44)
  drove the decode pipeline (`ICDecompressQuery` /
  `ICDecompressGetFormat` / `ICDecompressBegin` / `ICDecompress`
  / `ICDecompressEnd`) at 42.9 dB across 17/17 frames.  Round
  51 adds the symmetric encode pipeline; the codec now both
  produces and consumes MP43 elementary bitstreams under the
  bare VfW path (no DirectShow muxing layer needed for encode).
  - `src/win32/vfw32.rs` — six new `IC*Compress*` host-side
    wrappers (`ic_compress_query`, `ic_compress_get_format`,
    `ic_compress_get_size`, `ic_compress_begin`, `ic_compress`,
    `ic_compress_end`) mirroring the existing `ic_decompress_*`
    family.  The `ICM_COMPRESS_*` message ordinals
    (`ICM_USER + 4..9` = `0x4004..0x4009`) and the 48-byte
    `ICCOMPRESS` struct layout (12 dwords) are transcribed
    against `winsdk-10/Include/.../um/Vfw.h` and the MSDN
    `ICCompress` / `ICCompressBegin` / etc. topic pages.
    `ICCOMPRESS_KEYFRAME = 0x1` matches the same header.
    `ic_compress_begin` invokes the same
    [`msmpeg4_v3_preinit`] handshake plant the round-22
    decompress-begin path uses — without it,
    `ICCompressBegin` against mpg4c32 returns `ICERR_INTERNAL`
    (`-100`) for the same v3-wrapper-handshake gate that
    `ICDecompressBegin` hit pre-r22.  `ic_compress` returns a
    new [`CompressOutcome`] aggregate carrying the encoded
    bytes, post-call output BIH (whose `biSizeImage` holds the
    actual encoded size), and the codec-written
    `*lpdwFlags` / `*lpckid` slot values.  Truncates the byte
    vector at `output_bih.size_image` when the codec reports
    a non-zero in-bounds size (the MSDN-documented "on return
    the codec sets `biSizeImage`" contract).
  - `src/runtime.rs` — six matching [`Sandbox`] convenience
    methods (`ic_compress_query`, `ic_compress_get_format`,
    `ic_compress_get_size`, `ic_compress_begin`,
    `ic_compress`, `ic_compress_end`) forwarding into the
    `vfw32` module.
  - `src/win32/mod.rs` + `src/win32/vfw32.rs` — corrected
    the `HicEntry::mode` doc-comment and the `ic_open` doc-
    comment to reflect the canonical vfw.h mapping
    (`ICMODE_COMPRESS = 1`, `ICMODE_DECOMPRESS = 2`); both
    were inverted in the original docs.  No behavioural
    change — Microsoft's codecs are historically permissive
    about the mode word at DRV_OPEN, and existing tests
    continue to pass `mode=2` for decode unchanged.
  - `tests/round51_msmpeg4_encode_roundtrip.rs` — three new
    integration tests against `mpg4c32.dll`:
    - `msmpeg4_drv_open_compress_mode_returns_nonzero_hic`
      proves `ICOpen('VIDC','MP43', ICMODE_COMPRESS=1)`
      mints a HIC (the codec accepts compress-mode at
      DRV_OPEN; the ICINFO `dwFlags = VIDCF_QUALITY |
      VIDCF_TEMPORAL` from round 24 was the encode-capability
      announcement).
    - `msmpeg4_encode_lifecycle_and_self_roundtrip` walks the
      full BGR24 → MP43 encode → BGR24 decode cycle for a
      176×144 deterministic gradient pattern at
      `quality=5000`: I-frame compresses to 970 bytes (~78×
      ratio from 76032-byte uncompressed), self-roundtrip
      decode yields 27.83 dB PSNR-BGR24.  The output FOURCC
      the codec emits is empirically `MP43` regardless of the
      input format (the codec hard-codes its compressed-output
      tag — no FOURCC variant negotiation is honoured on the
      encode side, mirroring the round-44 decode-side
      `IPin::ReceiveConnection` "MP43 only" finding).
    - `msmpeg4_encode_iframe_then_pframe` drives a second
      frame with `flags=0` and `prev=frame0`; both frames
      encode successfully (I=970, P=1306 bytes), though the
      codec sets `*lpdwFlags = ICCOMPRESS_KEYFRAME |
      AVIIF_KEYFRAME` (`0x12`) for both because the second
      frame's content is identical to the first — the codec
      is allowed to override the caller's flag request per
      the MSDN `ICCompress` "the codec sets `*lpdwFlags` to
      indicate the actual frame type emitted" contract.
    - `msmpeg4_compress_query_format_inventory` mass-probes
      `ICCompressQuery` against 13 BIH shapes; the codec
      accepts BGR24, BGR32, YV12, I420, IYUV, YUY2, UYVY,
      RGB16-565, and BGR8/palette as encode inputs, but
      rejects NV12 / NV21 / RGB15-555 / `MP43-in-as-input`
      with `ICERR_UNSUPPORTED` (`-2` / `0xFFFFFFFE`).  No
      hidden FOURCC-self-loopback in the encoder.
  - Three new unit tests in `tests` inside
    `src/win32/vfw32.rs` pin the `ICCOMPRESS` struct size,
    the `ICM_COMPRESS_*` constants, and the canned-driver-proc
    round-trip behaviour of the new wrappers.
  - **Empirical findings:**
    - The encoder's `ICCompressGetSize` for 176×144 BGR24
      input returns `W*H*3 = 76032` — the worst-case
      "encoded fits in uncompressed" upper bound, not a
      codec-specific tighter estimate.  Real-world callers
      should size their output buffer to at least that
      value.
    - The codec sets `*lpckid = 'dc\0\0'` (`0x6364` LE) —
      the AVI compressed-frame chunk-id stem, missing the
      leading `00` byte (the stream-index nibble pair).
      An AVI muxer would prepend the stream index to form
      the canonical `'00dc'`.
    - At `quality=5000`, the codec emits keyframes for both
      frame 0 and frame 1 even when frame 1 is requested as
      a P-frame.  This is consistent with the codec's
      published behaviour at high-quality settings; lower
      `quality` values (1000..3000) typically force the
      P-frame branch.  Round 51 does not exercise the
      low-quality regime — future rounds can.

- Round 50 — **`msvcrt!_beginthreadex` stub advances `msadds32.ax`
  PE-load past the splitter's CRT thread-creation edge; combined
  with the r48 `_endthreadex` no-op stub this closes the entire
  CRT thread-lifecycle surface for the splitter's PE-load.**
  Round 49 wired `_strnicmp` and pinned the next splitter blocker
  as `_beginthreadex`; round 50 wires the 6-arg cdecl
  `_beginthreadex` (`uintptr_t __cdecl _beginthreadex(void
  *security, unsigned stack_size, unsigned (__stdcall
  *start_address)(void *), void *arglist, unsigned initflag,
  unsigned *thrdaddr)`) as a fail-soft no-op returning 0.  MSDN
  documents the failure contract as "returns 0 and sets errno to
  a nonzero value" — return 0 IS the documented failure sentinel,
  so callers that respect the documented "thread creation can
  fail" branch fall back or skip the worker-thread codepath
  cleanly.  The codec sandbox never actually spawns the
  splitter's worker thread on the decode path we drive (we only
  exercise `DLL_PROCESS_ATTACH` / `DriverProc` /
  `IPin::ReceiveConnection`); real call sites in the splitter's
  init layer check the return for non-zero and either fall back
  or skip (the worker thread is the splitter's render loop,
  which we never drive).
  - `src/win32/msvcrt.rs` — `stub_begin_thread_ex` + registry
    entry under a new "Round-50 addition: msadds32.ax PE-load
    surface" section.  All six cdecl args are pulled through
    `arg_dword` so a stack-bounds trap surfaces as a proper
    `Win32Error::InvalidArgument` rather than a silent
    under-read.  If the caller passes a non-NULL `thrdaddr`
    pointer the stub clears `*thrdaddr` to 0 via
    `mmu.store32(thrdaddr, 0)`; OOB pointers are silently
    swallowed (the MSDN contract has no way to surface a fault
    back to the caller, and panicking would tear down the host
    process — the alternative would be bubbling
    `Win32Error::InvalidArgument` up through the dispatcher,
    which would propagate as a sandbox-side trap and abort the
    decode).
  - `tests/round50_msvcrt_beginthreadex.rs` — 5 tests:
    registration probe; the canonical 6-dword call with NULL
    `thrdaddr` returning 0; the non-NULL `thrdaddr` write-back
    probe (pre-seed the slot to `0xCAFE_BABE`, confirm the stub
    overwrites it with 0); a fail-soft probe on an unmapped
    `thrdaddr` (`0x0000_0010`); and the headline
    `Sandbox::load("msadds32.ax")` PE-load advance with a
    negated-substring assert on the error message so any silent
    forward progress in a sibling round shows up here.
  - **Headline.**  `MPG4DS32.AX` (the DirectShow
    MS-MPEG-4-v3 decoder filter; the round-44 critical
    path) does NOT import `_beginthreadex` — only
    `msadds32.ax` does — so no DirectShow / VfW decode metric
    changes.  The win is exclusively in the splitter's
    PE-load surface, which moves from "stuck at
    `_beginthreadex`" to "stuck at `msvcrt!_ftol`" (the CRT
    float-to-long conversion helper, MSDN-documented as
    `long __cdecl _ftol(double)`).
  - **Next-round blocker.**  Round 51 should add
    `msvcrt!_ftol` (the CRT `double → long` truncation
    helper invoked by codecs through `fld[m64] / call _ftol`
    when the x87 implicit-rounding mode disagrees with the
    target's truncation contract).  Implementation shape: pop
    the x87 ST(0) double, truncate toward zero, return the
    `i32` result in `eax` (the MSDN contract is `long`, which
    is 32-bit on Win32 / MSVC).  cdecl no-arg, no callee-cleanup.
  - Stub documented from the MSDN signature page only
    (`learn.microsoft.com/.../beginthread-beginthreadex`);
    no ReactOS / Wine / MinGW msvcrt source consulted.

- Round 49 — **`msvcrt!_strnicmp` stub advances `msadds32.ax`
  PE-load past the splitter's case-insensitive bounded-compare
  edge.**  Round 48 wired `_endthreadex` and pinned the next
  splitter blocker as `_strnicmp`; round 49 implements the
  3-arg cdecl `_strnicmp` (`int __cdecl _strnicmp(const char
  *string1, const char *string2, size_t count)` returning
  `< 0` / `0` / `> 0`) as a real ASCII-tolower bounded
  compare.  Unlike the previous PE-load IAT-stub family
  (`SetTimer`, `KillTimer`, `StretchDIBits`, `_endthreadex`),
  `_strnicmp` is NOT a no-op candidate — the splitter calls
  it during init for FOURCC / header-magic matching, so a
  stub that returns a constant 0 (== "every string compares
  equal") would let the codec take a wrong branch and
  silently misbehave on a later decode.  Each byte is folded
  to lowercase by the ASCII rule `b'A'..=b'Z' → +0x20`; bytes
  ≥ `0x80` are compared byte-for-byte (no Unicode tolower);
  the compare terminates early on the first NUL on EITHER
  side within `count` bytes; the return value is the byte
  difference of the first mismatch (or terminator) cast to
  `i32`, then re-cast to `u32` for `eax`.
  - `src/win32/msvcrt.rs` — `stub_strnicmp` + registry entry
    under a new "Round-49 addition: msadds32.ax PE-load
    surface" section.  All three cdecl args are pulled
    through `arg_dword` so a stack-bounds trap surfaces as a
    proper `Win32Error::InvalidArgument` rather than a
    silent under-read.
  - **Fail-soft envelope.**  Either pointer reading OOB
    (`mmu.load8` returning a [`Trap`]) or an absurdly large
    `count` (`> 1 MiB`) returns 0 ("treat as equal") rather
    than propagating an error.  The MSDN contract has no way
    to surface a fault back to the caller, and the
    alternative — bubbling `Win32Error::InvalidArgument` up
    through the dispatcher — would tear down the decode on
    fuzz-shaped boundary cases that the codec's own use site
    never actually hits.  All "real" call sites
    (3-to-4-byte FOURCC compares against staged const-arena
    strings) are well inside the envelope.
  - `tests/round49_msvcrt_strnicmp.rs` — 13 tests:
    registration probe; the canonical equal-prefix
    case-insensitive compare (`"AVI " == "avi "`); the
    differing-prefix sign check (`"MP43" > "MP42"` → +1); a
    shorter-count probe (`"MP43"` vs `"MP42"` count=3 → 0);
    a NUL-terminator-within-count probe (`"AVI\0"` vs
    `"AVI\0XYZ"` count=7 → 0); a high-byte byte-for-byte
    probe (`0xC0` vs `0xE0` → -32, no Unicode fold); two
    fail-soft probes (OOB pointer + `u32::MAX` count); the
    `count == 0` MSDN-vacuous-equal probe; both-empty;
    one-side-NUL sign pickup (`"AVI\0"` < `"AVIX"`); the
    canonical `"riff"` vs `"RIFF"` use-site echo; and the
    headline `Sandbox::load("msadds32.ax")` PE-load advance
    with negated-substring assert on the error message so
    any silent forward progress in a sibling round shows up
    here.
  - **Headline.**  `MPG4DS32.AX` (the DirectShow
    MS-MPEG-4-v3 decoder filter; the round-44 critical
    path) does NOT import `_strnicmp` — only `msadds32.ax`
    does — so no DirectShow / VfW decode metric changes.
    The win is exclusively in the splitter's PE-load
    surface, which moves from "stuck at `_strnicmp`" to
    "stuck at `msvcrt!_beginthreadex`".
  - **Next-round blocker.**  Round 50 should add
    `msvcrt!_beginthreadex` (the splitter's CRT
    thread-creation entry — documented as `uintptr_t
    _beginthreadex(void *security, unsigned stack_size,
    unsigned (__stdcall *start_address)(void *), void
    *arglist, unsigned initflag, unsigned *thrdaddr)`
    returning the new thread handle on success, 0 on
    failure).  The codec sandbox never actually needs a
    real worker thread on the decode path we drive, so a
    fail-soft stub returning 0 (== "thread creation
    failed", which is a normal failure mode the splitter is
    documented to handle) is the natural shape — paired
    with the round-48 `_endthreadex` no-op, this closes the
    entire CRT thread-lifecycle surface for `msadds32.ax`'s
    PE-load.
  - Stub documented from the MSDN signature page only
    (`learn.microsoft.com/.../strnicmp-wcsnicmp-mbsnicmp-strnicmp-l-wcsnicmp-l-mbsnicmp-l`);
    no ReactOS / Wine / MinGW msvcrt source consulted.

- Round 48 — **`msvcrt!_endthreadex` stub advances `msadds32.ax`
  PE-load past the splitter's thread-teardown edge.**  Round 47
  wired `gdi32!StretchDIBits` and pinned the next splitter
  blocker as `msvcrt!_endthreadex`; round 48 wires the 1-arg
  cdecl `_endthreadex` (`void __cdecl _endthreadex(unsigned
  retval)`) as a fail-soft stub that returns 0.  MSDN documents
  the function as `__declspec(noreturn)` — in the real CRT
  control never returns to the caller after `_endthreadex`
  runs — but the codec sandbox never actually spawns the
  splitter's worker thread on the decode path we drive (we only
  exercise `DLL_PROCESS_ATTACH` / `DriverProc` /
  `IPin::ReceiveConnection`); the IAT slot just needs to
  resolve at PE-load time, and if the codec ever did reach the
  stub we'd want to fall back to the caller's return-address
  rather than terminate the host process, which is exactly what
  a cdecl `Ok(0)` stub does (the dispatcher pops nothing for
  cdecl, the codec's RET picks up the saved return-address from
  the stack).
  - `src/win32/msvcrt.rs` — `stub_end_thread_ex` + registry
    entry under a new "Round-48 addition: msadds32.ax PE-load
    surface" section.  The `retval` arg is pulled through
    `arg_dword` so a stack-bounds trap surfaces as a proper
    `Win32Error` rather than a silent under-read; the value
    itself is never surfaced back to the caller (per the MSDN
    noreturn contract).
  - `tests/round48_msvcrt_endthreadex.rs` — 4 tests: stub
    registered in the msvcrt registry; non-zero `retval`
    returns 0 end-to-end through the dispatcher; degenerate
    `retval == 0` also returns 0; and the headline
    `Sandbox::load("msadds32.ax")` advances past
    `_endthreadex`, with negated-substring assert on the
    error message so any silent forward progress in a sibling
    round shows up as a failure here.
  - **Headline.**  `MPG4DS32.AX` (the DirectShow
    MS-MPEG-4-v3 decoder filter; the round-44 critical
    path) does NOT import `_endthreadex` — only
    `msadds32.ax` does — so no DirectShow / VfW decode
    metric changes.  The win is exclusively in the
    splitter's PE-load surface, which moves from "stuck at
    `_endthreadex`" to "stuck at `msvcrt!_strnicmp`".
  - **Next-round blocker.**  Round 49 should add
    `msvcrt!_strnicmp` (the splitter's case-insensitive
    bounded string compare — documented as `int _strnicmp
    (const char *string1, const char *string2, size_t
    count)` returning `< 0` / `0` / `> 0`; the codec
    presumably uses it for FOURCC / header-magic compares
    during initialisation, so a real ASCII case-insensitive
    `memcmp`-shaped implementation is required, not a
    no-op).
  - Stub documented from the MSDN signature page only
    (`learn.microsoft.com/.../endthread-endthreadex`); no
    ReactOS/Wine/MinGW msvcrt source consulted.

- Round 47 — **`gdi32!StretchDIBits` stub advances `msadds32.ax`
  PE-load past the splitter's render-out edge.**  Round 46 wired
  `user32!{SetTimer, KillTimer}` and pinned the next splitter
  blocker as `gdi32!StretchDIBits`; round 47 wires the 13-arg
  `StretchDIBits` (`int StretchDIBits(HDC, int xDest, int yDest,
  int DestWidth, int DestHeight, int xSrc, int ySrc, int
  SrcWidth, int SrcHeight, const VOID *lpBits, const BITMAPINFO
  *lpbmi, UINT iUsage, DWORD rop)`) as a fail-soft stub that
  returns the caller's `DestHeight` as the "scanlines copied"
  count per MSDN's success contract.  The codec sandbox never
  enters the splitter's render-out path (we drive only the
  PE-load + DLL_PROCESS_ATTACH surface, not the paint cycle
  that would invoke `StretchDIBits`); the IAT slot just needs
  to resolve at PE-load time.  Reporting `DestHeight` rather
  than `GDI_ERROR` satisfies any "scanlines > 0 == success"
  probe at the call site without ever surfacing the explicit
  failure marker from a fail-soft stub.
  - `src/win32/gdi32.rs` — `stub_stretch_dibits` + registry
    entry under a new "Round-47 additions: msadds32.ax
    PE-load surface" section.  All 13 stdcall args are
    pulled through `arg_dword` so a stack-bounds trap
    surfaces as a proper `Win32Error` rather than a silent
    under-read; only `DestHeight` is actually inspected for
    the return value.
  - `tests/round47_gdi32_stretch_dibits.rs` — 4 tests:
    stub registered in the gdi32 registry; `DestHeight` is
    echoed end-to-end through the dispatcher with a 352×288
    canonical-call probe; degenerate `DestHeight == 0`
    echoes 0 (never surfaces `GDI_ERROR`); and the headline
    `Sandbox::load("msadds32.ax")` advances past
    `StretchDIBits`, with negated-substring assert on the
    error message so any silent forward progress in a
    sibling round shows up as a failure here.
  - **Headline.**  `MPG4DS32.AX` (the DirectShow
    MS-MPEG-4-v3 decoder filter; the round-44 critical
    path) does NOT import `StretchDIBits` — only
    `msadds32.ax` does — so no DirectShow / VfW decode
    metric changes.  The win is exclusively in the
    splitter's PE-load surface, which moves from "stuck at
    StretchDIBits" to "stuck at `msvcrt!_endthreadex`".
  - **Next-round blocker.**  Round 48 should add
    `msvcrt!_endthreadex` (the splitter's thread-teardown
    edge — documented as a `void __cdecl` terminator that
    never returns, but in a PE-load context only the IAT
    slot needs to resolve, so a no-op stub returning 0 is
    the natural starting point; the codec never spawns the
    thread on the decode path we drive).
  - Stub documented from the MSDN signature page only
    (`docs.microsoft.com/.../nf-wingdi-stretchdibits`); no
    ReactOS/Wine source consulted.

- Round 46 — **`user32!{SetTimer, KillTimer}` stubs advance
  `msadds32.ax` PE-load past the entire timer-API surface.**
  Round 45 unblocked `MapDialogRect` and pinned the next
  splitter blocker as `KillTimer`; round 46 wires both
  timer-API entries in one commit.  Both stubs are
  fail-soft per the round-24 / round-45 user32 playbook —
  the codec sandbox never enters the message-loop branch
  that would let a `TIMERPROC` callback actually fire, so
  no scheduling is performed host-side; the IAT slots just
  need to resolve at PE-load time.
  - `SetTimer(hWnd, nIDEvent, uElapse, lpTimerFunc)` —
    return `nIDEvent` if non-zero, else a synthetic `1`.
    Both satisfy the documented "non-zero == success"
    probe.
  - `KillTimer(hWnd, uIDEvent)` — return `TRUE` (1) per
    MSDN's "found and destroyed" contract.
  - `src/win32/user32.rs` — `stub_set_timer` +
    `stub_kill_timer` + registry entries under the
    round-46 msadds32 PE-load surface section.
  - `tests/round46_user32_set_kill_timer.rs` — 5 tests:
    both stubs registered; `SetTimer` echoes a non-zero
    `nIDEvent`; `SetTimer` returns the synthetic `1` for
    `nIDEvent == 0`; `KillTimer` returns `TRUE`;
    `Sandbox::load("msadds32.ax")` advances past both
    `KillTimer` and `SetTimer`, with the failure path
    pinned by negated-substring asserts so any silent
    forward progress in a sibling round shows up as a
    failure here.
  - **Headline.**  `MPG4DS32.AX` (the DirectShow
    MS-MPEG-4-v3 decoder filter; the round-44 critical
    path) does NOT import either timer API — only
    `msadds32.ax` does — so no DirectShow / VfW decode
    metric changes.  The win is exclusively in the
    splitter's PE-load surface, which moves from "stuck
    at KillTimer" to "stuck at gdi32!StretchDIBits".
  - **Next-round blocker.**  Round 47 should add
    `gdi32!StretchDIBits` (the splitter's render-out
    surface — fail-soft return per MSDN's
    GDI_ERROR/scanline-count contract is the natural
    starting point).
  - Stubs documented from MSDN signature pages only
    (`docs.microsoft.com/.../nf-winuser-settimer`,
    `nf-winuser-killtimer`); no ReactOS/Wine source
    consulted.

- Round 45 — **`user32!MapDialogRect` stub unblocks `msadds32.ax`
  PE-load past the round-24 user32 surface gap.**  The
  MS-MPEG-4-v3 reference bundle's audio-splitter half
  (`msadds32.ax`) imports 29 distinct `user32` symbols (full
  list per PE-walk in `tests/round45_user32_map_dialog_rect.rs`).
  Round 24 wired the first batch (`RegisterClassExA` /
  `UnregisterClassA`); round 45 adds `MapDialogRect` as a
  fail-soft identity passthrough — leave the caller's RECT
  untouched and report success per MSDN's `BOOL` return
  contract.  After round 45, `Sandbox::load("msadds32.ax")`
  advances past `MapDialogRect` and now stops at the NEXT
  unresolved user32 import: `KillTimer`.  The stub itself is
  documented from the public MSDN signature page only
  (`docs.microsoft.com/.../nf-winuser-mapdialogrect`); no
  ReactOS/Wine source consulted.
  - `src/win32/user32.rs` — `stub_map_dialog_rect` +
    registry entry under the round-24 msadds32 PE-load
    surface section.
  - `tests/round45_user32_map_dialog_rect.rs` — 4 tests:
    registry-resolves; identity passthrough returns TRUE +
    leaves the seeded RECT bytes untouched; NULL-RECT call
    does not trap; `Sandbox::load("msadds32.ax")` advances
    past `MapDialogRect` to the next blocker (`KillTimer`),
    pinned by exact error-message match so any silent
    forward progress in a sibling round shows up as a
    failure here.
  - **Headline.**  `MPG4DS32.AX` (the DirectShow MS-MPEG-4-v3
    decoder filter; the round-44 critical path) does NOT
    import `MapDialogRect` — only `msadds32.ax` does — so
    no DirectShow / VfW decode-path metric changes.  The
    win is exclusively in the splitter's PE-load surface,
    which moves from "stuck at MapDialogRect" to "stuck at
    KillTimer", ungating any future round that wants to
    drive the splitter's DLL_PROCESS_ATTACH or DriverProc.
  - **Next-round blocker.**  Round 46 should add
    `user32!{KillTimer, SetTimer}` (both required for the
    splitter's window-pump path, both fail-soft per MSDN —
    KillTimer returns TRUE iff a registered timer with the
    matching ID was found, SetTimer returns the timer ID
    handed to it).

- Round 44 — **full MS-MPEG-4 v3 fixture corpus exercised
  through the round-43 DirectShow pipeline**: 16 fixture-runs
  out of `docs/video/msmpeg4-fixtures/`, all surfacing every
  expected `Frame::Video` (20/20 frames in aggregate).  Two
  distinct axes covered:
  - **FourCC parity (6/6)**.  The corpus's six fourcc-*
    fixtures (MP43, DIV3, DIV4, DVX3, AP41, COL1) carry a
    byte-identical MS-MPEG-4-v3 elementary bitstream wrapped
    in AVI containers tagged with each respective FOURCC.
    Empirical finding: `MPG4DS32.AX` only accepts the MP43
    `MEDIASUBTYPE` at `IPin::ReceiveConnection`; every other
    FOURCC subtype is rejected with `0x8004022a`
    (`VFW_E_TYPE_NOT_ACCEPTED`).  This is a real codec
    property — `mpg4ds32` is a single-tag filter — not a
    host bug.  Real DirectShow stacks delegate FourCC →
    filter routing to the FilterMapper, which always presents
    `mpg4ds32` with MP43.  R44 mirrors that policy: each
    fixture's bytes are driven through a host factory
    registered with `record.fourcc="MP43"` regardless of the
    AVI container tag.  All 6 surface a Video frame.
  - **Harder content fixtures (4/4 + 5/5 + 5/5 single)**.
    Round 43 only drove `gop-30-352x288` and the round-42
    I+P pair; R44 adds the remaining seven content fixtures
    the docs corpus ships:
    - `motion-pan-352x288` (4 frames at CIF, large
      mandelbrot pan → big inter-frame MVs): **4/4 Video**.
    - `with-skip-mbs-352x288` (5 frames at CIF, qscale=16
      testsrc2 → ~38% SKIP-MB fraction): **5/5 Video**.
    - `qscale-high-352x288` (qscale=31, sparse coefs):
      **1/1 Video**.
    - `qscale-low-352x288` (qscale=2, dense coefs):
      **1/1 Video**.
    - `intra-pred-active-352x288` (mandelbrot I-frame,
      AC-pred direction churn): **1/1 Video**.
    - `i-only-352x288-cif` (testsrc I-frame at CIF):
      **1/1 Video**.
    - `tiny-i-only-176x144` (QCIF I-frame baseline):
      **1/1 Video**.
  - `tests/round44_fourcc_parity_and_harder_fixtures.rs`
    (4 tests; all 4 pass under `MPG4DS32.AX`):
    `r44_iframe_decodes_through_all_six_fourcc_containers`,
    `r44_motion_pan_4_frame_decodes_end_to_end`,
    `r44_with_skip_mbs_5_frame_decodes_end_to_end`,
    `r44_iframe_corpus_decodes_end_to_end`.  Each helper
    asserts plane0 = `w·h·3` bytes (24bpp BGR) per Video
    frame.  Tests gracefully skip when the codec DLL or a
    fixture is missing (CI safety) and assert the remaining
    available count, with a floor of ≥4 fixtures so the
    pass condition can't degenerate.
  - **No code change required** in `src/`: round 43's
    sample-release cycle, FourCC-blind subtype negotiation,
    and pool sanity-checks already supported the entire
    corpus.  R44 is an empirical confirmation that the R43
    surface generalises across MV magnitude, SKIP-MB
    density, qscale extremes, and AC-pred direction churn
    on real (ffmpeg-encoded) MS-MPEG-4 v3 bitstreams.

- Round 43 — **full 6-frame GOP decodes end-to-end at 352×288**:
  the `gop-30-352x288` MS-MPEG-4 v3 fixture now surfaces 6/6
  `Frame::Video`s through the same `SandboxedDshowDecoder`
  instance (round 42: 1/6).  Closes both R43 blockers
  identified empirically in round 42's diagnostic blob.
  - **Blocker (a) — output-allocator pool walk traps on
    P-frame.**  `alloc_get_buffer` now sanity-checks every
    pool pointer before the `cur+36` / `cur+32` reads — a
    corrupted next-link surfaces as `VFW_E_TIMEOUT` instead
    of a memory-fault trap inside our stub.  This was the
    `cur+36 = 0xffff0223` (i.e. `cur ≈ 0xffff_01ff`) failure
    that aborted the entire pipeline on round 42's frame 1
    of the gop-30 run.
  - **Blocker (b) — sample-release cycle gap.**  Three
    coordinated changes close the cycle:
    - New `sample_release` thunk replaces the generic
      `release` for `IMediaSample::Release`: when refcount
      transitions `1 → 0`, clears the sample's `in_use`
      flag at `+36`, mirroring the canonical
      `CMediaSample::~CMediaSample` destructor's call back
      into `pAllocator->ReleaseBuffer`.
    - `alloc_get_buffer` now FORCES the issued sample's
      refcount to exactly `1` (was: bump-by-1 over whatever
      the pool entry held), so the codec's standard
      one-AddRef + one-Release pattern reliably drives it
      through `1 → 0`.
    - `receive_frame` calls `IMemAllocator::ReleaseBuffer`
      on the input allocator after `IMemInputPin::Receive`
      returns, freeing the just-consumed input slot for
      the next `send_packet`.
  - Diagnostic blob in `receive_frame` extended with
    `r43_oalloc_state` (output allocator's first 0x40
    bytes) and `r43_oalloc_pool` (linked-list walk of the
    output pool head + first six entries with `in_use` /
    `next` per slot) so any future trap on this path
    arrives with the post-fix recovery telemetry already
    in hand.
  - `tests/round43_full_gop_decode.rs` (2 tests):
    `r43_gop30_full_six_frame_decode` (asserts 6/6 Video
    frames from the gop-30-352x288 fixture, each with
    plane0 = 352·288·3 = 304128 bytes), and
    `r43_pool_recycle_survives_ten_ip_cycles` (drives 10×
    back-to-back I+P pairs through one decoder = 20 frames
    total, well past the 4-slot pool, asserts ≥10 Video
    frames so the recycle path can't silently regress to
    pool exhaustion).  20/20 frames surface in practice.
  - Round-42 regression guard preserved: the
    `r42_iframe_then_pframe_through_same_decoder` test
    still passes (2/2 Video).

- Round 42 — **multi-frame DShow decode lands**: drives the
  `i-frame-then-p-frame-176x144` fixture's I-frame followed by
  its P-frame through the SAME `SandboxedDshowDecoder` instance
  and surfaces both as `Frame::Video` (1 → 2 frames end-to-end).
  Round 41 was the first ever Video frame out of the DShow path
  but only ever drove ONE packet; r42 confirms the codec's
  internal state machine survives back-to-back `Receive` calls
  against the same filter instance.
  - `tests/round42_dshow_iframe_then_pframe.rs` (4 tests):
    `r42_iframe_then_pframe_through_same_decoder` (the headline
    I+P run, asserts plane0=176·144·3=76032 bytes per frame),
    `r42_gop30_six_frame_run_through_dshow` (drives all 6 GOP
    samples of `gop-30-352x288` and pins per-frame outcomes),
    `r42_codec_id_reflects_registered_fourcc`,
    `r42_fixture_extracts_two_video_samples`.
  - **R43 blockers identified empirically by gop-30 run.**  At
    352×288 (4× the 176×144 surface), the I-frame still surfaces
    Video but frames 1..=3 trap with `HostIMemAllocator::GetBuffer:
    memory fault at 0xffff0223 (page unmapped)` at MPG4DS32 RVA
    `0x4064d4` — the same instruction r41 fixed for the INPUT
    allocator, now hitting the OUTPUT side via `output_alloc=
    0x60200fc0` (`output_alloc_vtbl0=0x60200fd0`,
    `output_alloc_qi_thunk=0xfffe0240`).  The codec walks slot 7
    (`call [ecx+0x1c]`) of the output allocator's vtable with
    `ecx=0x60200fd0` (a vtable address rather than a `this`
    pointer), which suggests our output-side stub is hit with a
    different calling convention than the input one.  Frames
    4+5 then return `0x80040211 (VFW_E_NOT_COMMITTED)` once the
    pool is exhausted by the unreleased samples from frames
    1..=3.  Two distinct R43 sub-goals: (a) audit the
    output-side `IMemAllocator` mint helper for parity with the
    input side fixed in r41; (b) ensure the host's
    `media_sample_release` / `IMemAllocator::ReleaseBuffer`
    cycle returns samples to the pool once the downstream
    receive callback has surfaced them.
  - The 176×144 I+P case stays clean: 2/2 frames Video.  The
    diagnostic blob from the gop-30 run — full register
    snapshots at five Transform-internal watchpoints, the
    output-allocator's vtable contents, the trap RVA + esp /
    ebp / ebx / ecx state — gives r43 immediate handoff data.

### Fixed

- Round 41 — **`IMemAllocator::GetBuffer` arg-count fix unblocks
  MP43 keyframe decode end-to-end via DirectShow**.  The
  bisect dispatched in round 40 (snapshot watchpoints across
  `Transform`'s ten internal `call dword ptr [...]` sites) showed
  the 4-byte stack imbalance was introduced at the FIRST site,
  RVA `0x4064d4 = call [ecx+0x1c]` — `IMemAllocator::GetBuffer`,
  signature `(this, IMediaSample **ppBuffer, REFERENCE_TIME
  *pStartTime, REFERENCE_TIME *pStopTime, DWORD dwFlags)` =
  **5 pushed dwords**.  Our host stub registration in
  `crate::com::host_iface::register` had `arg_dwords=4`, so
  the dispatcher's stdcall callee-cleanup
  (`win32::dispatch_stub`) popped 16 bytes instead of 20,
  leaving esp 4 bytes too low.  Transform's matched
  `pop ebx` at `0x4065c4` then read junk (`0x60000110` =
  filter_base) instead of the correct saved-ebx slot one
  dword higher (`0x600007a0` = pInSample), causing the
  downstream slot-13 dispatch at `0x40263b` to land on the
  filter primary vtable's slot 13 (`0x2da7`) and ultimately
  fault inside `IsEqualGUID` at RVA `0x7184`.
  - `arg_dwords` for `IMemAllocator::GetBuffer` bumped 4 → 5.
  - `alloc_get_buffer` now reads the previously-ignored
    `dwFlags` arg (per `strmif.h`: `AM_GBF_NOTASYNCPOINT |
    AM_GBF_PREVFRAMESKIPPED | AM_GBF_NOWAIT`).  The host
    pool ignores the bits but the read keeps the per-arg
    trace blob honest.
  - Receive now returns S_OK for the MP43 keyframe; the
    downstream `HostIMemInputPin::Receive` callback queues
    a sample which `surface_received_dshow_frame` flips to
    top-down BGR24 and surfaces as a `Frame::Video`.
  - Watchpoint instrumentation is preserved (drained on
    both success + trap branches now) so any future
    regression re-traps with the bisect data immediately
    to hand.
  - Tests in `tests/round41_getbuffer_arg_count_fix.rs`
    (renamed from `round40_*`) assert the FIXED behaviour:
    a Video frame surfaces from the keyframe, and
    `Registry::resolve(host-com.host, "IMemAllocator::
    GetBuffer")` returns an entry whose `arg_dwords == 5`.
  - `tests/round39_imediasample2_qi.rs` updated: the three
    tests previously anchored to the trap-baseline
    diagnostic blob now assert the post-r41 fix (Video
    frame surfaces).
  - **Receive trap GONE** — first end-to-end MP43 decode
    via DirectShow.

### Added

- Round 40 — **register-snapshot + memory-probe watchpoints
  identify a stack imbalance inside `CTransformFilter::Transform`
  (RVA `0x6473..0x65c6`) as the root cause of the r39 trap**.
  - New `Cpu::add_register_watchpoint(eip)` /
    `clear_register_watchpoints()` /
    `take_memory_snapshots()` instrumentation.  When an
    armed eip is hit at `Cpu::step` entry, the integer
    register file (eax/ecx/edx/ebx/esp/ebp/esi/edi) plus
    four parallel memory probes (`[esp]`, `[esp+4]`,
    `[ebp+8]`, `[ebp-0x50]`) are snapshotted BEFORE the
    instruction executes — so the values reflect the
    state at the watchpoint, NOT at trap time (which may
    have been overwritten by intervening writes).  Capped
    at 64 hits per run.
  - `discovery::codec::receive_frame` arms 16 watchpoints
    around the post-Transform call site in the enclosing
    `0x25a2` function and across Transform's own prologue
    (`0x6479` push ebx) and epilogue (`0x65c4` pop ebx).
  - On trap, the `Receive` diagnostic now carries
    `r40_snaps=[...]` (per-hit register file) and
    `r40_arg1=[...]` (per-hit `[esp]` / `[esp+4]` /
    `[ebp+8]@addr` / `[ebp-0x50]@addr`).
  - **Findings.**  At the post-Transform return site
    `0x402626`, `ebx == 0x60000110` (filter_base), NOT the
    expected `0x600007a0` (pInSample).  **Hypothesis (b)
    ruled out**: throughout Transform's body the arg slot
    `[ebp+8]@0x900ffeb8 == 0x600007a0` (pInSample intact)
    and the saved-ebx slot `[ebp-0x50]@0x900ffe60 ==
    0x600007a0` (correctly preserved).  **Hypothesis (a)
    confirmed** in a refined form: at Transform's
    `0x4065c4` `pop ebx`, esp == `0x900ffe5c` — FOUR BYTES
    LOWER than `[ebp-0x50] == 0x900ffe60`.  pop ebx reads
    from `[esp]` (= `0x60000110`, a leftover stack value)
    instead of from the saved-ebx slot one dword higher
    (which DOES hold `0x600007a0`).  Some intermediate
    `__stdcall` call inside Transform's body either
    pushed an extra arg or returned with a callee-cleanup
    short by 4 bytes.
  - **Trap site unchanged** at MPG4DS32 RVA `0x7184`
    (`IsEqualGUID(NULL+0x1c, &kIID)` inside the helper at
    `0x7176`).  The slot-13 dispatch at `0x40263b` is
    `(*ebx->vtable[13])(ebx, &arg)` — with the wrong
    `ebx` it lands on filter primary vtable slot 13 =
    `0x2da7`, which expects `ecx == this` per `__thiscall`
    but receives `ecx == 0x900ffee0` (a stack address).
    `0x2da7` then does `mov ebx, ecx; mov ecx, [ebx+0x8c];
    add ecx, 0x1c; call 0x7176` — the `[ebx+0x8c]` reads
    from a stack location (junk), `ecx` becomes `0x1c`,
    and `IsEqualGUID(0x1c, ...)` faults.
  - **R41 handoff.** Bisect inside Transform by arming
    watchpoints at every `call dword ptr [...]` site
    (`0x4064d4`, `0x4064f3`, `0x406505`, `0x406545`,
    `0x40655b`, `0x40656e`, `0x40657f`, `0x406590`,
    `0x4065a8`, `0x4065bd`) and tracking esp delta
    before/after each.  The first site whose delta differs
    from args_pushed is the culprit.  Likely candidates:
    `0x4064d4` (slot 7 of `[ecx]`'s vtable, possibly an
    `IBaseFilter::EnumPins` or pin-side allocator method)
    or `0x4064f3` (`call [eax]` = QueryInterface, 3-arg
    `__stdcall` with `ret 12` callee-cleanup).
  - 5 new tests in `tests/round40_ebx_origin_at_0x2626.rs`:
    `r40_ebx_at_post_transform_is_filter_base`,
    `r40_slot13_call_dispatches_off_filter_vtable`,
    `r40_arg1_pinsample_intact_across_snapshots`,
    `r40_function_entry_does_not_bind_ebx_to_arg1`,
    `r40_stack_imbalance_at_pop_ebx_confirmed`.
  - Receive trap unchanged at MPG4DS32 RVA `0x7184`.

- Round 39 — **`IID_IMediaSample2` host-side QI support; Transform
  reaches its success-tail at `0x65c0`** (was `0x6560` failure
  cleanup).  Round-38 disasm of the QI at MPG4DS32.AX RVA
  `0x4064f3` identified the IID being requested as
  `{36B73884-C2C8-11CF-8B46-00805F6CEF60}` =
  `IID_IMediaSample2` (Microsoft Platform SDK `strmif.h`
  extension of `IMediaSample`, two new methods: `GetProperties`
  / `SetProperties` of the `AM_SAMPLE2_PROPERTIES` struct).
  Returning `E_NOINTERFACE` (the round-30..38 baseline) sent
  the codec down the QI-failure cleanup branch.  Round 39:
  - New IID constant `IID_IMEDIASAMPLE2` in `crate::com`.
  - New slot constants `SLOT_MEDIASAMPLE_GET_MEDIA_TYPE` (13),
    `SLOT_MEDIASAMPLE_SET_MEDIA_TYPE` (14),
    `SLOT_MEDIASAMPLE_GET_MEDIA_TIME` (17),
    `SLOT_MEDIASAMPLE_SET_MEDIA_TIME` (18),
    `SLOT_MEDIASAMPLE2_GET_PROPERTIES` (19),
    `SLOT_MEDIASAMPLE2_SET_PROPERTIES` (20).
  - `sample_qi` accepts `IID_IMEDIASAMPLE2`.
  - Three new host stubs: `sample_set_media_time` (slot 18 —
    previously NULL on the host vtable, an active footgun
    because Transform's failure-cleanup `[ecx+0x48]` call at
    RVA `0x4065bd` would have dispatched to NULL),
    `sample_get_properties` and `sample_set_properties`
    (slots 19/20).  Both round-trip the public
    `AM_SAMPLE2_PROPERTIES` fields the codec writes
    (`cbData` / `dwSampleFlags` / `lActual` / `pbBuffer` /
    `cbBuffer` / `pMediaType`).
  - Host sample vtable resized 18 → 21 entries, header from
    `64 + 18*4 = 136` → `64 + 21*4 = 148` bytes (rounded to
    16 = 160).
  - Pre-Receive diagnostic dump in
    `discovery::codec::receive_frame` extended with
    `output_alloc` / `output_alloc_vtbl0` /
    `output_alloc_qi_thunk` so r40 can see whether the
    codec's output-pin allocator (set inside its private
    `CoCreateInstance(CLSID_MemoryAllocator)` call from
    sub-goal r35) has the host-thunk vtable head.
  - New `tests/round39_imediasample2_qi.rs` (3 tests):
    Transform success-tail RVA `0x65c0` reached, helper
    success RVA `0x5f24` reached, sample slot 13 unchanged
    after run.
  - `Receive` trap RVA `0x7184` is unchanged (still
    `IsEqualGUID(NULL+0x1c, &GUID_NULL)`).  R40 needs to
    explain why pInSample's slot-13 call at RVA `0x40263b`
    in `0x25a2` resolves to `0x2da7` (filter primary vtable
    slot 13 = `JoinFilterGraph`) instead of the host thunk
    `0xfffe03a0` we wrote at `[obj+0x74]`.

- Round 38 — **identify the codec's C++ class base + prove
  `[filter_base+0x8c]` is NON-NULL pre-Receive**, ruling out
  the round-36/37 hypothesis that the trap at MPG4DS32 RVA
  `0x7184` was caused by an uninitialised input-pin field on
  the CoCreateInstance-returned filter object.
  - **Static disasm correlation** — the `Receive → Transform`
    call chain is now fully reverse-engineered from the codec
    DLL's own bytes (`objdump -d -M intel`) at every site
    listed in the round-37 GOAL: `0x69ab` (CTransformInputPin::
    Receive prologue, EnterCriticalSection + delegate to slot
    21 = `0x25a2`), `0x5e34` (the worker that calls
    `sample->GetTime` at slot 5 + `sample->GetMediaType` at
    slot 13), `0x25a2` (CTransformFilter::Receive — calls
    `0x6fee` preprocess then takes failure branch `0x261a`
    when our `sample_get_time` returns `VFW_S_NO_STOP_TIME`,
    falling through to call `0x6473` Transform), `0x6473`
    (CTransformFilter::Transform — reads `[filter+0x8c]`
    input-pin pointer, calls allocator GetBuffer, may QI for
    a sub-interface and jump to cleanup `0x6560` on failure),
    and `0x2da7` (slot 13 of vtable `0x269f4` — the codec's
    PRIMARY C++ class vtable).
  - **Vtable layout decoded** — the codec's filter constructor
    at RVA `0x24ca` stamps FIVE vtables on the C++ class:
    primary at `[obj+0] = 0x269f4` (`CTransformFilter`-style
    polymorphic methods), IPersist at `[obj+0xc] = 0x269b8`
    (this is what CoCreateInstance returns as IBaseFilter, so
    `self.filter = filter_base + 0xc`), three more at `+0x10`,
    `+0xc0`, `+0xc4`.  The `m_pInput` field that traps in
    `0x2da7` (`mov ecx, [ebx+0x8c]; add ecx, 0x1c; call
    0x7176`) is at `[filter_base + 0x8c]` — i.e.
    `[self.filter + 0x80]`.
  - **Pre-Receive sanity dump** — `discovery::codec::receive_
    frame` now reads `[filter_base + 0]`, `[filter_base +
    0x8c]`, `[filter_base + 0x90]`, `[sample]`, and
    `[sample_vtbl + 0x34]` BEFORE driving Receive, so the
    trap message's new `r38_pre=...` section carries:
    `sample=<host arena>`, `sample_vtbl=<host arena + 0x40>`,
    `sample_vtbl[+0x34]=0xfffe<thunk>` (proves slot 13 is
    OUR `sample_get_media_type` host thunk), `mip=<host
    arena>`, `self.filter=<host arena>`, `filter_base=<self.
    filter - 0xc>`, `[filter_base+0]=0x1c4269f4` (matches
    the constructor's stamp), `[filter_base+0x8c]=0x60000280`
    (NON-NULL — input pin IS allocated by EnumPins/Next).
  - **Round-37 hypothesis falsified** — the trap is reached
    even though `[filter_base + 0x8c]` is non-NULL, so the
    failing object inside the codec's Transform chain is NOT
    the top-level filter.  r39 should chase which intermediate
    object's `+0x8c` is being read at trap time (likely
    accessed via the QI path at `0x4064f3` — the QI may
    return a sub-interface whose vtable's slot 13 also =
    `0x2da7`, leading to a cascading null-pointer in some
    transient).
  - **Force-allocation fallback (defensive, currently no-op)** —
    if `[filter_base + 0x8c]` IS observed as NULL on a future
    fixture, we now call slot 7 of the primary vtable
    (`0x33fd`, the per-CLSID GetPin helper) on `filter_base`
    to force lazy-init.  The current MP43 fixture's filter
    has the field already non-NULL, so this branch never
    runs in r38; it remains as a guard for r39+ scenarios.
  - **`tests/round38_filter_base_offset.rs`** — three tests:
    (1) the trap message MUST carry the `r38_pre=` section
    + identify `filter_base`, `[filter_base+0]=0x1c4269f4`,
    and `[filter_base+0x8c]` non-NULL; (2) the input sample's
    slot 13 MUST resolve to a host thunk in `0xFFFE_xxxx`
    space (proves no codec sample-substitution); (3) the
    round-37 negotiation baseline (GA/SP/CO=S_OK,
    using_codec_allocator=true) MUST hold.

- Round 36 — **diagnose `IMemInputPin::Receive` trap site** that
  r35 unblocked.  Round 35 closed with the codec successfully
  minting its own allocator + driving `SetProperties + Commit`
  on it, then trapping inside `Receive` with `memory fault at
  0x0000001c (page unmapped)` — the canonical "NULL+0x1c"
  pattern when the codec dereferences a NULL pointer and
  reads the dword at +0x1c off it.
  - **Diagnostic infrastructure** — the production-path
    `Receive` call in `discovery::codec` now catches the trap,
    snapshots `cpu.regs.eip`, every GP register
    (`eax/ecx/edx/ebx/esp/ebp/esi/edi`), the last 16 dwords of
    the guest stack, the last 24 entries of a 4096-deep trace
    ring (compressed to call-site boundaries), and every dword
    in the codec's IMemInputPin object (offsets 0x00..=0xa0).
    All folded into the `Error::other` message so a failing
    test surfaces actionable register state without needing a
    separate `cargo test --features trace` build.
  - **Trap site identified**: codec MPG4DS32.AX, RVA `0x7184`,
    instruction `f3 a7` = `repe cmpsd` (compare 4 dwords).
    Function at RVA `0x7176` is an inlined `IsEqualGUID`:
    `mov esi, ecx; mov edi, &kZeroGuid (= rva 0x26c08, all-zeros);
    mov ecx, 4; xor eax, eax; repe cmpsd; setne al; ret`.
    Caller at RVA `0x2da7` does `mov ecx, [ebx+0x8c]; add ecx,
    0x1c; call IsEqualGUID(ecx, &kZeroGuid)`.  Trap fires
    because `ebx+0x8c` (the `this` of the calling function,
    where `ebx = 0x900ffee0` — a STACK address — meaning a
    stack-allocated codec object) holds NULL at offset 0x8c,
    so `[NULL+0x1c]` faults.
  - **Call chain to trap** (compressed RVAs, oldest first):
    `0x69ab → 0x5e34 → 0x674f → 0x5e5f → 0x69c9 → 0x25a2 →
    0x6fee (IMemInputPin::Receive entry) → 0x70f1 → 0x25ba
    → 0x261a → 0x6473 → 0x6560 → 0x2626 → 0x2da7 → 0x7176
    (IsEqualGUID, traps)`.  Function 0x6fee is the codec's
    own `IMemInputPin::Receive(IMediaSample*)` — confirmed by
    its prolog `mov ebx, [ebp+8]; mov esi, ecx; mov eax, [ebx];
    push &local2; push &local1; push ebx; call [eax+0x14]`
    (the `[eax+0x14]` = slot 5 = `IMediaSample::GetTime`).
  - **Failing semantic**: the codec's stack-local helper struct
    has a field at offset `+0x8c` that was never initialised.
    The codec checks `if (m_pSomething->guid_at_0x1c ==
    GUID_NULL) return E_FAIL;` — this is a defence against
    uninitialised media-type GUIDs.  When `m_pSomething` itself
    is NULL we trap before the comparison can run.
  - **Round-37 candidate**: the actual fix needs to identify
    which initialisation step (likely deeper than what r33-r35
    pre-`Receive` already drives — `JoinFilterGraph + 
    EnumPins + ReceiveConnection + QI(IMemInputPin) + 
    GetAllocator + SetProperties + Commit + Pause + Run +
    GetState`) the codec expects to populate the stack-local's
    `+0x8c` field.  Likely candidates: `IPin::QueryPinInfo` /
    `IPin::ConnectedTo` (currently both return `E_NOTIMPL`
    from our host stubs), or a per-pin `IFilterGraph2`/
    `IMediaSeeking`/`IBaseFilter::QueryFilterInfo` interface
    we haven't minted.
  - +6 tests in `tests/round36_receive_trap_site.rs`
    documenting the trap state, the disassembly of the trap
    site, the bytes at the static GUID literal, the call
    chain, the IMemInputPin field layout, and a smoke check
    that round-30/32/33/34/35's host-allocator path still
    works.  Test count: 541 → 556.
- Round 35 — **register host-side `CLSID_MemoryAllocator` class
  factory** so `mpg4ds32`'s internal
  `CoCreateInstance(CLSID_MemoryAllocator, NULL, _,
  IID_IMemAllocator, &alloc)` (called from inside
  `IMemInputPin::GetAllocator`) succeeds rather than returning
  `CLASS_E_CLASSNOTAVAILABLE (0x80040111)`. Round 34 closed with
  GetAllocator surfacing `0x80040111` because the host had no
  factory for the canonical DirectShow memory-allocator class
  (CLSID `{1E651CC0-B199-11D0-8212-00C04FC32C45}` per
  `axextend.h`); the codec then returned that same HRESULT to our
  upstream filter and the codec-allocator path could never engage.
  - New `crate::com::CLSID_MEMORY_ALLOCATOR` Guid constant.
  - New `crate::com::mint_host_mem_allocator_class_factory`
    helper builds a 5-slot `IClassFactory` vtable (QI / AddRef /
    Release / CreateInstance / LockServer); the CreateInstance
    stub validates the requested IID is `IUnknown` or
    `IMemAllocator`, rejects aggregation with
    `CLASS_E_NOAGGREGATION (0x80040110)` per MSDN, and otherwise
    mints a fresh `HostIMemAllocator` (4-slot pool × 256 KiB
    capacity by default) and writes its address to `*ppv`.
  - `Sandbox::new` now pre-registers this factory under
    `CLSID_MEMORY_ALLOCATOR` in `HostState::com.class_factories`,
    so any codec that calls `ole32!CoCreateInstance` for the
    memory-allocator CLSID gets an immediate `S_OK` + non-NULL
    allocator interface pointer.
  - End-to-end against `mpg4ds32`: `GetAllocator` now returns
    `S_OK` with a non-NULL codec allocator, `SetProperties` and
    `Commit` on it both return `S_OK`, and
    `using_codec_allocator` flips true. The round-34 baseline
    `VFW_E_NOT_COMMITTED (0x80040209)` from `IMemInputPin::Receive`
    is gone; receive now reaches a different (round-36) blocker
    in the codec's downstream sample path.
  - `Sandbox::mint_host_mem_allocator_class_factory` exposes the
    factory mint helper for tests that want a raw factory pointer.
  - Stack-cleanup correctness: registered
    `IClassFactory::CreateInstance` with `arg_dwords=4` (the
    `this` pointer counts as the first stdcall arg). A 3-arg
    registration leaks 4 stack bytes per call, which surfaces as
    a wild EIP after the next codec `ret`.
- Round 34 — **work WITH the codec's own allocator instead of
  fighting our host one.**  Round 33 closed with the diagnosis
  that `mpg4ds32` walks its OWN allocator from inside
  `IMemInputPin::Receive` rather than the `NotifyAllocator`-
  supplied host one we'd Commit'd, returning `VFW_E_NOT_COMMITTED
  (0x80040209)` from every Receive call.  The fix follows the
  canonical DShow allocator-negotiation contract:
  - `SandboxedDshowDecoder::ensure_open` now drives
    `IMemInputPin::GetAllocator(IMemAllocator** ppAllocator)`
    (slot 3, per `axextend.h`) right after the QI for
    `IMemInputPin`.  When the codec returns a non-NULL allocator
    with `S_OK`, we drive `SetProperties(req, actual)` (cBuffers=4,
    cbBuffer=max(w·h·3, 256 KiB), cbAlign=1, cbPrefix=0) then
    `Commit()` on it; on success the codec allocator becomes the
    `receive_frame` source.  When the codec rejects GetAllocator
    (NULL / E_NOTIMPL / VFW_E_NO_ALLOCATOR / observed-empirical
    `0x80040111`), the host-allocator fallback path remains
    intact.
  - `receive_frame` now picks the allocator (codec vs host) based
    on the round-34 negotiation result.  The codec-allocator path
    drives `IMediaSample::GetPointer + SetActualDataLength +
    SetSyncPoint` through the vtable (because the codec's sample
    layout is internal and opaque); the host-allocator path keeps
    the round-30 `media_sample_set_payload` direct-poke shortcut.
  - New public constants in `crate::com`:
    `SLOT_MEMINPUTPIN_GET_ALLOCATOR=3`,
    `SLOT_MEDIASAMPLE_GET_POINTER=3`,
    `SLOT_MEDIASAMPLE_GET_SIZE=4`,
    `SLOT_MEDIASAMPLE_IS_SYNC_POINT=7`,
    `SLOT_MEDIASAMPLE_SET_SYNC_POINT=8`,
    `SLOT_MEDIASAMPLE_GET_ACTUAL_DATA_LENGTH=11`,
    `SLOT_MEDIASAMPLE_SET_ACTUAL_DATA_LENGTH=12`.
  - New public capture surface
    `discovery::CodecAllocatorNegotiation` + accessor
    `discovery::last_codec_allocator_negotiation(codec_id)`
    expose the per-codec `(GetAllocator HRESULT,
    codec_allocator pointer, SetProperties HRESULT, Commit
    HRESULT, using_codec_allocator)` tuple so tests can introspect
    what the codec actually did without spawning a parallel
    sandbox.
  - +12 tests in `tests/round34_dshow_codec_allocator.rs` (3
    integration tests against `MPG4DS32.AX` driving the production
    path + slot-constant unit + host-allocator-fallback unit + 7
    re-runs of the `avi_extractor` unit module).
  - Empirical finding for `mpg4ds32`:
    `IMemInputPin::GetAllocator` returns `0x80040111`
    (`CLASS_E_CLASSNOTAVAILABLE`) — likely the codec's internal
    `CoCreateInstance(CLSID_MemoryAllocator)` failing in the
    sandbox.  The production path correctly falls back to the host
    allocator; bridging this last gap (round 35 candidate) needs a
    host-side `CLSID_MemoryAllocator` class factory so the codec's
    internal allocator construction succeeds.

- Round 33 — **pursue all three round-32 follow-ups: real MP43
  keyframe, `IMediaFilter::GetState` drive, `SetProperties`
  capture.**
  - **A.** New integration test
    `tests/round33_dshow_real_mp43.rs` extracts the real
    MS-MPEG-4-v3 keyframe sample 0 from
    `docs/video/msmpeg4-fixtures/fourcc-MP43/input.avi` (176×144,
    183-byte payload — same bitstream the VfW path decodes
    bit-perfectly) via the existing `common::avi_extractor` walker
    and feeds it into `SandboxedDshowDecoder` through the public
    `oxideav_core::Decoder` trait.  Falls back to the gop-30 /
    DIV3-tagged 352×288 fixture if the explicit-MP43 fixture is
    missing.  The test confirms the path no longer panics and
    surfaces a DShow-pathway diagnostic; the codec currently still
    returns `VFW_E_NOT_COMMITTED` from `IMemInputPin::Receive`
    (suggesting it walks its own internal allocator rather than
    the host-supplied one — round-34 candidate).
  - **B.** `SandboxedDshowDecoder::ensure_open` now drives
    `IMediaFilter::GetState(1000ms, FILTER_STATE*)` immediately
    after `Run(0)` and stashes both the HRESULT and the
    `FILTER_STATE` value into `last_get_state_hr` /
    `last_get_state_value` private fields for diagnostic logging.
    New public constants in `crate::com`:
    `FILTER_STATE_{STOPPED, PAUSED, RUNNING}` (per `strmif.h`
    `FILTER_STATE` enum), `VFW_S_STATE_INTERMEDIATE`,
    `VFW_S_CANT_CUE`.
  - **C.** `HostIMemAllocator::SetProperties` now captures the
    four `ALLOCATOR_PROPERTIES` LONG fields (cBuffers / cbBuffer /
    cbAlign / cbPrefix) plus the `this` pointer into a per-
    `HostState` log via the same static-mutex pattern the round-31
    `host_iface_r31` queue uses.  Surfaced through new
    `Sandbox::{last_set_properties, all_set_properties,
    clear_set_properties_log}` accessors and the public
    `crate::com::AllocatorPropertiesCapture` struct.  Tests can
    now assert exactly what shape a codec asks for.
  - +10 tests (one DShow trait integration test + one host
    SetProperties unit test + one constants smoke test + 7
    re-runs of the avi_extractor unit module).

- Round 32 — **close the DirectShow decode loop end-to-end:
  `IMediaFilter::Run(0)` drive + `HostIMemAllocator::Commit` state
  machine + `IPin::QueryDirection` filter on `first_input_pin`.**
  - **A.** `SandboxedDshowDecoder::ensure_open` now drives
    `IMediaFilter::Pause()` (slot 5) → `IMediaFilter::Run(0)`
    (slot 6) against the codec filter after `NotifyAllocator` so
    the codec transitions out of `State_Stopped` before
    `IMemInputPin::Receive`. Slots are reachable directly via the
    `IBaseFilter` pointer because `IBaseFilter` extends
    `IMediaFilter` (no explicit QI(IID_IMediaFilter) needed).
  - **B.** `HostIMemAllocator` now tracks a per-instance commit
    flag in guest memory (`obj+12`: 0 = decommitted, 1 = committed).
    `Commit()` flips the flag to 1; `Decommit()` flips it back to 0;
    `GetBuffer()` returns `VFW_E_NOT_COMMITTED (0x80040209)` while
    decommitted, regardless of pool state. The newly-minted
    allocator starts decommitted to match real `IMemAllocator`
    semantics; `ensure_open` Commit()s it explicitly before driving
    the first `Receive`. Round-30's GetBuffer-after-mint test was
    updated to drive Commit first.
  - **C.** `first_input_pin` (and the new `pin_with_direction`
    helper used by both `first_input_pin` and `first_output_pin_dshow`)
    now walks every pin via `IBaseFilter::EnumPins → IEnumPins::Next`,
    queries each for `IPin::QueryDirection(PIN_DIRECTION*)`
    (slot 9), and picks the first pin reporting the requested
    direction (`PIN_INPUT = 0` / `PIN_OUTPUT = 1`). This replaces
    the historic "input pins enumerate first" heuristic, which
    `mpg4ds32` violated (its first enumerated pin was non-input,
    causing downstream `EnumMediaTypes` to return `E_NOTIMPL` and
    `ReceiveConnection` to reject every AMT). Non-chosen pins are
    Released on the way out.
  - New public constants in `crate::com`: `SLOT_MEDIAFILTER_{STOP,
    PAUSE, RUN, GET_STATE}` (= IBaseFilter slots — IBaseFilter
    extends IMediaFilter), `SLOT_MEMALLOCATOR_{SET_PROPERTIES,
    COMMIT, DECOMMIT, GET_BUFFER, RELEASE_BUFFER}`,
    `SLOT_MEMINPUTPIN_{NOTIFY_ALLOCATOR, RECEIVE}`,
    `SLOT_PIN_{RECEIVE_CONNECTION, QUERY_DIRECTION,
    ENUM_MEDIA_TYPES}`, `SLOT_ENUMPINS_NEXT`,
    `PIN_DIRECTION_{INPUT, OUTPUT}`, `VFW_E_NOT_COMMITTED`,
    `VFW_E_TIMEOUT`, `VFW_E_NO_ALLOCATOR`. Replaces magic-number
    slot literals throughout `discovery::codec`.
  - New `tests/round32_dshow_run_commit_querydir.rs` — 5 tests:
    decommitted-on-mint allocator rejects GetBuffer; Commit /
    Decommit round-trip toggles the state; the new
    `SLOT_MEDIAFILTER_*` constants alias their `SLOT_BASEFILTER_*`
    siblings; HostIPin output role + input role report distinct
    directions (PIN_OUTPUT / PIN_INPUT); end-to-end DShow trait
    path against MPG4DS32.AX exercises Run+Commit+QueryDir without
    panicking. Test count: 499 → 504.
- Round 31 — **`IPin::EnumMediaTypes` walk + downstream
  `HostIPin::Receive` capture.** New `crate::com::host_iface_r31`
  module mints paired (HostIPin (input role), HostIMemInputPin)
  + HostIBaseFilter + HostIEnumPins; `HostIMemInputPin::Receive`
  re-enters the guest to read `IMediaSample::GetActualDataLength /
  GetPointer / GetTime / IsSyncPoint / GetMediaType` and queues
  the captured bytes onto a per-`HostState` FIFO. New
  `walk_codec_input_pin_amts` drives `IPin::EnumMediaTypes →
  IEnumMediaTypes::Next` against the codec's input pin and
  captures every advertised AMT. `SandboxedDshowDecoder::ensure_open`
  prefers codec-native AMTs over the synth fabrication when any
  surface; falls back to the synth AMT only when every codec-native
  candidate is rejected.

- Round 30 — **two sub-goals: DirectShow IMemAllocator + IMediaSample
  host stubs (sub-goal A) + Indeo / Cinepak fixture-driven trait
  tests + ICM_DECOMPRESS_GET_FORMAT dimension probe (sub-goal B).**
  - **A.** New `crate::com::host_iface` minting helpers
    [`mint_host_mem_allocator`] and [`mint_host_media_sample`]
    (re-exported on `Sandbox`) plus 11 IMemAllocator vtable thunks
    (3 IUnknown + SetProperties / GetProperties / Commit /
    Decommit / GetBuffer / ReleaseBuffer) and 18 IMediaSample
    vtable thunks (3 IUnknown + GetPointer / GetSize / GetTime /
    SetTime / IsSyncPoint / SetSyncPoint / IsPreroll / SetPreroll
    / GetActualDataLength / SetActualDataLength / GetMediaType /
    SetMediaType / IsDiscontinuity / SetDiscontinuity /
    GetMediaTime). The host allocator threads its sample pool
    through a singly-linked list at `obj+8 → sample+32 → …`;
    GetBuffer marks each sample in-use until ReleaseBuffer flips
    the flag back. New `SandboxedDshowDecoder` wires DirectShow
    codecs end-to-end through `make_decoder` (round-29 used to
    return `Err(Unsupported)` immediately): on first
    `send_packet`, drives DllGetClassObject → CreateInstance →
    EnumPins → JoinFilterGraph → ReceiveConnection →
    QueryInterface(IMemInputPin) → NotifyAllocator(host_alloc,
    FALSE) → IMemInputPin::Receive(host_sample) carrying the
    packet bytes. Codec output capture via a downstream
    HostIPin::Receive callback is r31 work — `receive_frame`
    surfaces `Unsupported` carrying the diagnostic + a
    `trace_ring` snapshot for the next round to mine.
  - **B.** New `Sandbox::ic_decompress_get_format` (`vfw32::
    ic_decompress_get_format`) drives `ICM_DECOMPRESS_GET_FORMAT`
    against the codec to recover the output `BITMAPINFOHEADER` —
    used by `SandboxedVfwDecoder::ensure_open` to probe stream
    dimensions when `CodecParameters.{width,height}` are `None`
    (more robust than round-29's hard-error path; callers without
    advance dimension knowledge now decode end-to-end via the
    trait surface). `tests/round30_dshow_and_indeo_cinepak.rs`
    adds 19 new tests (workspace total 492): IMemAllocator pool
    layout + GetBuffer/ReleaseBuffer cycle + GetProperties pool
    walk + QI; IMediaSample GetPointer/GetSize/GetActualDataLength
    + IsSyncPoint round-trip; DShow trait-path constructor +
    diagnostic surface; trait-path keyframe decode for IV31
    (cubes.mov 160×120 through IR32_32.DLL), IV41 (crashtest.avi
    240×180 through IR41_32.AX), IV50 (cat_attack.avi 320×240
    through IR50_32.DLL), and CVID (Cinepak through ICCVID.DLL);
    plus a dim-probe test that drops `CodecParameters` dims and
    confirms `ICM_DECOMPRESS_GET_FORMAT` populates them lazily.

- Round 29 — **`oxideav_core::Decoder` trait wired end-to-end for
  VfW codecs discovered through round-28's auto-discovery path.**
  `SandboxedVfwDecoder` (in `discovery::codec`) now retains the
  `Sandbox` + the `HIC` across `send_packet` / `receive_frame`
  calls and threads the full `ICDecompressQuery →
  ICDecompressBegin → ICDecompress → ICDecompressEnd` lifecycle:
  - `ensure_open()` — lazy on the first `send_packet`. Loads the
    DLL, runs DllMain, opens the codec, runs query+begin against a
    synthesised input `BITMAPINFOHEADER` (FourCC from the
    `DiscoveryRecord`, 24bpp, dimensions from `CodecParameters`)
    + a fixed BI_RGB 24bpp output BIH. Width/height are required
    on `CodecParameters` — round 24 confirmed VfW codecs cannot
    infer dimensions from the bitstream alone.
  - `receive_frame()` — calls `ic_decompress` with
    `ICDECOMPRESS_NOTKEYFRAME` set unless `packet.flags.keyframe`,
    then materialises the codec's bottom-up BGR24 output as a
    top-down `Frame::Video` with `PixelFormat::Bgr24` (the new
    `oxideav_vfw::discovery::output_pixel_format()` helper exposes
    the format choice for downstream wiring).
  - `Drop` — calls `ic_decompress_end` (when begin completed)
    + `ic_close` so the codec's per-instance state and the HIC
    table both unwind cleanly.
  - DirectShow codecs (`Kind::DirectShow`) still return
    `Error::Unsupported` from `make_decoder` — that path needs
    `IMemAllocator` + `IMediaSample` host stubs (round 30+).
  - `tests/round29_decoder_trait_integration.rs` — 16 new tests
    (workspace total 473) covering: BGR24 pixel-format contract,
    `width-is-None` rejection, DirectShow `Unsupported`
    preservation, `codec_id_for` stability, and 4 byte-equality
    checks (`gop-30 / with-skip-mbs / motion-pan / intra-pred-active`)
    that drive 1..3 frames through *both* the new trait path and
    the round-24 manual `Sandbox::ic_decompress` path and assert
    the per-frame BGR24 output is byte-identical (after the
    bottom-up→top-down flip the trait path applies). Confirms the
    trait integration adds no semantic skew vs the manual path.

- Round 28 — **codec auto-discovery at `register()` time.** New
  `src/discovery/` module (~700 LOC) walks a configurable
  discovery path, probes every `*.dll` / `*.ax`, and registers
  one `oxideav_core::CodecInfo` per recognised FourCC into
  `RuntimeContext::codecs`. Each registered entry sits at codec
  priority 200 (after SW codecs at 100 and HW codecs at 10) and
  uses codec id format `vfw_<lowercase-fourcc>_<dll-stem>` to
  avoid collisions when multiple DLLs claim the same FourCC.
  - **Discovery path resolution.** `OXIDEAV_VFW_CODEC_PATH`
    overrides the platform default
    (`$XDG_DATA_HOME/oxideav/codecs/` on UNIX,
    `%LOCALAPPDATA%\oxideav\codecs\` on Windows). Empty
    components in the override list are skipped silently; a
    missing directory is not an error (cleanly registers zero
    codecs).
  - **Probe scope.** Each candidate DLL is loaded into a fresh
    `Sandbox`, `DriverProc` is exercised first via
    `ICOpen('VIDC', candidate)` against a static FourCC sweep
    (`MP43 / MP42 / MPG4 / DIV3 / IV31 / IV41 / IV50 / CVID /
    MJPG`); on miss we fall back to `DllGetClassObject` against
    a small static CLSID list (today: `{82CCD3E0-…}` for
    `MPG4DS32.AX`). FourCCs decoded via `IPin::EnumMediaTypes`
    are deferred to round 29 — DirectShow registrations record
    just the matching CLSID. Anything that doesn't match either
    path is recorded as `Kind::Unsupported` so we don't re-probe.
  - **Cache.** `$XDG_CACHE_HOME/oxideav/vfw-discovery.json`
    (or `$HOME/.cache/oxideav/…` /
    `%LOCALAPPDATA%\oxideav\Cache\…`), keyed by
    `(absolute_path, mtime, size_bytes)`. Atomic writes via
    tempfile + rename; corrupted JSON is treated as a clean
    miss. Cache invalidates correctly on file mtime / size
    change.
  - **Decoder factory.** `DecoderFactory` is a bare `fn`;
    per-codec context (DLL path / FourCC / CLSID) lives in a
    process-wide `OnceLock<Mutex<HashMap>>` keyed by codec id.
    The shared `make_decoder` looks up the matching
    `DiscoveryRecord` at construction time. Per-frame decode
    through the generic `Decoder` trait still leans on the
    existing manual `Sandbox::ic_decompress` API and surfaces
    `Error::Unsupported` for now; round 29 wires the full
    receive_frame path. DirectShow codecs surface
    `Error::Unsupported` with the CLSID in the message so the
    caller knows which filter to drive manually.
  - **New cargo feature `auto-discovery`** (default-on) gates
    the entire FS scan + cache + `log` / `serde` dependency
    tail. Consumers building with `default-features = false`
    get the bare manual `Sandbox` API without the new
    transitive deps.
  - **New tests:** 5 integration tests in
    `tests/round28_auto_discovery.rs` (nonexistent default
    path is clean,
    `OXIDEAV_VFW_CODEC_PATH=/dev/null:/tmp/nonexistent` is
    clean, cache round-trip via `discover()`, cache
    invalidation on mtime change, synthetic `build_minimal_dll`
    classified as `Kind::Unsupported`); 19 unit tests across
    `discovery::{cache, codec, paths, probe}` modules.
- Round 27 — **IFilterGraph + IPin host stubs land; MPG4DS32
  `IPin::ReceiveConnection` reaches `S_OK`.**  Past round 26's
  `VFW_E_NO_TYPES` (`0x80040208`) gate.  Two sub-goals:
  - **A.1 — MEDIASUBTYPE / FORMAT_* probe matrix.**  The new
    `tests/round27_filtergraph_and_subtypes.rs::round27_probe_matrix_against_mpg4ds32`
    walks 12 `(FOURCC, FORMAT_kind)` combinations
    (`MP43`/`mp43`/`MP4S`/`mp4s`/`MPG4`/`MP42`/`DIV3`/`DIVX`/`DX50`
    × `VIH1`/`VIH2`) against `IPin::ReceiveConnection`.  Without
    a valid `pConnector`, every combination returns the same
    `VFW_E_NO_TYPES` — i.e. CheckMediaType is fine with the AMT
    shape and the rejection comes from the connector-direction
    sanity check.
  - **A.2 — IFilterGraph + IPin host stubs.**  New
    `src/com/host_iface.rs` (~440 LOC) registers a family of
    synthetic thunk addresses under the `host-com.host`
    pseudo-DLL; `mint_host_filter_graph` and
    `mint_host_output_pin(amt_addr)` build vtable layouts in
    arena memory whose function-pointer slots dispatch through
    `dispatch_stub`.  HostIFilterGraph: 11 methods, every
    IFilterGraph method `E_NOTIMPL`, `QueryInterface(IUnknown |
    IFilterGraph) → S_OK`.  HostIPin: 18 methods,
    `QueryDirection → PIN_OUTPUT`, `QueryAccept → S_OK`,
    `ConnectionMediaType` copies the staged AMT,
    `EnumMediaTypes` vends a HostIEnumMediaTypes that yields the
    AMT once.  HostIEnumMediaTypes: 7 methods.  `Sandbox` exposes
    `mint_host_filter_graph` + `mint_host_output_pin`.  Drove
    `JoinFilterGraph(host_graph, NULL) → S_OK` then
    `ReceiveConnection(host_pin, MP43 VIH1) → S_OK = 0`.  Round
    27 deliverable.
  - **B (stretch) — IMemInputPin probe after S_OK.** With the
    pin connected, `QueryInterface(IID_IMemInputPin)` returns
    a valid IMemInputPin pointer; `GetAllocator` returns
    `VFW_E_NO_ALLOCATOR` (codec waits for upstream's
    `NotifyAllocator`); `GetAllocatorRequirements` returns
    `E_NOTIMPL` (codec is happy with caller-provided sizing);
    `ReceiveCanBlock → S_OK`.  Trace ring captured 64 EIPs of
    codec internal state advance through the QI / GetAllocator /
    GetAllocatorRequirements / ReceiveCanBlock sequence —
    confirming the codec is now live past the connection
    handshake.  Round 28 stages the host-side IMemAllocator +
    IMediaSample.
  - **Side-bonus — WMVDS32 CLSID hunt (deferred).**  Static
    analysis of `WMVDS32.AX` `.rdata` finds 23 fourcc-base
    `MEDIASUBTYPE_*` GUIDs (`WMV1` / `wmv1` / `MPG4` / `mpg4` /
    `MP42` / `mp42` / `MP43` / `mp43` / `MP4S` / `mp4s` /
    `MSS1` / `mss1` / `Y41T` / `Y42T` / `UYVY` / `YUY2` /
    `Y41P` / `YVU9` / `YV12` / `I420` / `IYUV` / `vids` /
    `auds`) plus standard DirectShow IID / FORMAT_VideoInfo /
    Quartz interface GUIDs — but no unique codec CLSID literal.
    The `82CCD3E1-F71A-11D0-…` CLSID family used for sibling
    MPG4DS32 is not present.  WMVDS32 likely constructs its
    CLSID dynamically; deferred to a round that disassembles
    `DllRegisterServer` (`RVA 0x20D5` in WMVDS32) to pinpoint
    the constructor call site.
- Round 26 — **`user32!CreateWindowExA` cascade stubs +
  IPin::ReceiveConnection probe.** Two sub-goals:
  - **A. user32 cascade stubs.** Many DirectShow filters and
    legacy MS codecs call `user32!CreateWindowExA` during init
    expecting a non-NULL `HWND`. Round 26 hands out synthetic
    `HWND_BASE + n` values (`HWND_BASE = 0xCAFE_0000`) from a
    new `host.hwnd_registry: BTreeSet<u32>` plus
    `host.next_hwnd_index: u32` counter; companion stubs are
    fail-soft so the codec falls through to its headless path.
    Stubs added: `CreateWindowExA` (12 args → synthetic HWND),
    `UpdateWindow` (1 → TRUE), `IsWindow` (1 → TRUE iff in
    registry), `GetMessageA` (4 → 0 / WM_QUIT, zero-fills MSG),
    `DispatchMessageA` (1 → 0), `TranslateMessage` (1 → 0),
    `PeekMessageA` (5 → 0), `PostQuitMessage` (1 → 0). Patched:
    `DestroyWindow` (1 → TRUE, drops from registry — was 0),
    `MoveWindow` (6 → TRUE — was 0). Neither MPG4DS32 nor
    WMVDS32 imports `CreateWindowExA` directly (only msadds32
    does, and that's deliberately not driven through DLL_PROCESS_ATTACH);
    the cascade is staged here so future rounds loading
    wmvds32 / wmv8ds32 / msscds32 through the COM ABI find a
    complete user32 surface ready.
  - **B. IPin::ReceiveConnection probe.** Round-25 reached
    `IBaseFilter::Run = S_OK` and walked
    `IBaseFilter::EnumPins → IEnumPins::Next` to retrieve an
    input pin at `0x6000025C`. Round 26 stages an
    `AM_MEDIA_TYPE` (72 bytes) describing
    `MEDIATYPE_Video / MEDIASUBTYPE_MP43 / FORMAT_VideoInfo`
    with a `VIDEOINFOHEADER` (88 bytes) carrying a
    `BITMAPINFOHEADER` for 320x240 MP43, then drives
    `IPin::ReceiveConnection(pConnector, pmt)` (slot 4) on the
    input pin. With `pConnector = NULL` the codec returns
    `E_POINTER` (0x80004003); with `pConnector = pin` it
    returns `0x80040208` (VFW_E-class — likely needs the filter
    state-machine / IFilterGraph hookup). Logged for round 27;
    not asserted as success. The input pin connection negotiation
    is the round-27 goal.
  - **C. Test surface.**
    `tests/round26_user32_cascade_and_pin_receive.rs` — 5 new
    tests: cascade-registration check, synthetic-HWND lifecycle
    (CreateWindowExA → IsWindow → DestroyWindow → IsWindow),
    HWND counter increment, message-pump zero-fills MSG +
    returns 0, and the IPin::ReceiveConnection probe.
  - 13 new tests overall (8 carry over from
    `tests/common/avi_extractor` because the test crate
    re-imports the helper module). Total: 408 passing tests.
- Round 25 — **DirectShow IBaseFilter scaffolding (Stages 1-5
  all landed against MPG4DS32.AX).** Round 24 closed with the
  verdict that `WMVDS32.AX` and `MPG4DS32.AX` lack a
  `DriverProc` export entirely — they are pure DirectShow
  filters. Round 25 builds the COM/DirectShow ABI surface that
  reaches in through their actual entry points:
  - **Stage 1 (COM scaffolding).** New `src/com/` module
    (~600 LOC) defines `Guid` (with MIDL-string parser +
    16-byte LE round-trip), 11 hardcoded IID constants
    (IUnknown, IClassFactory, IPersist, IMediaFilter,
    IBaseFilter, IPin, IMemInputPin, IEnumPins, IMemAllocator,
    IMediaSample, IFilterGraph), 8 public HRESULT codes
    (`S_OK`, `S_FALSE`, `E_NOINTERFACE`, `E_NOTIMPL`,
    `E_POINTER`, `E_FAIL`, `E_UNEXPECTED`,
    `CLASS_E_CLASSNOTAVAILABLE`), the standard
    vtable-slot-index constants for every method we drive,
    `ComObjectTable` host-side AddRef/Release bookkeeping +
    in-process class-factory cache, and the
    `vtable_ptr` / `method_va` / `vtable_is_plausible` /
    `call_method` / `query_interface` / `add_ref` / `release`
    helpers. All sourced from public MSDN documentation +
    Windows SDK MIDL-generated headers — never the BaseClasses
    sample source.
  - **Stage 2 (DllGetClassObject + IClassFactory).**
    `Sandbox::dll_get_class_object(image, clsid, riid)` stages
    the two GUIDs + an out-pointer slot in arena memory,
    drives the codec's `DllGetClassObject` export, and on
    success registers the returned IClassFactory under
    `clsid` in the host's class-factory cache.
    `MPG4DS32.AX` succeeds with the bundle's MPEG-4 v3
    decoder filter CLSID `{82CCD3E0-F71A-11D0-9FE5-
    00609778EA66}` (returned at guest VA `0x600000B0`).
    `WMVDS32.AX` returns `CLASS_E_CLASSNOTAVAILABLE` against
    the MPEG-4 CLSID — its actual filter CLSID is not yet
    in the candidate list (round-26 follow-up to enumerate).
  - **Stage 2.5 (QueryInterface on the class factory).**
    `Sandbox::query_interface(obj, riid)` succeeds against
    the IClassFactory for `IID_IUnknown` (returns the same
    underlying object pointer per COM ABI rules).
  - **Stage 3 (CreateInstance + IBaseFilter spawn).**
    `Sandbox::co_create_instance(clsid, riid)` consults the
    cache and drives `IClassFactory::CreateInstance(NULL,
    IID_IBaseFilter, ppv)`. **MPG4DS32 spawns a real
    IBaseFilter** at guest VA `0x600000EC`. `QueryInterface`
    succeeds for `IID_IUnknown` (`0x600000E0`),
    `IID_IPersist`, `IID_IMediaFilter`, `IID_IBaseFilter`
    (each returns `0x600000EC` — the filter satisfies the
    inheritance chain through a single tear-off interface).
    `Release` on the chain drops the refcount cleanly to 0.
  - **Stage 4 (IBaseFilter::Run reach goal).**
    `IBaseFilter::Stop` → `S_OK`, `IBaseFilter::Pause` →
    `S_OK`, `IBaseFilter::Run(0)` → `S_OK`. The codec's
    internal state-machine accepts every transition without a
    filter graph attached. `IBaseFilter::EnumPins(ppEnum)`
    also returns `S_OK` with a valid `IEnumPins` pointer
    (`0x60000210`).
  - **Stage 5 stretch (IPin walk).** Driving
    `IEnumPins::Next(1, ppPins, &fetched)` returns one IPin
    pointer at `0x6000025C` with `fetched=1`, and
    `IPin::QueryDirection` reports `dir=0x0` (PIN_INPUT).
    The MPG4DS32 input pin is now reachable from the host.
  - **Bookkeeping additions.** `HostState` grows a
    `com: ComObjectTable` field. `ole32!CoCreateInstance`
    upgraded from a blind `E_NOTIMPL` to a real lookup
    against the class-factory cache (returns
    `CLASS_E_CLASSNOTAVAILABLE` on miss). New ole32 stubs
    `CoInitializeEx` (S_OK) + `CoTaskMemRealloc` (allocs
    fresh slab + copies prior bytes).
  - **Test count: +32 tests** (363 → 395 total). The new
    `tests/round25_directshow_com_scaffold.rs` exercises
    every stage; binary-fixture-gated tests skip cleanly when
    `wmpcdcs8-2001/` is absent.
  - **Reach for round 26.** WMVDS32 needs a different CLSID
    in the candidate list. Pushing a real WMV1/WMV2 sample
    through `IPin::ReceiveConnection → IPin::Receive` is the
    natural next step now that the pin is reachable.
- Round 24 — **Multi-frame MP43 decode + WMV/DirectShow ABI
  verdict + ICGetInfo `cb` size-gate fix +
  user32!UnregisterClassA stub**. Twin main sub-goals plus
  two follow-ups on top of the round-23 I+P unblock:
  - **Follow-up 1 — ICGetInfo against mpg4c32 returns 568
    bytes (was 0).** Round-20 noted that `ICGetInfo` came back
    empty on mpg4c32 even though `ICOpen` + `ICDecompressQuery`
    succeeded. Static disasm of `mpg4c32!DriverProc+0x999..0x99c`
    surfaced the gate:
    ```text
        mov  ebx, 0x238       ; sizeof(ICINFO) = 568
        cmp  [ebp+0x10], ebx  ; lParam2 (cb)
        jb   .return_zero
    ```
    Real `vfw32!ICGetInfo` always passes `sizeof(ICINFO) = 568`
    as `lParam2`; round-20's research call passed `cb=80`,
    which is `< 568`, so mpg4c32 short-returned 0 silently
    (no error indication). Fix: new public constant
    `oxideav_vfw::win32::vfw32::ICINFO_SIZE = 568`, round-20
    test updated to use it, doc-comment on
    `vfw32::ic_get_info` calls out the strict-codec
    requirement, and a new test
    `tests/round24_mp43_multiframe_and_wmv.rs::mp43_get_info_returns_full_icinfo_record`
    verifies mpg4c32 with `cb=568` returns:
    `dwSize=0x238`, `fccType='vidc'`, `fccHandler='MP43'`,
    `dwFlags=0x28`, `dwVersion=1`, `dwVersionICM=0x104`, plus
    `szName="MP43"` (UTF-16LE). Indeo predecessors
    (`IR32_32.DLL`, `IR41_32.AX`) accept `cb < 568` and write
    a truncated header — they're the lenient case; mpg4c32 is
    the strict case host code must conform to.
  - **Follow-up 2 — `user32!UnregisterClassA` +
    `RegisterClassExA` stubs registered.** `msadds32.ax` (the
    audio splitter from wmpcdcs8-2001) imports both for
    hidden-window-class registration in its
    `DLL_PROCESS_ATTACH` / `DLL_PROCESS_DETACH` hooks.
    Fail-soft stubs ship in `src/win32/user32.rs` —
    `RegisterClassExA → 0xC001` (synthetic global atom),
    `UnregisterClassA → 1` (TRUE per MSDN BOOL convention).
    Test
    `tests/round24_mp43_multiframe_and_wmv.rs::user32_unregister_class_a_stub_registered`
    asserts both stubs resolve through the registry. msadds32
    has additional unsatisfied imports
    (CreateWindowExA / GetMessageA / DispatchMessageA / …);
    the audio-splitter window pump is parked off the round-24
    critical path — closing those imports is a future-round
    responsibility per user instruction
    ("wire the stub, don't drive msadds32 through DRV_LOAD or
    anything else").
  - **Sub-goal A — multi-frame MP43 across the larger
    fixtures.** Round 23 only exercised a 2-frame I+P fixture
    (176×144). New test
    `tests/round24_mp43_multiframe_and_wmv.rs` drives mpg4c32
    through the larger `docs/video/msmpeg4-fixtures/` fixtures
    at 352×288: gop-30 (6/6 frames), with-skip-mbs (5/5),
    motion-pan (4/4), intra-pred-active (1/1), qscale-high
    (1/1) — **17/17 frames** total, every one returning
    `ICERR_OK` with > 25% non-zero output. Exercises mb-skip
    + alternate-MV-VLC + `use_skip_mb_code=1` + qscale=16
    paths the round-23 fixture didn't reach. Per-frame cost
    settles at ~5 M emulator instructions on a 352×288
    P-frame (vs. ~8–9 M for the I-frame and the round-23 13 M
    that included codec startup). State carries cleanly across
    six successive `ICDecompress` calls inside one `ICOpen`.
  - **Sub-goal B — WMV1/WMV2 DirectShow ABI verdict.** New
    test probes drive `WMVDS32.AX` and `MPG4DS32.AX` (both
    PE-load green since round 21) through the VfW
    `DRV_LOAD → DRV_ENABLE → DRV_OPEN` sequence with every
    plausible handler 4CC (`WMV1`/`WMV2`/`wmv1`/`wmv2`/`WMVA`/
    `WMVP` for WMV; `MP43`/`mp43`/`DIV3`/`div3` for MPG4DS32).
    Verdict: **neither binary exports `DriverProc`.** Both are
    pure DirectShow filters (`.ax` extension, expose
    `DllGetClassObject` + `IBaseFilter`-derived COM objects);
    the VfW `DriverProc` ABI is fundamentally absent.
    Conclusion: WMV1/WMV2 decode through the wmpcdcs8-2001
    bundle requires a DirectShow IBaseFilter wrapper — a
    different ABI than VfW. Future round candidates: (a)
    implement a minimal IBaseFilter / IPin / IMemAllocator
    wrapper to drive `wmvds32.ax` through DirectShow, or (b)
    source a VfW-shaped WMV decoder (Microsoft shipped
    `wmvcore.dll` with VfW-compat exports in some early WMP
    releases). Round-23 mpg4c32 path (a real VfW driver, not
    a DirectShow filter) remains the project's MSMPEG4-family
    decode story.
  - **Pivot probes — matrix delta investigation.** Same test
    file runs `ICDecompressQuery` against five YUV output
    candidates (YV12 / I420 / IYUV / YUY2 / UYVY) to test
    whether mpg4c32 will hand back its native YUV frame and
    bypass its internal BGR converter. Verdict: every YUV
    candidate returns `0xfffffffe` (`ICERR_BADFORMAT`); the
    codec only honours BI_RGB output via this VfW surface.
    The round-23 ~12 dB matrix delta vs ffmpeg is therefore
    a property of mpg4c32's internal BGR converter and would
    need either a disasm-driven mirror of its coefficients or
    a host-side post-processor — deferred to round-25+.
    Helper module ships a clean-room BT.601 limited-range
    YUV4:2:0 → BGR24 converter (transcribed from BT.601-7
    Annex 1) plus self-consistency unit tests
    (`bt601_yuv_to_bgr_helper_handles_solid_blue` /
    `_handles_grayscale_ramp` / `_psnr_self_consistency`),
    ready for the round-25 host-side renderer when mpg4c32
    is rerouted (or replaced) to surface YUV.
  - **ICINFO_SIZE strict-codec gate.** New
    `vfw32::ICINFO_SIZE = 568` constant + `mpg4c32`'s strict
    `cmp [ebp+0x10], 0x238 / jb .return_zero` gate at
    `mpg4c32!DriverProc+0x999..0x99c` documented inline. The
    round-20 experimental `ICGetInfo(cb=80)` call hit that
    gate and the codec returned 0 bytes silently; the round-20
    test now passes `cb = ICINFO_SIZE` so the full 568-byte
    identity card lands. New
    `mp43_get_info_returns_full_icinfo_record` unit test
    asserts the populated fields:
    `dwSize=0x238 / fccType='vidc' / fccHandler='MP43' /
    dwFlags=0x28 / dwVersion=1 / dwVersionICM=0x104`.
  - **user32 stubs `RegisterClassExA` + `UnregisterClassA`.**
    The MS-Audio splitter `msadds32.ax` (sibling of
    `mpg4c32.dll` in wmpcdcs8-2001) imports both for hidden
    window-class registration in its DllMain hooks. Round 24
    ships fail-soft stubs (`RegisterClassExA → 0xC001`,
    `UnregisterClassA → TRUE`) so those user32 import slots
    resolve at PE-load time. The audio splitter remains parked
    off the critical path; the stubs are scoped to PE-load
    surface only and registered in the user32 module.
- Round 23 — **MSMPEG4 v3 ffmpeg-oracle keyframe cross-check +
  I+P 2-frame decode**. Round 22 decoded the MP43 keyframe and
  asserted "any non-zero output". Round 23 raises the bar:
  - **Sub-goal A — bit-exact / PSNR oracle.** New test
    `tests/round23_mp43_pframe_and_oracle.rs::mp43_keyframe_matches_ffmpeg_oracle_psnr`
    spawns `ffmpeg -i fourcc-MP43/input.avi -frames:v 1
    -pix_fmt bgr24 -f rawvideo -` as a black-box validator and
    compares mpg4c32's BGR24 output against ffmpeg's. Bit-exact
    when buffers match; otherwise computes PSNR and asserts
    `>= 30 dB`. Today's run reports **PSNR 42.90 dB** on the
    solid-blue 176×144 keyframe — a comfortable margin above the
    floor. The drift is the YUV→BGR conversion-matrix difference
    between mpg4c32's internal converter (output bytes
    `ff 02 04`) and ffmpeg swscale (`ff 01 01`) — visually
    indistinguishable, no structural decoder mismatch. Skipped
    gracefully when ffmpeg is not on `PATH`.
  - **Sub-goal B — sequential I + P decode.** New test
    `tests/round23_mp43_pframe_and_oracle.rs::mp43_i_plus_p_two_frame_decode`
    drives mpg4c32 through the
    `i-frame-then-p-frame-176x144/input.avi` fixture (a
    `-vtag DIV3` 2-frame I+P encode whose elementary bitstream
    is plain MSMPEG4 v3, accepted by the codec when the host
    side opens with `fccHandler='MP43'`). Both frames return
    `ICERR_OK`; the I-frame writes 67 639 / 76 032 non-zero
    bytes and the P-frame writes 67 698 / 76 032, confirming
    mpg4c32 maintains its reference-frame state across calls.
    The P-frame consumes 1.13 M emulator instructions (vs.
    13 M for the keyframe), driven by the ~100-byte P-frame
    bitstream in the fixture.
  - **Sub-goal C — state-field audit.** New test
    `mp43_state_field_audit` snapshots `[driver_id +
    0xa0..0xc8]` and `[driver_id + 0x15b0..0x15c4]` before /
    after `ICDecompressBegin` / `ICDecompress`, confirming the
    round-22 wrapper-handshake plant lands intact in the field
    range the BEGIN handler probes. Findings:
    * `[+0xa4]` — codec writes `01 00 00 00` during BEGIN
      (frame-state-ready sentinel).
    * `[+0xb4..+0xc8]` — round-22 wrapper-handshake plant
      survives BEGIN (sentinel `1u32` at `+0xb4`, GUID
      `b4c66e30-0180-11d3-bbc6-006008320064` at `+0xb8..+0xc8`).
    * `[+0x15b0..+0x15c4]` — round-22 disasm flagged this as
      a copy target at `mpg4c32!DriverProc+0x2b41`. Round-23
      audit shows it stays zero through both BEGIN and the
      keyframe DECOMPRESS — the relocation path does NOT fire
      under the current wrapper plant. No additional planting
      required.
- Round 22 — **MSMPEG4 v3 ICDecompressBegin + first keyframe
  decode unblock**. Round 21 closed two DRV_OPEN gates and the
  codec advanced through `ICOpen` / `ICDecompressQuery`, but
  `ICDecompressBegin` still returned `ICERR_INTERNAL` (`-100`).
  - **Sub-goal A — root-cause the v3-only ICDecompressBegin
    gate.** Static disasm + a research test
    (`tests/round22_decomp_begin_trace.rs`) traced the failure
    to `mpg4c32!DriverProc+0x14e2` at `0x1c2034dc..0x1c2034f0`:
    when DRV_OPEN tagged the per-instance state with
    `[esi+0x18]=3` (i.e. `MP43`), the begin path probes
    `state[+0xb4..+0xc8]` for a 20-byte `{ DWORD == 1, 16-byte
    GUID }` record. The 16-byte GUID at `mpg4c32!.text:0x1128`
    decodes as `b4c66e30-0180-11d3-bbc6-006008320064` —
    a private wrapper handshake (DirectShow / DMO codec
    factory) that real WMP populates before invoking the
    codec. No public ICM_* message writes those fields.
    `vfw32::ic_decompress_begin` now plants the wrapper's
    contribution directly at `[driver_id + 0xb4..0xc8]` for
    instances DRV_OPEN tagged as v3 (gated on `fcc_handler ∈
    { MP43, mp43, MP42, mp42, MPG4, mpg4 }` + a runtime
    `[+0x18] == 3` re-check). After the fix:
    `ICDecompressBegin → ICERR_OK`,
    `ICDecompress(keyframe, BI_RGB 24bpp) → ICERR_OK`, output
    buffer populated (76032 bytes for the 176×144 fixture).
  - **Sub-goal B — five new x87 D9 reg-form sub-forms** in
    `src/emulator/isa_fpu.rs`: FSIN (`D9 FE`), FCOS (`D9 FF`),
    FPREM (`D9 F8`), FSCALE (`D9 FD`), and a re-located
    FRNDINT (`D9 FC` → `(reg=7, rm=4)`; round 21 had it at the
    wrong `(6, 4)` slot). The MSMPEG4 v3 begin path uses
    FSIN/FCOS to populate the IDCT trig tables; without these
    the trace trapped immediately after the GUID gate cleared.
- Round 22 sentinel tests:
  * `tests/round22_decomp_begin_trace.rs` — research instrument;
    drives `ICDecompressBegin` with `Cpu::trace_ring(256)` +
    `Cpu::visited_eips()` enabled, dumps the EIP path + which
    fragment of `mpg4c32!DriverProc+0x14e2` was reached.
  * `tests/round21_mp43_decompress.rs` — sub-test
    `mp43_keyframe_decompress_through_real_codec` now asserts
    `ICDecompressBegin` returns 0 and `ICDecompress` returns 0
    (was descriptive-only in round 21).

- Round 21 — **x87 FPU executor + MSMPEG4 v3 DRV_OPEN unblock**.
  Round 20 left mpg4c32's `ICOpen('VIDC','MP43')` returning
  hic=0 because the abbreviated CRT-startup DllMain bailed at
  the first FPU instruction (the static-ctor table walked by
  `_initterm` contained `dd 05 88 18 20 1c` — `FLD QWORD
  […]`), leaving the codec's stored DllMain pointer at
  `[0x1c2ae55c]` NULL and the handler table uninitialised.
  Sub-goal A roots out two distinct gates; sub-goal B
  finishes the DirectShow-filter PE-load pass.
  - **Sub-goal A1 — x87 FPU lights up** (`src/emulator/isa_fpu.rs`,
    ~700 LOC). New `FpuState` (eight `f64` ST(i) slots + TOP +
    SW with C0..C3 condition codes) attached to `Cpu`; new
    dispatcher routes every `0xD8..=0xDF` form. Memory-form
    coverage: FLD/FST/FSTP m32/m64, FLD m80, FILD/FIST/FISTP
    m16/m32/m64, FADD/FSUB/FMUL/FDIV/FDIVR/FCOM/FCOMP across
    all four operand sizes (single, double, i32, i16),
    FLDCW/FNSTCW/FLDENV/FNSTENV. Reg-form coverage: FLD ST(i),
    FXCH, FCHS, FABS, FTST, FLD1/FLDPI/FLDZ/FLDL2T/FLDL2E
    /FLDLG2/FLDLN2, FRNDINT, FSQRT, FNCLEX, FNINIT, FFREE,
    FUCOM/FUCOMP, FNSTSW AX, FCOMPP, the eight FADDP/FMULP/
    FSUBP/FSUBRP/FDIVP/FDIVRP variants. After landing, the
    abbreviated CRT entry now runs **85 instructions** to
    DllMain return (was 45 before, returning 0); the second
    `_initterm` table entry (`0x1c228546: e9 00 00 00 00 dd
    05 …` — load + store of a global double constant) executes
    cleanly, and the codec's `[0x1c2ae55c]` stored-DllMain
    pointer is populated. Real CRT-init signals follow:
    `kernel32!DisableThreadLibraryCalls` is now invoked by
    the user's stored DllMain.
  - **Sub-goal A2 — `vfw32::ic_open` lower-cases ICOPEN
    fccType / fccHandler** (`src/win32/vfw32.rs`). The
    Microsoft codec checks `cmp dword [ebx+4], 'vidc'`
    (lower-case `mmioFOURCC('v','i','d','c')` — the
    canonical `vfw.h ICTYPE_VIDEO`); Indeo predecessors did
    not check fccType at all so previous tests passed
    `b"VIDC"` verbatim. Real Win32 `vfw32!ICOpen`
    canonicalises the user-supplied 4CC to lower case before
    staging the ICOPEN block; round 21 mirrors that. After
    the lower-case fix, **`ICOpen('VIDC','MP43')` returns
    `hic = 0x1`** (was `0`); 206 instructions of DriverProc
    then run end-to-end through `DRV_LOAD` + `DRV_ENABLE` +
    `DRV_OPEN`, allocating per-instance state via
    `operator new`, copying the `MP43` handler tag into a
    codec-local slot, and returning a non-zero driver_id.
    `ICDecompressQuery(input=MP43, output=BI_RGB 24bpp)`
    returns `ICERR_OK`; `ICDecompressBegin` returns
    `ICERR_INTERNAL` (-100) — the next-blocker for
    bit-perfect decode but past the round-21 reach goal of
    "DRV_OPEN unblocked + ICOpen returns non-zero hic".
  - **Sub-goal B — `mpg4ds32.ax` + `wmvds32.ax` PE-load
    closes** with three new `msvcrt` stubs:
    * `_onexit(_onexit_t func)` — record nothing, return
      `func` (success per MSDN).
    * `__dllonexit(_PVFV func, _PVFV** pbegin, _PVFV** pend)`
      — same shortcut.
    * `sprintf(buf, fmt, ...)` — supports `%s %d %i %u %x %X
      %c %p %%` plus width / precision / flag modifiers.
    Both DirectShow filters now `Sandbox::load()` cleanly
    (65 imports, 0 missing); image_base 0x1c400000, four
    exports each.
- Round 21 sentinel tests:
  * `tests/round21_fpu_smoke.rs` — seven hand-built code
    sequences covering FLD m32/m64 + FADD m32 + FILD/FISTP
    + FNSTSW AX + FXCH + FLDCW/FNSTCW round-trip.
  * `tests/round21_dsax_load.rs` — both DirectShow
    filters' `Sandbox::load` closure.
  * `tests/round21_mp43_decompress.rs` — drives `ICOpen
    + ICDecompressQuery + ICDecompressBegin +
    ICDecompress` against the `fourcc-MP43/input.avi`
    fixture. The `mp43_drv_open_returns_nonzero_hic`
    sub-test asserts `hic != 0` (the round-21 reach
    gate); `mp43_keyframe_decompress_through_real_codec`
    runs the rest of the chain end-to-end without
    asserting on the bit pattern (deferred to a future
    round once `ICDecompressBegin`'s remaining
    `ICERR_INTERNAL` is rooted out).

- Round 20 — **MMX kernels dispatch + MSMPEG4 v3 PE-load
  unblock**, two parallel sub-goals.
  - **Sub-goal A — `[ebp-8]` MMX-enable gate localised to a
    registry probe.** Round 19 left the codec's "use MMX
    kernels" decision flag at `[0x1c4a9a38] = 0` because
    `[ebp-8]` was never written. Round 20 disassembled
    `IR41_32.AX` file_off 0x319a0..0x31b50 and identified
    that `[ebp-8]` is set to 1 iff
    `RegOpenKeyExA(HKLM, "HARDWARE\DESCRIPTION\System\FloatingPointProcessor",
    0, KEY_READ, &hKey)` returns ERROR_SUCCESS. Real Win9x
    /NT machines unconditionally have that key. The
    advapi32 stub now returns ERROR_SUCCESS for the
    `FloatingPointProcessor` path (`key_exists_synthetically()`).
    After the fix, the codec's `[0x1c4a9a38]` reaches 1 and
    MMX kernels run end-to-end:
    * `indeo5.avi` 320×240 IV50: 1.5M MMX dispatches/frame ×
      8 frames = **11.5M total**.
    * `Educ_Movie_DeadlyForce.avi` 240×180 IV50: 5.99M.
    * `miss_congeniality_cryptedindeo5.avi` 640×352 IV50:
      **42.1M**.
    * `indeo41.avi` 320×240 IV41: 138/1032 MMX-byte VAs reach
      decoder execution (vs 0 pre-round-20). 8/8 frames OK.
  - **Group-2 RCL/RCR (reg=2/3) implemented** in `C0/C1/D0/D1
    /D2/D3` r/m8 and r/m32 forms. The codec uses RCL on the
    MMX path; round-19 trapped on `0xD1 0xD1` (`RCL ECX, 1`)
    as soon as the use_mmx flag was set.
  - **Sub-goal B — 13 mpg4c32.dll PE-load stubs.**
    Per `docs/winmf/winmf-emulator.md` §"Milestone 3.1":
    * `kernel32!{CreateEventA, CreateThread, SetEvent,
      SetThreadPriority, ResumeThread, MulDiv,
      GetProfileIntA}` — synchronous-thread + priority +
      classic-Win32-utility surface.
    * `msvcrt.dll` — new module (`src/win32/msvcrt.rs`):
      `??2@YAPAXI@Z` (operator new),
      `??3@YAXPAX@Z` (operator delete), `_except_handler3`,
      `_initterm`, `_purecall`, `malloc`, `free`. All cdecl.
      `_initterm` re-enters the run loop via `call_guest`
      to invoke each non-null fn-ptr in the table.
    * `user32!{GetScrollPos, SetScrollPos, SetScrollRange}`
      — fail-soft zero-return (UI vestige, not reached on
      decode path).
    * `winmm!GetDriverModuleHandle` — returns
      `host.primary_module_base`.
  - **`Registry::register_data` data-import channel.**
    `msvcrt!_adjust_fdiv` is a 4-byte data symbol, not a
    function. The codec dereferences the IAT slot value
    (`mov reg, [iat]; mov reg, [reg]`) — putting a thunk
    there crashes on the second deref. We pre-reserve a
    4 KiB R/W region at `0x70100000` for data imports;
    `Sandbox::new` seeds each slot with its registered
    `initial` value; the loader's IAT-resolve sees the
    data slot as the registered "thunk" address and patches
    accordingly.
  - **`Sandbox::call_dll_main` falls back to PE
    `AddressOfEntryPoint`** when no `DllMain` named export
    is present. mpg4c32 only exports `DriverProc`; its
    DLL_PROCESS_ATTACH path is the PE entry (typical CRT
    `_DllMainCRTStartup`).
  - Test suite: `tests/round20_mpg4c32_load.rs` (3 tests
    asserting imports inventory empty, `Sandbox::load`
    succeeds, and DRV_LOAD → ICOpen reaches DriverProc).
    The mpg4c32.dll bytes are read from
    `docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/`;
    tests skip with a stderr note when the docs subtree
    isn't pulled.
  - Test count delta: +5 (from 290 → 297). All round-19
    instruments + every prior real-codec pipeline stay
    green.

- Round 19 — **Lead A: trace-coverage analysis identifies the
  EFLAGS.ID-bit gap as the root cause of zero-MMX-dispatch in
  rounds 12..17.** The crate's MMX module
  (`src/emulator/isa_mmx.rs`, ~1007 LOC, ~50 opcodes) was
  semantically validated by its 19 unit + 13 emulator step
  tests, but rounds 12..17 multi-frame decode pipelines
  reported `mmx_dispatch_count = 0` for every IV31/IV41/IV50
  fixture across the corpus despite the IR41/IR50 binaries
  containing 1094 / 2518 `0F D0..FF` MMX-arithmetic byte
  patterns + 2 / 2 `0F A2` CPUID instructions in their
  executable sections (round 17 byte-scan finding).
  - **Cpu::track_visited_eips + Cpu::visited_eips: BTreeSet<u32>**
    — round-19 instrument (~10 LOC delta in `src/emulator/isa_int.rs`).
    `enable_visited_eip_tracking()` arms a per-instruction probe
    in [`Cpu::step`] that inserts every distinct entry-EIP into
    a sorted set; `take_visited_eips()` drains it. Memory cost is
    O(unique instruction addresses), not O(total instructions) —
    a 20-million-instruction IV41 decode visits ~11 K unique
    EIPs, well within a single-run BTreeSet. The set lets a
    research test answer "did the codec ever step at this RVA?"
    via a `BTreeSet::contains(&va)` lookup.
  - **`tests/round19_mmx_dispatch_analysis.rs`** (~570 LOC) —
    drives `indeo41.avi` (IV41, 320×240) through `IR41_32.AX`
    AND `cat_attack.avi` (IV50, 320×240) through `IR50_32.DLL`
    with unique-EIP tracking on, then computes the
    set-difference between MMX-byte VAs in each binary's
    executable section and the visited-EIP set. Output is the
    full inventory of CPUID sites (preceding 64 B + following
    96 B for each, so the gating branch is visible directly in
    the test stderr) plus first/last 5 unreached MMX bytes.
  - **Round-19 finding: EFLAGS.ID bit (bit 21) was missing from
    `Flags::pack()` / `Flags::unpack()`.** Both Indeo binaries'
    DRV_LOAD-time CPUID-detection runs the canonical Intel-SDM
    §3.4.3.4 toggle test:
      ```
      pushfd ; pop eax        ; baseline EFLAGS
      mov ebx, eax
      xor eax, 0x200000        ; toggle ID bit
      push eax ; popfd         ; load toggled EFLAGS
      pushfd ; pop eax         ; read back
      xor eax, ebx ; and eax, 0x200000   ; isolate diff
      jz <skip-cpuid-block>    ; if bit didn't toggle, no CPUID
      ```
    Pre-round-19 our `Flags::pack` always returned a constant
    value over the modelled bits — bit 21 was simply not in
    the layout. The toggle round-tripped as a no-op, the diff
    was always 0, the `jz` always taken, and the entire
    feature-bit detection block (including the `0F A2` CPUIDs
    AND the `mov [...], <mmx-flag>` writes that follow) was
    skipped. Cause was identical in IR41 and IR50.
  - Fix: `regs::Flags::id: bool` packed into bit 21 of
    `pack()` / `unpack()`. With the round-trip preserved, both
    Indeo binaries' CPUIDs now reach (`reached=true` / 2 of 2)
    in `tests/round19_mmx_dispatch_analysis.rs`.
  - Companion fix: `0x0F 0x31 RDTSC` handler. After the
    EFLAGS-toggle gate cleared, IR50's DRV_LOAD path advanced
    further and tripped on a previously-unimplemented opcode
    at `eip=0x10001A98` — the codec micro-benchmarks two
    candidate kernels with `rdtsc / call kernel / rdtsc`. We
    synthesise the time-stamp counter from `instr_count >> 1`,
    so two consecutive RDTSC calls separated by N integer
    instructions report a delta of floor(N/2) — monotonic but
    not real-clock-tied. Implementation in
    `src/emulator/isa_int.rs::dispatch_0f`.
  - Companion change: CPUID leaf 1 EAX bumped from family 5
    model 4 (Pentium MMX) to family 6 model 3 (Pentium II
    Klamath). The IR41 / IR50 dispatchers run a
    `cmp ebx, 0x600` family discriminator that routes
    family-5-or-lower CPUs to the integer path even when MMX
    is reported. Pentium II is the lowest pre-SSE family-6
    chip that still supports MMX. CPUID.EDX additionally
    advertises CMOV (bit 15), already implemented since
    round 5; SSE / SSE2 stay off because we don't have a
    16-byte SIMD register file.
  - **Reachability outcome (round 19 finding for round 20)**:
    With the four fixes (ID-bit, RDTSC, family/model, MMX
    bit), CPUID is now reached 2/2 in both IR41 and IR50.
    The codec's global feature-flag write `[0x1c4a9a54] =
    0x800000` (raw MMX mask) succeeds — verified by the
    test's post-decode MMU snapshot. **However the
    "use MMX kernels" decision flag at `[0x1c4a9a38]` stays
    at 0**: the codec's CPUID-detection routine combines the
    MMX bit AND a local `[ebp-8] != 0` check (some caller-
    provided per-instance enable flag we have not yet
    located) before setting the global "MMX is on" sentinel
    that the `ICDecompress` per-frame dispatcher consults.
    Therefore MMX-byte reachability stays at 0/1032 for
    IR41 and 0/2442 for IR50 even with all CPUID gates
    cleared — the round-13 MMX module remains correct
    semantics waiting for round 20 to localise the
    `[ebp-8]` source.
  - **Round-20 plan (queued)**: Use the `trace` Cargo feature
    (round 18) to install a watchpoint on `[0x1c4a9a38]` and
    capture the call-stack of every write attempt. The
    `[ebp-8]` value is plausibly:
    1. A registry value the codec reads via
       `RegQueryValueExA` (we'd need a new fixture for the
       Intel-Indeo registry layout in our `advapi32` stub).
    2. An environment variable like `INDEO_FORCE_MMX=1`
       which our `kernel32` GetEnvironmentStrings stub
       returns empty for.
    3. A `DRV_LOAD` lparam from a vfw32-side caller we don't
       currently mimic.
    The trace will distinguish these. If the gate is config-
    based, round 20 either supplies the right config or
    flips the conditional gate via a memory-write hook.
  - Three new lib unit tests pin the round-19 contract:
    `id_flag_round_trips_through_pack_unpack` (regs.rs);
    `pushfd_popfd_toggles_id_bit_for_cpuid_detection` +
    `rdtsc_returns_monotonic_counter_in_edx_eax` +
    `cpuid_leaf_1_reports_pentium_ii_with_mmx` (isa_int.rs).
    Plus 2 integration tests
    (`ir41_mmx_byte_reachability_during_iv41_decode` +
    `ir50_cpuid_reachability_during_iv50_decode`) recording
    the set-difference inventory.

## [0.1.0](https://github.com/OxideAV/oxideav-vfw/compare/v0.0.2...v0.0.3) - 2026-05-08

### Other

- Round 18: trace Cargo feature for the RE instrumentation surface

## [0.0.2](https://github.com/OxideAV/oxideav-vfw/compare/v0.0.1...v0.0.2) - 2026-05-08

### Fixed

- update IV5 driver bundle URL path video/windows → codecs/windows

### Other

- Round 17 — corpus byte-scan + larger IV41 fixture + LIST rec walker
- Round 16 — multi-frame IV41 sequence + OpenDML AVI 2.0 walker
- Round 15 — IV41 (Indeo 4) decode through IR41_32.AX::DriverProc
- Round 14 — multi-fixture IV50 decode + IR41_32.AX surface probe
- Round 13 — MMX ISA + sequential P-frame decode through IR50_32.DLL
- Round 12 — IR50 cat_attack first keyframe decodes to ICERR_OK + RGB24 pixels
- Round 11 — DRV_LOAD + DRV_ENABLE plumbing for first ICOpen
- Round 10 — 0x66-prefix honored across the integer ISA + x87 CW shadow
- Round 9 — fix 0x66 (operand-size override) on MOV; IR50_32.DLL ICOpen passes
- Merge branch 'master' into wip/round8
- Round 7 — "Real IV31 keyframe decode through cubes.mov + MMX scaffold"
- Round 6 — "ICDecompress* against Intel IR32_32.DLL"
- Round 5 — "DllMain + ICOpen + ICGetInfo + ICClose against Intel IR32_32.DLL"
- reverse-engineering aid as co-equal goal + Trace mode in CHANGELOG
- Round 4 — "Close the 49 round-3 import gaps"
- Round 3 — "Real-codec smoke against Intel IR32_32.DLL"
- switch fixture story to on-demand HTTPS fetch (no local DLLs)
- Round 2 — "Decode one Cinepak frame" milestone
- Round 1 — "Load + DllMain + clean exit" milestone

### Added

- Round 18 — **`trace` Cargo feature for the
  reverse-engineering instrumentation surface (task #625
  resolved).** New `trace` feature (default OFF) plus a
  `trace-exec` sub-feature gates the JSONL probe tape
  documented in `docs/winmf/winmf-emulator.md` §"Trace mode".
  With the feature off, every `#[cfg(feature = "trace")]`
  call site compiles to nothing — compatibility-only consumers
  pay zero hot-path cost. With the feature on, four event
  flavours land on a sink configured via
  `OXIDEAV_VFW_TRACE_FILE=<path|2>` env var or
  programmatically through [`Sandbox::set_trace_sink`]:
  * `kind=win32_call` — every `dispatch_stub` invocation
    captures `(dll, name, args, ret, eip)` from the guest
    stack at call time + return value.
  * `kind=mem_write` / `kind=mem_read` — programmable
    watchpoints via [`Sandbox::watch(addr, size, mode)`]
    where `mode = WatchMode::{Write,Read,Both}`. Linear-scan
    inside MMU `load{8,16,32,64}` / `store{8,16,32,64}`;
    overlapping watchpoints fire independently.
  * `kind=exec` — per-instruction trace gated on the
    `trace-exec` sub-feature AND
    `Sandbox::set_exec_trace(true)`; carries first-byte
    SDM-style mnemonic hint + 8-register snapshot.
  * `kind=trap` — emitted unconditionally when `trace` is on
    and the run loop bubbles up a `Trap` / `Win32Error` /
    `PeError`. The most informative event when something
    goes wrong.
  Sink is wrapped in `RefCell<Option<Box<dyn Write + Send>>>`
  on the [`crate::trace::TraceState`] struct owned by the
  [`Mmu`], so the immutable-borrow MMU load paths can still
  emit through the same code path as the mutable-borrow
  store paths. JSONL schema mirrors the `oxideav-magicyuv`
  / `oxideav-tta` `--features trace` emitters; `jq`-line
  greppable. Estimated ~1–2 KLOC budget per the design doc;
  shipped at ~470 LOC src + 130 LOC test (round-18 trace
  module + MMU/dispatch hooks + `tests/round18_trace_feature.rs`,
  4 new integration tests). Round-2 work tracked: GDB
  Remote Serial Protocol server (gdbstub-based) wraps these
  primitives for interactive driving.
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
