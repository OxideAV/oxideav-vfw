# oxideav-vfw

Pure-Rust 32-bit x86 emulator + PE loader + Video for Windows host
that lets [oxideav](https://github.com/OxideAV/oxideav-workspace)
delegate decoding (and eventually encoding) to legitimately-licensed
Windows codec DLLs on **any** platform — Linux, macOS, FreeBSD,
Windows itself. The codec never executes on the host CPU; it runs
through a software-interpreter sandbox.

## Status

**Round 54 — AVI 1.0 muxer for vfw-encoded MSMPEG4 v3 output +
`ffmpeg` cross-decode validation lights up green.**  Round 51
produced raw MSMPEG4 v3 elementary bytes that self-roundtrip at
27.83 dB PSNR through the same `mpg4c32.dll` decode path.  Round
54 validates the bytes through a SECOND independent decoder:
inline AVI 1.0 muxer (no `oxideav-avi` dev-dep, raw byte
construction against the Microsoft AVI RIFF File Reference +
`winsdk-10/Aviriff.h`) wraps the encoded bytes; `ffprobe` accepts
the AVI; `ffmpeg -f rawvideo -pix_fmt bgr24` decodes all 5 frames
cleanly (380160 bytes = 5 × 176 × 144 × 3); `mpv --vo=null`
probe rc=0.  Mean PSNR-BGR24 across 5 frames = 20.86 dB comparing
ffmpeg's output (vertically flipped to BMP bottom-up) to our
original BGR24 input — consistent with `quality=5000` lossy
behaviour.  Headline: ffmpeg successfully decoded our codec's
encoded bytes end-to-end, providing independent confirmation that
our encode pipeline produces valid MSMPEG4 v3 bitstreams.  See
`tests/round54_avi_wrap_ffmpeg_decode.rs`.

**Round 53 — P-frame quality-regime probe: mpg4c32 clears the
keyframe flag for non-keyframe requests on differing content but
emits a P-frame that's LARGER than the corresponding I-frame
(P/I = 1.386 invariant across `quality ∈ {1000, 2000, 3000, 5000,
8000}`) under the bare VfW path.**  Round 51 found that at
`quality=5000` the codec emits keyframes for both I and P-tagged
frames when content is *identical* (frame 0 == frame 1).  Round
53 probes truly differing content (frame 1 = frame 0 shifted
right by 8 pixels) across a quality sweep.  Finding: the codec
DOES clear the keyframe flag for every P-frame request (so it
acknowledges the request) but the residual + new-content cost
together exceed the intra-only I-frame cost at every quality
level we probed.  Real P-frame compression (P < I) under this
codec build may require richer motion estimation (DirectShow
encode path) or a fixture with greater temporal redundancy.  See
`tests/round53_pframe_quality_probe.rs`.

**Round 52 — `msvcrt!_ftol` real impl advances `msadds32.ax`
PE-load past the splitter's CRT FP-truncation edge.**  Round 50
wired `_beginthreadex` and pinned the next splitter blocker as
`_ftol`; round 52 implements the real x87-to-i32 truncate helper
(`long __cdecl _ftol(double)` — caller emits `FLD qword ptr [arg]`
before the CALL, the stub reads `ST(0)`, truncates toward zero,
pops the x87 slot, and returns the i32 in `eax`).  Unlike the
r48/r50 fail-soft pair (`_endthreadex` / `_beginthreadex`, both
never actually invoked on the decode path we drive), `_ftol` IS
called from filter-coefficient init paths and needed a real impl
— a constant 0 or wrong-sign truncation would scramble every
conversion of a precomputed float coefficient back to the i32 the
splitter's FIR loops expect.  Saturation: NaN → `i32::MIN` (the
MSVC "indefinite integer" sentinel `0x8000_0000`), `f >= 2^31` →
`i32::MAX`, `f <= -2^31-1` → `i32::MIN`.  Registered with
`arg_dwords = 0` — the *argument* is on the x87 stack and not on
the regular cdecl stack at all.  After round 52,
`Sandbox::load("msadds32.ax")` advances past `_ftol` and now
stops at the next unresolved import: `msvcrt!rand`.  No
DirectShow / VfW decode metric changes — `MPG4DS32.AX` (the
round-44 critical path) does not import `_ftol`; the win is
exclusively in the audio splitter's PE-load surface.  See
`tests/round52_msvcrt_ftol.rs`.

