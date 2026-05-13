# `msadds32.ax` — `IMemInputPin::Receive` E_UNEXPECTED bail-out (round-64 forensics)

This document captures the round-64 clean-room forensics on the
`IMemInputPin::Receive` HRESULT that surfaces after the round-63
[`Sandbox::msadds32_patch_helper_addref`] workaround clears the
NULL-deref trap documented in
[`msadds32-receive-null-0x20.md`](msadds32-receive-null-0x20.md).

With the patch applied (any `value ≥ 6554`), `Receive` runs to
completion + returns `eax = 0x8000FFFF` (`E_UNEXPECTED`).

All decoding here is from raw `msadds32.ax` byte inspection
against Intel SDM Vol. 2 opcode tables, plus public MSDN COM ABI
references. **No Wine / ReactOS / MinGW / Microsoft DShow base-
class source was consulted.**

## Failure site

The `E_UNEXPECTED` value is **NOT** emitted via any of the 10
`mov eax, 0x8000FFFF` (`b8 ff ff 00 80`) sites visible in a linear
byte scan of `.text`.  Per phase-2 of
`tests/round64_msadds32_e_unexpected.rs`, none of those sites is
reached during the patched `Receive`.

The actual emission point is the in-line `c7 45 08 ff ff 00 80`
sequence at **RVA `0x172f`**:

```text
0x172f: c7 45 08 ff ff 00 80    mov dword [ebp+0x08], 0x8000FFFF
```

`[ebp+0x08]` is the caller's HRESULT out-slot (the codec's
`Receive` implementation stores its return there before falling
through to the cleanup tail at `0x1736..0x176c`, which finally
loads `eax = [ebp+0x08]` at `0x176c` and returns).

## How the bail-out is reached

The trap function is the input-pin `Receive` body at RVA `0x1501`.
Its main loop is `0x157a..0x172a`:

```text
0x157a: 39 5d f4                cmp  [ebp-0x0c], ebx       ; bytes remaining?
0x157d: 0f 86 e0 01 00 00       jbe  +0x1e0 → 0x1763      ; exit loop if 0
0x1583..0x163f:                  loop body                 ; setup args, drive inner decode
0x1643: e8 3f b2 00 00          call 0xc887               ; INNER_DECODE
0x1648: 3b c3                   cmp  eax, ebx              ; eax == 0 ?
0x164a: 89 45 08                mov  [ebp+0x08], eax       ; stash as HRESULT (so far 0)
0x164d: 0f 85 e3 00 00 00       jnz  +0xe3 → 0x1736       ; bail if INNER_DECODE failed
0x1653: 39 5d f0                cmp  [ebp-0x10], ebx       ; samples produced ?
0x1656: 75 15                   jne  +0x15 → 0x166d       ; insert+release if so
0x1658: 39 5d dc                cmp  [ebp-0x24], ebx       ; first time here ?
0x165b: 0f 85 ce 00 00 00       jnz  +0xce → 0x172f       ; ←── BAIL-OUT
0x1661: c7 45 dc 01 00 00 00    mov  [ebp-0x24], 1         ; mark "we've seen no-output once"
0x1668: e9 8c 00 00 00          jmp  +0x8c → 0x16f9       ; skip to release tail
... (release + back-edge at 0x172a) ...
```

`[ebp-0x24]` is the "we already drained one input frame without
producing output" flag.  Its initial value is `0` (zeroed at
`0x1571: 89 5d dc`); the first time the loop completes a decode
that returns `S_OK` without writing to its `&[ebp-0x10]` "samples
produced" slot, the flag is set to `1` at `0x1661` and the loop
re-iterates.  On the **second** such call, the JNZ at `0x165b` is
taken and control flows to `0x172f` where the `mov [ebp+0x08],
0x8000FFFF` (`E_UNEXPECTED`) is stamped.

## What "no output produced" means

The inner decode call (RVA `0xc887`) is a `__thiscall` with 9
stack args:

| stack offset (callee view) | source (caller `[ebp-X]`)       | role            |
|----------------------------|---------------------------------|-----------------|
| `[ebp+0x08]`               | `[ebp-0x14]`                    | input pointer   |
| `[ebp+0x0c]`               | `[ebp-0x0c]`                    | bytes available |
| `[ebp+0x10]`               | `&[ebp-0x40]`                   | out-struct A    |
| `[ebp+0x14]`               | `[ebp-0x3c]`                    | flag/length     |
| `[ebp+0x18]`               | `[ebp-0x38]`                    | flag/length     |
| `[ebp+0x1c]`               | `&[ebp-0x10]`                   | out: samples produced flag |
| `[ebp+0x20]`               | `[ebp-0x20]`                    | flag/length     |
| `[ebp+0x24]`               | `[ebp-0x1c]`                    | flag/length     |
| `[ebp+0x28]`               | `&[ebp-0x2c]`                   | out-struct B    |
| (`ecx`)                    | `[esi+0xa4]`                    | inner context   |

The inner decode body:

