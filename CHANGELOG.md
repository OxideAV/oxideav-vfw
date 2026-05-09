# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