**Round 51 — Encode side of the IC* surface lands end-to-end
against `mpg4c32.dll`; `quality=5000` BGR24 → MP43 → BGR24
self-roundtrip at 27.83 dB PSNR.**  The decode pipeline
(rounds 21..44) had been parking at 42.9 dB PSNR-RGB across
17/17 frames on five multi-frame 352×288 fixtures; round 51
brings up the symmetric encode pipeline.  Six new host-side
`IC*Compress*` wrappers (`ic_compress_query`,
`ic_compress_get_format`, `ic_compress_get_size`,
`ic_compress_begin`, `ic_compress`, `ic_compress_end`)
mirror the existing decompress family one-for-one, sharing
the round-22 `msmpeg4_v3_preinit` handshake plant for the
v3-wrapper-handshake gate at
`mpg4c32!DriverProc+0x14e2` (without which both BEGIN
messages return `ICERR_INTERNAL`).  The
`ICCOMPRESS` struct (48 bytes / 12 dwords) and the
`ICM_COMPRESS_*` message ordinals (`ICM_USER + 4..9` =
`0x4004..0x4009`) are transcribed from
`winsdk-10/Include/.../um/Vfw.h` + the MSDN
`ICCompress` / `ICCompressBegin` topic pages.  At
`quality=5000` the codec compresses a 176×144 BGR24 gradient
to 970 bytes (~78× ratio); the encoded MP43 elementary
bitstream survives a self-roundtrip through the existing
decode path at 27.83 dB.  The codec emits FOURCC `MP43` for
every encode regardless of the requested handler tag —
mirroring the round-44 decode-side `IPin::ReceiveConnection`
"MP43-only" finding.  Encode-input format probe lands BGR24
/ BGR32 / YV12 / I420 / IYUV / YUY2 / UYVY / RGB16-565 /
BGR8-palette as accepted formats; NV12 / NV21 / RGB15-555 /
MP43-as-input are rejected with `ICERR_UNSUPPORTED`.  See
`tests/round51_msmpeg4_encode_roundtrip.rs`.

**Round 50 — `msvcrt!_beginthreadex` stub advances `msadds32.ax`
PE-load past the splitter's CRT thread-creation edge; combined
with the r48 `_endthreadex` no-op stub this closes the entire
CRT thread-lifecycle surface for the splitter's PE-load.**
Round 49 wired `_strnicmp` and pinned the next splitter blocker
as `_beginthreadex`; round 50 wires the 6-arg cdecl
`_beginthreadex` (`uintptr_t __cdecl _beginthreadex(void
*security, unsigned stack_size, unsigned (__stdcall
*start_address)(void *), void *arglist, unsigned initflag,
unsigned *thrdaddr)`) as a fail-soft no-op returning 0 — the
MSDN-documented failure sentinel.  The codec sandbox never
actually spawns the splitter's worker thread on the decode
path we drive (we only exercise `DLL_PROCESS_ATTACH` /
`DriverProc` / `IPin::ReceiveConnection`); real call sites in
the splitter's init layer check the return for non-zero and
either fall back or skip the worker-thread codepath cleanly.
If the caller passes a non-NULL `thrdaddr` pointer the stub
clears `*thrdaddr` to 0; OOB pointers are silently swallowed.
After round 50, `Sandbox::load("msadds32.ax")` advances past
`_beginthreadex` and now stops at the next unresolved import:
`msvcrt!_ftol`.  No DirectShow / VfW decode metric changes —
`MPG4DS32.AX` (the round-44 critical path) does not import
`_beginthreadex`; the win is exclusively in the audio
splitter's PE-load surface.  See
`tests/round50_msvcrt_beginthreadex.rs`.

