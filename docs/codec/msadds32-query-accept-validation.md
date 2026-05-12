# `msadds32.ax` AMT-acceptance validator (round 60)

This document captures the clean-room reverse-engineering of the
audio-input pin's media-type validation chain inside
`msadds32.ax`'s "Windows Media Audio Decoder" DirectShow filter
(CLSID `{22E24591-49D0-11D2-BB50-006008320064}`, from the
`wmpcdcs8-2001` binary bundle shipped under
`docs/video/msmpeg4/reference/binaries/`).

## Methodology

All disassembly performed against raw byte inspection of
`msadds32.ax` via the in-tree `tests/round60_msadds32_query_accept_disasm.rs`
harness, using opcode encodings from Intel® 64 and IA-32
Architectures Software Developer's Manual, Volume 2.  No
Wine / ReactOS / MinGW / Microsoft DShow / ffmpeg WMA source
consulted.

## Background — round 58/59 baseline

Round 58 demonstrated that `IPin::ReceiveConnection` on the
splitter's input pin rejects synthetic `AM_MEDIA_TYPE`s carrying
all-zero `WAVEFORMATEX::cbSize`-bytes-of-extradata.  Round 59
extended this with REAL extradata extracted from ffmpeg-generated
`.wma` ASF fixtures (`wFormatTag=0x0160`/`0x0161`,
`cbSize`={4,10}), and observed the splitter still returns
`HRESULT 0x80004005` (`E_FAIL`) for both.  The validator was
rejecting *something specific* the ffmpeg WMA encoder does not
produce.

## The validation chain

`IPin::ReceiveConnection` (vtable slot 4 of the input pin) at
RVA `0x476f` performs four pre-validation gates, then dispatches
to two virtual methods on an internal "inner-class" object
located at `pin - 0xC`:

```text
ReceiveConnection(this, pConnector, pmt):
  ; gate 1: pConnector != NULL && pmt != NULL              → E_POINTER (0x80004003)
  ; gate 2: this->m_pConnected == NULL                     → VFW_E_ALREADY_CONNECTED (0x80040204)
  ; gate 3: this->m_pFilter->m_State == State_Stopped (0)  → VFW_E_NOT_STOPPED (0x80040224)
  ; gate 4: pConnector->QueryDirection() != m_Direction    → VFW_E_INVALID_DIRECTION (0x80040208)

  ; trampoline path (inner.vtable[10] @ RVA 0x5623)
  call inner.CheckConnect(pConnector)
  if (failed) goto end           ; RAW return — no remap
  ; (CheckConnect is itself a trampoline → calls helper at RVA 0x4743
  ;  which only verifies opposite-direction pins; returns S_OK on our setup.)

  ; CheckMediaType (inner.vtable[8] @ RVA 0x568a)
  call inner.CheckMediaType(pmt)
  if (success) goto connect_setup
  ; failure path remap:
  if (eax == 0x80004005 || eax == 0x80070057)
      eax = 0x8004022a            ; VFW_E_TYPE_NOT_ACCEPTED
  goto end

connect_setup:
  this->m_pConnected = pConnector
  pConnector->AddRef()
  call inner.SetMediaType(pmt)                ; inner.vtable[9] @ RVA 0x56cb
  call inner.CompleteConnect(pConnector)      ; inner.vtable[12] @ RVA 0x2057
                                              ; *** THE REAL VALIDATOR ***
```

The `CheckMediaType` virtual at RVA `0x568a` is a near-no-op:
it eagerly returns `S_OK` after calling the BaseFilter's stub at
RVA `0x4a19` (which itself is `xor eax, eax; ret 8`).  This means
the per-AMT bytes are NOT validated by `CheckMediaType` — the
real check happens inside `CompleteConnect`.

## `CompleteConnect` (RVA 0x2057) — the actual gate

The `CompleteConnect` callee re-fetches the AMT via the upstream
pin's `IPin::ConnectionMediaType` (so it inspects what the
upstream pin advertises, not what ReceiveConnection received),
then inspects the WAVEFORMATEX:

```text
CompleteConnect(this, pConnector):
  pConnector->ConnectionMediaType(&local_amt)
  pbFormat = local_amt.pbFormat
  switch (word [pbFormat]):                            ; wFormatTag
    case 0x0160 (WMA1):
      if (word [pbFormat + 0x10] < 0x29) goto fail     ; cbSize >= 41 ?
      if (memcmp(pbFormat + 0x16,
                 "1A0F78F0-EC8A-11d2-BBBE-006008320064\0",
                 0x25) != 0) goto fail                 ; 37-byte magic at extradata[4]
      goto cleanup_ok

    case 0x0161 (WMA2):
      if (word [pbFormat + 0x10] < 0x2F) goto fail     ; cbSize >= 47 ?
      if (memcmp(pbFormat + 0x1c,
                 "1A0F78F0-EC8A-11d2-BBBE-006008320064\0",
                 0x25) != 0) goto fail                 ; 37-byte magic at extradata[10]
      goto cleanup_ok

    default:
      esi = 0x8000FFFF                                 ; E_UNEXPECTED

  cleanup_ok:
    esi = 0
    goto end

  fail:
    esi = 0x80004005                                   ; E_FAIL  — round 59 observed THIS
    goto end
```