```text
0xc887: 55 8b ec ...           prologue
0xc890: 39 45 08               cmp [ebp+0x08], eax (=0)
0xc893..c897:                   spill ecx = inner_ctx
0xc898: 0f 84 cb 00 00 00      jz  +0xcb → 0xc969     ; arg0 == 0 → E_FAIL
0xc89e: 8b 5d 10               mov ebx, [ebp+0x10]    ; arg2
0xc8a1: 3b d8                  cmp ebx, eax (=0)
0xc8a3: 0f 84 c0 00 00 00      jz  +0xc0 → 0xc969     ; arg2 == 0 → E_FAIL
0xc8a9: 39 45 14                cmp [ebp+0x14], eax
0xc8ac: 0f 84 b7 00 00 00      jz  → 0xc969           ; arg3 == 0 → E_FAIL
0xc8b2: 8b 7d 1c                mov edi, [ebp+0x1c]   ; arg5 (= &samples_produced)
0xc8b5: 3b f8                  cmp edi, eax (=0)
0xc8b7: 0f 84 ac 00 00 00      jz  → 0xc969           ; arg5 NULL → E_FAIL
... main decode body 0xc8bd..0xc92c ...
0xc92c: e8 44 00 00 00          call 0xc975           ; inner-inner decode
0xc931: 85 c0                  test eax, eax
0xc935: 75 36                  jnz +0x36 → 0xc96d    ; bail to E_FAIL (mov eax, 0x80004005)
... post-call accounting at 0xc937..0xc962 ...
0xc962: eb 8e                  jmp -0x72 → 0xc8f2    ; loop back
... success exit at 0xc965 ...
0xc965: 8b 45 1c               mov eax, [ebp+0x1c]   ; eax = arg5 = sample-out pointer
0xc968: eb 05                  jmp +5 → 0xc96f       ; skip E_FAIL load
0xc96a: b8 05 40 00 80         mov eax, 0x80004005   ; E_FAIL (unreached on success)
0xc96f: 5f 5e 5b c9            epilogue
0xc973: c2 24 00               ret 0x24              ; 9 stdcall args
```

Because the caller observes `eax == 0` at `0x1648`, the inner
decode entered the **success-exit** path BUT the `[ebp-0x10]`
slot the caller passed by-ref as arg5 remains `0`.  The success
exit returns `eax = arg5_pointer` itself — a non-zero pointer —
yet the caller's `cmp eax, ebx` shows `eax == ebx`.  This means
the actual path returning `eax == 0` is somewhere else in the
function (one of the post-`call 0xc975` jumps lands at `0xc8f2`
and the loop ultimately reaches a different exit that returns
`eax = 0`).

In short, **the inner decode swallowed our input frame without
emitting samples, returning `S_OK + samples_produced = 0`**, and
the outer `Receive` loop then bails because two consecutive
no-output iterations is interpreted as "stream cannot make
progress".

## Diagnosis

There are four candidate root causes for the "no samples
produced" outcome.  We have not yet narrowed to exactly one:

1. **ASF framing isn't stripped** — `IMemInputPin::Receive` is
   called with the raw ASF data-packet body, not raw codec
   frames.  ASF wraps each compressed frame with a Payload Parsing
   Information byte, payload length encoding, and possibly a
   replicated-data block.  The codec expects the bitstream
   immediately following the ASF parser; our scaffold doesn't run
   an ASF demuxer.
2. **Codec-private-data (extradata) wasn't installed** — the
   `wma_criteria_passing` `AmtBlueprint` carries `cbSize` bytes of
   `WAVEFORMATEX` tail but our scaffold sets it to 0.  Real WMA
   streams attach a per-stream codec-private block here that the
   codec snapshots during `ReceiveConnection` and consults during
   decode setup.
3. **`Pause` / `Run` didn't drive the codec's internal
   initialisation path** — the same path that, when correctly
   wired, would also set the `helper_struct[+0x20]` flag (round-63
   workaround surface), would presumably also populate the inner
   context at `[esi+0xa4]`.  Without that init, the inner decode's
   internal state machine never reaches a "decode-frame" state.
4. **IMediaSample setters (`SetMediaTime`, `SetDiscontinuity`,
   `SetSyncPoint`) need richer values** — the codec may demand
   monotonically-increasing media times or use `IsDiscontinuity`
   as a flush trigger.

Phase 5's setter panel runs all 6 plausible combinations of
sync-point / media-time / discontinuity; all return the same
`hr = 0x8000ffff` with the same trace pattern — **so the failing
check is not IMediaSample-side**.  That rules out (4).

## Round-65 hand-off

The structurally cleanest fix is (3) — drive the proper
`JoinFilterGraph` + `Pause` + (possibly) `IFilterGraph::Run` path
so the codec's own initialisation populates BOTH
`helper_struct[+0x20]` (retiring the round-63 patch) AND the
inner-context fields the decode needs.  This is the
[`msadds32-receive-null-0x20.md`](msadds32-receive-null-0x20.md)
round-64 hand-off note bullet (1).

If (3) doesn't change the outcome, round 65 should attempt (2) by
extracting the codec-private-data from the ASF fixture's stream-
properties object and passing it as the `WAVEFORMATEX` tail.

If (2) doesn't change the outcome, round 65 should investigate (1)
— specifically, find where the codec reads the input pointer
(at `[ebp-0x14]` in the outer Receive, which becomes arg0 of the
inner decode) and trace the first 16 bytes it consumes against
an ASF Payload Parsing dump.

The round-64 test harness at
`tests/round64_msadds32_e_unexpected.rs` pins the four structural
sentinels (bail-out site, guard JNZ, guard CMP, loop back-edge)
so round 65 can replay this state without re-tracing.

## Workaround status

`Sandbox::msadds32_patch_helper_addref` (round 63) is **still
required** in round 64.  Without it, `Receive` traps at RVA
`0x256a` before reaching the inner decode at all (regression
guarded by `phase4_workaround_regression_guard`).  Round 65 may
retire it iff path (3) sets `helper_struct[+0x20]` natively.