**Round 49 — `msvcrt!_strnicmp` stub advances `msadds32.ax`
PE-load past the splitter's case-insensitive bounded-compare
edge.**  Round 48 wired `_endthreadex` and pinned the next
splitter blocker as `_strnicmp`; round 49 implements the real
ASCII-tolower bounded compare (`int __cdecl _strnicmp(const
char *string1, const char *string2, size_t count)` returning
`< 0` / `0` / `> 0`).  Unlike the previous IAT-stub family,
`_strnicmp` is NOT a no-op candidate — the splitter calls it
during init for FOURCC / header-magic matching, so a stub
returning a constant 0 (== "every string compares equal")
would let the codec take a wrong branch and silently misbehave
on a later decode.  The implementation folds each byte to
lowercase by the ASCII rule `b'A'..=b'Z' → +0x20`, terminates
early on the first NUL on either side, returns the byte
difference cast to `i32`, and fail-softs to 0 (treat as equal)
when either pointer reads OOB or `count > 1 MiB`.  After round
49, `Sandbox::load("msadds32.ax")` advances past `_strnicmp`
and now stops at the next unresolved import:
`msvcrt!_beginthreadex`.  No DirectShow / VfW decode metric
changes — `MPG4DS32.AX` (the round-44 critical path) does not
import `_strnicmp`; the win is exclusively in the audio
splitter's PE-load surface.  See
`tests/round49_msvcrt_strnicmp.rs`.

**Round 48 — `msvcrt!_endthreadex` stub advances `msadds32.ax`
PE-load past the splitter's thread-teardown edge.**  Round 47
unblocked `gdi32!StretchDIBits` and pinned the next splitter
blocker as `msvcrt!_endthreadex`; round 48 wires the 1-arg
cdecl `_endthreadex` (`void __cdecl _endthreadex(unsigned
retval)`) as a fail-soft no-op returning 0.  MSDN documents
the function as `__declspec(noreturn)` — in the real CRT
control never returns to the caller — but the codec sandbox
never actually spawns the splitter's worker thread on the
decode path we drive (we only exercise `DLL_PROCESS_ATTACH` /
`DriverProc` / `IPin::ReceiveConnection`); the IAT slot just
needs to resolve at PE-load time, and the cdecl `Ok(0)` stub
falls back to the caller's saved return-address rather than
terminate the host process.  After round 48, `Sandbox::load
("msadds32.ax")` advances past `_endthreadex` and now stops
at the next unresolved import: `msvcrt!_strnicmp`.  No
DirectShow / VfW decode metric changes — `MPG4DS32.AX` (the
round-44 critical path) does not import `_endthreadex`; the
win is exclusively in the audio splitter's PE-load surface.
See `tests/round48_msvcrt_endthreadex.rs`.

**Round 47 — `gdi32!StretchDIBits` stub advances `msadds32.ax`
PE-load past the splitter's render-out edge.**  Round 46
unblocked `user32!{SetTimer, KillTimer}` and pinned the next
splitter blocker as `gdi32!StretchDIBits`; round 47 wires the
13-arg `StretchDIBits` (`int StretchDIBits(HDC, int xDest, int
yDest, int DestWidth, int DestHeight, int xSrc, int ySrc, int
SrcWidth, int SrcHeight, const VOID *lpBits, const BITMAPINFO
*lpbmi, UINT iUsage, DWORD rop)`) as a fail-soft stub that
returns the caller's `DestHeight` as the "scanlines copied"
count per MSDN's success contract.  The codec sandbox never
actually paints; `msadds32.ax` is the MS-MPEG-4-v3 audio
splitter and only pulls this import as part of its
statically-linked render-out surface, never invokes it on the
decode path we drive.  After round 47, `Sandbox::load
("msadds32.ax")` advances past `StretchDIBits` and now stops
at the next unresolved import: `msvcrt!_endthreadex`.  No
DirectShow / VfW decode metric changes — `MPG4DS32.AX` (the
round-44 critical path) does not import `StretchDIBits`; the
win is exclusively in the audio splitter's PE-load surface.
See `tests/round47_gdi32_stretch_dibits.rs`.