Specifically:

* The validator reads `wFormatTag` directly from `pbFormat`.  Must
  be `0x0160` (WMA1) or `0x0161` (WMA2); anything else returns
  `E_UNEXPECTED`.
* The validator reads `cbSize` from `pbFormat + 0x10` (= the
  `WAVEFORMATEX::cbSize` field).  For WMA1 it must be `>= 0x29` (41).
  For WMA2 it must be `>= 0x2F` (47).
* The validator then runs `memcmp(extradata + N, MAGIC_CLSID, 0x25)`
  where `N = 4` for WMA1 (`pbFormat + 0x16` = `pbFormat + 18 + 4`)
  and `N = 10` for WMA2 (`pbFormat + 0x1c` = `pbFormat + 18 + 10`).
* The 37-byte `MAGIC_CLSID` constant lives at `.rdata` RVA
  `0x11138` and decodes as the ASCII string
  `"1A0F78F0-EC8A-11d2-BBBE-006008320064\0"` (36 visible chars
  plus the trailing NUL — total 37 bytes which matches the
  validator's `push 0x25` length).

## Why ffmpeg's `.wma` fixtures fail

ffmpeg's `wmav1` / `wmav2` encoders emit:

* WMA1: `wFormatTag = 0x0160`, `cbSize = 4`, extradata
  `[0x00, 0x00, 0x01, 0x00]`.  The cbSize check (4 < 41) fails
  immediately — without ever inspecting the extradata bytes.
* WMA2: `wFormatTag = 0x0161`, `cbSize = 10`, extradata
  `[0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]`.
  Same story — cbSize (10 < 47) fails before the magic check.

So the splitter is NOT validating codec parameters (channels,
sample rate, block align, bitrate) — it is gating on a 37-byte
opaque CLSID-shaped magic string embedded inside the extradata.
This is almost certainly an installation-tracker / DRM-style
"only accept media from streams my own encoder produced" gate;
the magic string is the Microsoft WMA encoder's own component
CLSID, and Microsoft's encoder appends it to every encoded
stream's extradata.

ffmpeg has no reason to emit this string.

## Construction recipe for a passing AMT

```rust
use oxideav_vfw::com::AmtBlueprint;

// WMA2 (cbSize must be >= 47, extradata[10..47] = MAGIC_CLSID)
let bp = AmtBlueprint::wma_criteria_passing(
    0x0161,      // wFormatTag = WMA2
    1,           // nChannels
    44_100,      // nSamplesPerSec
    4_000,       // nAvgBytesPerSec
    185,         // nBlockAlign
);
assert_eq!(bp.extradata.len(), 47);
assert_eq!(&bp.extradata[10..47], b"1A0F78F0-EC8A-11d2-BBBE-006008320064\0");
```

Round 60's `phase4_*` tests verify this lands `HRESULT 0x00000000`
(`S_OK`) on the live `msadds32.ax` splitter for both WMA1 and
WMA2.

## What lies beyond AMT acceptance

Once `ReceiveConnection` returns `S_OK`, the splitter is in
`State_Stopped` with a media type accepted.  Pushing real
encoded bytes via `IMemInputPin::Receive` (after
`IMediaFilter::Pause + Run(0)`) returns `0x80040209`
(`VFW_E_NOT_COMMITTED`) — the codec's internal `IMemAllocator`
has not been committed (round 60 does not drive that handshake).

Round 61's anchor task is to drive the full allocator commit
sequence (`GetAllocator → SetProperties → Commit →
NotifyAllocator`) so that pushing a real WMA frame surfaces PCM
bytes on the host sink.  The validator decoded here is now off
the critical path for that work.

## Cross-references

* `crates/oxideav-vfw/src/com/asf_amt.rs` —
  `AmtBlueprint::wma_criteria_passing` constructor.
* `crates/oxideav-vfw/tests/round60_msadds32_query_accept_disasm.rs` —
  full disassembly harness, phase-by-phase.
* `crates/oxideav-vfw/tests/round58_msadds32_audio_amt_walk_and_connect.rs` —
  round 58 baseline (synthetic AMTs rejected).
* `crates/oxideav-vfw/tests/round59_msadds32_wma_real_fixture.rs` —
  round 59 baseline (ASF-extracted AMTs rejected).