**Round 46 — `user32!{SetTimer, KillTimer}` stubs advance
`msadds32.ax` PE-load past the entire timer-API surface.**
Round 45 unblocked `MapDialogRect` and pinned the next
splitter blocker as `KillTimer`; round 46 wires both
`SetTimer` (returns the caller's `nIDEvent` if non-zero,
else a synthetic `1`) and `KillTimer` (returns `TRUE` per
MSDN's "destroyed" contract) as fail-soft stubs in one
commit.  The codec sandbox never enters the message-loop
branch that would let a TIMERPROC actually fire, so
no scheduling is performed host-side — the IAT slots just
need to resolve at PE-load time.  After round 46,
`Sandbox::load("msadds32.ax")` advances past both timer
imports and now stops at the next unresolved import:
`gdi32!StretchDIBits`.  No DirectShow / VfW decode metric
changes — `MPG4DS32.AX` (the round-44 critical path) does
not import `SetTimer` / `KillTimer`; the win is
exclusively in the audio splitter's PE-load surface.  See
`tests/round46_user32_set_kill_timer.rs`.

**Round 45 — `user32!MapDialogRect` stub unblocks `msadds32.ax`
PE-load past the round-24 user32 surface gap.**  The
MS-MPEG-4-v3 reference bundle's audio-splitter half
(`msadds32.ax`) imports 29 distinct `user32` symbols; round
24 wired the first batch (`RegisterClassExA` /
`UnregisterClassA`), and round 45 adds `MapDialogRect` as a
fail-soft identity passthrough (leave the caller's RECT
untouched, return TRUE per MSDN's `BOOL` contract).  After
round 45, `Sandbox::load("msadds32.ax")` advances past
`MapDialogRect` and now stops at the next unresolved
`user32` import: `KillTimer`.  No DirectShow / VfW decode
metric changes — `MPG4DS32.AX` (the round-44 critical
path) does not import `MapDialogRect`; the win is
exclusively in the audio splitter's PE-load surface, which
moves from "stuck at MapDialogRect" to "stuck at
KillTimer", ungating any future round that wants to drive
its `DLL_PROCESS_ATTACH` or DriverProc.  See
`tests/round45_user32_map_dialog_rect.rs`.

**Round 44 — entire MS-MPEG-4 v3 fixture corpus exercised
through the round-43 DirectShow pipeline.**  Sixteen
fixture-runs from `docs/video/msmpeg4-fixtures/`, all
surfacing every expected `Frame::Video` (20/20 frames in
aggregate).  Two distinct axes covered: **FourCC parity
(6/6)** — the corpus's six fourcc-* fixtures (MP43, DIV3,
DIV4, DVX3, AP41, COL1) all decode through the round-43 path
when registered against the MP43 subtype (empirical finding:
`MPG4DS32.AX` accepts only `MEDIASUBTYPE_MP43` at
`IPin::ReceiveConnection`; every other FOURCC subtype is
rejected with `0x8004022a` `VFW_E_TYPE_NOT_ACCEPTED`, exactly
mirroring real DirectShow's reliance on the FilterMapper to
route every MS-MPEG-4-v3 FOURCC to one MP43-tagged filter).
**Harder content fixtures** — `motion-pan-352x288` 4/4
(large-magnitude inter-frame MVs at CIF), `with-skip-mbs-352x288`
5/5 (~38% SKIP-MB fraction), plus single-frame coverage of
the qscale-31 / qscale-2 / mandelbrot-AC-pred / testsrc-CIF /
QCIF I-frame edge cases.  No `src/` change required: round
43's sample-release cycle and FourCC-blind subtype negotiation
already supported the entire corpus.  See
`tests/round44_fourcc_parity_and_harder_fixtures.rs`.

**Round 43 — full 6-frame GOP decodes end-to-end at 352×288
through DirectShow.**  The two blockers round 42 identified via
the gop-30 diagnostic blob are both fixed: (a) a corrupted
output-allocator pool pointer no longer surfaces as a memory-fault
trap inside our `alloc_get_buffer` stub — the walk now sanity-
checks each `cur+36` / `cur+32` read and treats failures as
`VFW_E_TIMEOUT`; and (b) the sample-release cycle now closes
properly via a dedicated `sample_release` thunk that recycles
the sample to its allocator's pool on the `1 → 0` refcount
transition (replacing the generic `release` thunk's floor-at-1
behaviour), `alloc_get_buffer` forcing the issued sample to
refcount = 1 so the codec's standard one-AddRef + one-Release
pattern reliably reaches 0, plus an explicit
`IMemAllocator::ReleaseBuffer` call from `receive_frame` against
the input allocator after `IMemInputPin::Receive` returns.  Net
result: `gop-30-352x288` decodes 6/6 frames (was 1/6 in r42), and
the `r43_pool_recycle_survives_ten_ip_cycles` regression guard
drives 10× back-to-back I+P pairs (20 frames total, well past the
4-slot pool) through one decoder with 20/20 Video surfacing.  See
`tests/round43_full_gop_decode.rs`.

**Round 42 — first multi-frame DShow decode lands.**  The
`i-frame-then-p-frame-176x144` MS-MPEG-4 v3 fixture's I-frame
followed by its P-frame both surface as `Frame::Video` through
the SAME `SandboxedDshowDecoder` instance (1 → 2 frames
end-to-end).  Round 41 was the first time ANY Video frame ever
came out of this pipeline but only ever drove a single packet;
r42 confirms the codec's internal state machine survives
back-to-back `Receive` calls against the same filter instance.
Driving the larger `gop-30-352x288` (6-frame GOP at CIF) surfaces
1/6 frames Video and identifies two distinct R43 blockers via
the diagnostic blob: (a) the output-allocator side trap at
MPG4DS32 RVA `0x4064d4` (parity gap with the input-side r41 fix
— output-allocator stub is called with `ecx=output_alloc_vtbl`
rather than a `this` pointer), and (b) sample-release cycle
gap that exhausts the host pool after 4 unreleased samples
(`0x80040211 = VFW_E_NOT_COMMITTED`).  See
`tests/round42_dshow_iframe_then_pframe.rs`.

**Round 41 — first end-to-end MP43 keyframe decode through
DirectShow lands.**  Round 40 had localised a 4-byte stack
imbalance to `Transform`'s `pop ebx` at MPG4DS32 RVA `0x4065c4`;
round 41 bisected across the ten internal `call dword ptr [...]`
sites and pinned it to the FIRST one — `0x4064d4 = call [ecx+0x1c]`
(`IMemAllocator::GetBuffer(this, ppBuffer, pStartTime, pStopTime,
dwFlags)`).  This is FIVE pushed dwords (this + four args), but
the host stub registration in `crate::com::host_iface::register`
had `arg_dwords=4`, so the dispatcher's stdcall callee-cleanup
in `win32::dispatch_stub` popped 16 bytes instead of 20 and left
esp 4 bytes too low.  Transform's matched `pop ebx` then read
`0x60000110` (filter_base, leftover stack) instead of the correct
saved-ebx slot one dword higher (`0x600007a0` = pInSample); the
downstream slot-13 call on the wrong `ebx` landed on the codec's
PRIMARY-vtable internal method `0x2da7`, which expected
`ecx == filter_base` and faulted on the resulting `IsEqualGUID
(0x1c, ...)` read.  Bumping `arg_dwords` to 5 (and reading the
`dwFlags` arg in the stub) made the trap GO AWAY entirely:
`IMemInputPin::Receive` returns S_OK and the codec emits a
24bpp BGR sample through the downstream pin which
`surface_received_dshow_frame` flips top-down and surfaces as
`Frame::Video`.  The diagnostic watchpoints from round 40
remain (drained on success + trap branches now) so any
regression re-traps with the bisect data immediately to hand.

**Round 40 — register-snapshot + memory-probe watchpoints
identify a stack imbalance inside `CTransformFilter::Transform`
as the root cause of the r39 trap.** New
`Cpu::add_register_watchpoint` instrumentation captures the
integer register file plus `[esp]` / `[esp+4]` / `[ebp+8]` /
`[ebp-0x50]` AT WATCHPOINT TIME (not trap time, which masks
intervening writes).  Sixteen watchpoints arm the post-Transform
return BB plus Transform's prologue (`0x6479` push ebx) and
epilogue (`0x65c4` pop ebx).  Findings: (a) the saved-ebx slot
`[ebp-0x50]@0x900ffe60` holds `0x600007a0` (pInSample) intact
throughout Transform's body — but at `0x65c4`, esp is FOUR BYTES
LOWER than `[ebp-0x50]`, so `pop ebx` reads `0x60000110` from
the wrong stack slot.  (b) Transform's `[ebp+8]@0x900ffeb8` =
pInSample throughout — hypothesis "SetProperties write-back
clobbered the arg slot" RULED OUT.  Some intermediate
`__stdcall` call inside Transform's body either pushed an extra
arg or returned with a callee-cleanup short by 4 bytes.  R41
must bisect across each `call dword ptr [...]` site in
Transform (`0x4064d4`, `0x4064f3`, `0x406505`, `0x406545`,
`0x40655b`, `0x40656e`, `0x40657f`, `0x406590`, `0x4065a8`,
`0x4065bd`) tracking esp delta before/after each.  The r39
disasm interpretation that called `0x2da7` "JoinFilterGraph
from the IBaseFilter sub-vtable" was wrong: `0x2da7` is slot 13
of the C++ class's PRIMARY vtable, an internal `__thiscall`
method that expects `ecx == filter_base` and dereferences
`[ecx+0x8c] = m_pInput`.  With ebx incorrectly set to filter_base
(via the stack-imbalance bug) the slot-13 dispatch lands on
this method with a stack-junk `ecx`, and it traps inside
`IsEqualGUID` reading `0x1c`.  Receive trap unchanged at
MPG4DS32 RVA `0x7184`.

**Round 39 — `IID_IMediaSample2` host-side QI support; Transform
now takes its success-tail at `0x65c0` (was `0x6560` failure
cleanup).** Round-38 disasm of the QI at MPG4DS32.AX RVA `0x4064f3`
identified the IID being requested as
`{36B73884-C2C8-11CF-8B46-00805F6CEF60}` =
`IID_IMediaSample2` (Microsoft Platform SDK `strmif.h` extension
of `IMediaSample`).  Returning `E_NOINTERFACE` (the round-30..38
baseline) sent the codec's `CTransformFilter::Transform` down its
QI-failure cleanup branch at `0x6560`, where it propagated
per-sample properties through individual `IMediaSample` slot
calls.  Round 39 wires the host vtable up to recognise the IID
in `sample_qi` and adds three new thunks at slots 18..20:
`IMediaSample::SetMediaTime` (slot 18 — previously NULL on the
host vtable, an active footgun for the cleanup branch's `[ecx+0x48]`
call at RVA `0x4065bd`), plus `IMediaSample2::GetProperties` /
`SetProperties` (slots 19/20).  Both new methods round-trip the
public `AM_SAMPLE2_PROPERTIES` struct (`cbData` / `dwSampleFlags`
/ `lActual` / `pbBuffer` / `cbBuffer` / `pMediaType`) so the
codec's success-branch write-back at RVA `0x6545` accepts our
sample.

**Round 30 — DirectShow IMemAllocator + IMediaSample host stubs
land; ICM_DECOMPRESS_GET_FORMAT dim probe + Indeo / Cinepak trait
tests.** New `crate::com::host_iface::mint_host_mem_allocator` /
`mint_host_media_sample` plus 11+18 vtable thunks back the codec's
`IMemInputPin::NotifyAllocator(host_alloc, FALSE)` →
`Receive(host_sample)` chain end-to-end. `SandboxedDshowDecoder`
wires DirectShow `make_decoder` (round 29 returned `Unsupported`
immediately) through DllGetClassObject → CreateInstance →
EnumPins → JoinFilterGraph → ReceiveConnection → IMemInputPin →
Receive. Codec output capture via a downstream HostIPin::Receive
callback is r31 work — `receive_frame` surfaces `Unsupported`
carrying the diagnostic + a `trace_ring` snapshot. Sub-goal B:
`Sandbox::ic_decompress_get_format` lifts round-29's hard
"`width is None` reject" into a lazy `ICM_DECOMPRESS_GET_FORMAT`
probe, plus 4 trait-path keyframe-decode tests for IV31 (cubes.mov
through IR32_32.DLL), IV41 (crashtest.avi through IR41_32.AX),
IV50 (cat_attack.avi through IR50_32.DLL), CVID (Cinepak through
ICCVID.DLL). **Total: 492 tests.**

**Round 27 — IFilterGraph + IPin host stubs land; MPG4DS32
input-pin handshake reaches `S_OK`.** New `src/com/host_iface.rs`
mints synthetic guest-side COM objects whose vtable function
pointers are thunk addresses dispatched by Rust handlers — the
codec sees an `IFilterGraph` host that fail-softs every method,
and an `IPin` host that pretends to be an OUTPUT pin advertising
the staged `AM_MEDIA_TYPE`.  Bound together by
`IBaseFilter::JoinFilterGraph(host_graph, NULL)` (returns S_OK)
+ `IPin::ReceiveConnection(host_output_pin, MP43 VIDEOINFOHEADER)`
returns **`S_OK = 0x00000000`** — round-26's `VFW_E_NO_TYPES`
gate is past.  Subsequent calls return `VFW_E_ALREADY_CONNECTED`
confirming the pin is bound.  `IMemInputPin` reachable via
QI; `GetAllocator → VFW_E_xxx` (no allocator yet — sub-goal B
next-round target).  Probe matrix: `MP43`/`mp43`/`MP4S`/`mp4s`/
`MPG4`/`MP42`/`DIV3`/`DIVX`/`DX50` × `VIH1`/`VIH2` all return
the same `VFW_E_NO_TYPES` from the codec's `CheckMediaType` when
called against a self-loop pConnector — but `S_OK` once a
HostIPin advertising `MP43+VIH1` provides the missing OUTPUT-side
of the handshake.  WMVDS32 CLSID side-bonus: static analysis of
`.rdata` finds 23 fourcc-base `MEDIASUBTYPE_*` GUIDs but no
dedicated codec CLSID literal (binary constructs it dynamically;
deferred).  **Total: 428 tests.**

**Round 26 — `user32!CreateWindowExA` cascade stubs land + IPin
ReceiveConnection probed.** Synthetic-HWND registry
(`HWND_BASE = 0xCAFE_0000` + monotonic counter) plus
`CreateWindowExA` / `UpdateWindow` / `IsWindow` / `GetMessageA` /
`DispatchMessageA` / `TranslateMessage` / `PeekMessageA` /
`PostQuitMessage` stubs; `DestroyWindow` / `MoveWindow` patched
to return TRUE per MSDN. Stretch: `IPin::ReceiveConnection` on
the MPG4DS32 input pin with an MP43 / `VIDEOINFOHEADER` media
type executes cleanly — returns `E_POINTER` (0x80004003) on a
NULL `pConnector` and `0x80040208` (VFW_E-class — likely needs
IFilterGraph hookup) when the pin's own pointer is passed.
Round 27 wires IFilterGraph + IMemAllocator + IMediaSample stubs
to drive the connection to `S_OK` and exercise `IPin::Receive`.
**Total: 408 tests.**

**Round 25 — DirectShow IBaseFilter scaffolding lands. All five
stages reached on `MPG4DS32.AX`** (`IID_ICLASSFACTORY` returned,
IBaseFilter spawned, Stop/Pause/Run all `S_OK`, IPin enumerated,
input pin reachable for round-26 `Receive`). Round 24 verdict:
`WMVDS32.AX`/`MPG4DS32.AX` lack `DriverProc` entirely — they're
DirectShow filters reachable via COM. Round 25 builds that COM
ABI surface (`src/com/`, ~600 LOC: `Guid` parser, 11 IID
constants, `ComObjectTable` AddRef/Release bookkeeping,
`vtable_ptr`/`method_va`/`call_method`/`query_interface`
helpers) and drives it end-to-end:

* **Stage 1 (always-runs).** `Guid` parser round-trips MIDL
  `{xxxxxxxx-xxxx-…}` strings; 11 hardcoded IIDs (IUnknown,
  IClassFactory, IPersist, IMediaFilter, IBaseFilter, IPin,
  IMemInputPin, IEnumPins, IMemAllocator, IMediaSample,
  IFilterGraph) sourced from public MSDN documentation +
  Windows SDK MIDL-generated headers (no BaseClasses sample
  source consulted).
* **Stage 2 (DllGetClassObject).** `MPG4DS32.AX` returns a
  class factory at guest VA `0x600000B0` for the bundle's
  MPEG-4 v3 decoder filter CLSID
  `{82CCD3E0-F71A-11D0-9FE5-00609778EA66}`.
* **Stage 3 (CreateInstance + IBaseFilter spawn).**
  `IClassFactory::CreateInstance(NULL, IID_IBaseFilter, ppv)`
  returns a real IBaseFilter at `0x600000EC`; QueryInterface
  succeeds for IUnknown/IPersist/IMediaFilter/IBaseFilter;
  Release drops the chain to refcount 0.
* **Stage 4 (IBaseFilter::Run reach goal).**
  `IBaseFilter::Stop` / `Pause` / `Run(0)` all return `S_OK`
  without an attached filter graph. `IBaseFilter::EnumPins`
  also `S_OK`.
* **Stage 5 stretch (IPin walk).** `IEnumPins::Next` returns
  one IPin at `0x6000025C`; `IPin::QueryDirection` reports
  `PIN_INPUT`. The MPG4DS32 input pin is now reachable from
  the host for round-26 to push samples through.

`ole32.dll` upgrades alongside: `CoCreateInstance` is now a
real lookup against the in-process class-factory cache (no
more blind `E_NOTIMPL`), `CoInitializeEx` and
`CoTaskMemRealloc` join the stub set. Test count: 363 → 395
(+32). `WMVDS32.AX` returns `CLASS_E_CLASSNOTAVAILABLE` for
the MPEG-4 CLSID — its actual filter CLSID is the round-26
follow-up (the candidate list needs the WMV decoder GUID per
`wmvax.inf`).

**Round 24 — multi-frame MP43 decode at 352×288 + WMV
DirectShow-ABI verdict.** Round 23 unblocked I+P at 176×144
on a 2-frame fixture; round 24 scales to the 5..6 frame
fixtures at 352×288 and resolves the WMV1/WMV2 question.

* **A — multi-frame MP43.** New
  `tests/round24_mp43_multiframe_and_wmv.rs` walks mpg4c32
  through five `docs/video/msmpeg4-fixtures/` fixtures at
  352×288: **17/17 frames** all return `ICERR_OK` with
  > 25% non-zero output (gop-30 6/6, with-skip-mbs 5/5,
  motion-pan 4/4, intra-pred-active 1/1, qscale-high 1/1).
  Exercises `use_skip_mb_code=1` + alternate-MV-VLC +
  qscale=16 + AC-prediction paths the round-23 fixture
  didn't reach. Per-352×288 P-frame settles at ~5 M
  emulator instructions; state carries cleanly across six
  successive `ICDecompress` calls inside one `ICOpen`.
* **B — WMV1/WMV2 verdict.** Same test file probes
  `WMVDS32.AX` and `MPG4DS32.AX` through `DRV_LOAD →
  DRV_ENABLE → DRV_OPEN` with every plausible handler 4CC.
  Both binaries **lack a `DriverProc` export** — they are
  pure DirectShow filters (`.ax` extension, expose
  `DllGetClassObject` + `IBaseFilter`-derived COM objects),
  not VfW drivers. The VfW message ABI is therefore
  fundamentally absent in the wmpcdcs8-2001 bundle.
  Round-25+ would either implement a minimal IBaseFilter /
  IPin / IMemAllocator DirectShow wrapper, or source a
  VfW-shaped WMV decoder (some early WMP releases shipped
  `wmvcore.dll` with VfW-compat exports).
* **Matrix delta probe.** mpg4c32 rejects every YUV output
  4CC (YV12 / I420 / IYUV / YUY2 / UYVY → `ICERR_BADFORMAT`)
  through `ICDecompressQuery`; only BI_RGB is honoured.
  The round-23 ~12 dB delta vs ffmpeg is therefore a
  property of the codec's internal BGR converter, not a
  selectable host-side option. The same test file ships a
  clean-room BT.601 limited-range YUV→BGR converter (from
  BT.601-7 Annex 1) ready for the round-25 host-side
  renderer once mpg4c32 is rerouted (or replaced) to
  surface its YUV.
* **ICINFO_SIZE = 568 strict-codec gate.** New constant
  documents mpg4c32's `cmp [ebp+0x10], 0x238 / jb
  .return_zero` gate at `DriverProc+0x999`. Round-20's
  experimental `ICGetInfo(cb=80)` hit it silently;
  `cb=568` populates the full identity card
  (`fccType='vidc' / fccHandler='MP43' / dwFlags=0x28 /
  dwVersion=1 / dwVersionICM=0x104`). Two `user32` stubs
  added (`RegisterClassExA` → 0xC001, `UnregisterClassA`
  → TRUE) for the `msadds32.ax` audio-splitter PE-load
  surface; the splitter itself remains parked off the
  critical path.

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
| MSMPEG4 v3 (DIV3/MP43) | `mpg4c32.dll` (VfW) | `fourcc-MP43` keyframe + I+P 176×144 + 5 multi-frame 352×288 fixtures | 22 / 23 / 24 | `ICERR_OK` (**17/17 frames** at 352×288 + 2/2 at 176×144; PSNR 42.9 dB vs ffmpeg oracle) |
| WMV1/2 (WMV1/WMV2) | `wmvds32.ax` (DS filter) | n/a | 21 / 24 | PE-load ✓; lacks `DriverProc` export — DirectShow ABI, not VfW. Needs IBaseFilter wrapper |

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
