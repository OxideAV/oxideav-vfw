# `msadds32.ax` — `IMemInputPin::Receive` NULL+0x20 trap (round-62 forensics)

This document captures the round-62 clean-room disassembly of the
`IMemInputPin::Receive` call site inside `msadds32.ax` that traps
with `memory fault at 0x00000020 (page unmapped)` after the full
round-61 allocator handshake + output-pin `ReceiveConnection`
have landed `S_OK`.

All decoding here is from raw `msadds32.ax` byte inspection
against Intel SDM Vol. 2 opcode tables, plus public MSDN COM ABI
references. **No Wine / ReactOS / MinGW / Microsoft DShow
base-class source was consulted.**

## Trap state (captured by `round62_msadds32_null_0x20_forensics`)

```
TRAP-EIP:   0x1c40256a   (image_base 0x1c400000, RVA 0x0000256a)
bytes:      89 72 20 8b 11 89 10 83 21 00 ff 15 40 f0 40 1c
trap kind:  memory fault at 0x00000020 (page unmapped)
```

Register file at the moment of trap:

```
eax = 0x600001f0   ecx = 0x900ffed8   edx = 0x00000000   ebx = 0x00000000
esp = 0x900ffe68   ebp = 0x900ffe74   esi = 0x00000000   edi = 0x600001d8
```

## Faulting instruction

`89 72 20` decodes (Intel SDM Vol. 2C, opcode table) as:

```
mov dword ptr [edx + 0x20], esi
```

With `edx = 0x00000000` and `esi = 0x00000000`, the store to
`[0 + 0x20]` traps because address `0x00000020` is on an unmapped
page.

## The trap function (RVA 0x2548..0x257f)

```
0x2548: 55              push    ebp
0x2549: 8b ec           mov     ebp, esp
0x254b: 56              push    esi
0x254c: 8b f1           mov     esi, ecx                  ; esi = this
0x254e: 57              push    edi
0x254f: 8d be 48 01 00 00  lea     edi, [esi + 0x148]    ; &critsec
0x2555: 57              push    edi
0x2556: ff 15 3c f0 40 1c  call    [0x1c40f03c]          ; EnterCriticalSection
0x255c: 8b 4d 08        mov     ecx, [ebp + 0x08]         ; ecx = &caller's out-slot
0x255f: 8d 86 60 01 00 00  lea     eax, [esi + 0x160]    ; eax = &this[0x160] (LIFO head)
0x2565: 57              push    edi
0x2566: 8b 11           mov     edx, [ecx]                ; edx = *out-slot (NULL!)
0x2568: 8b 30           mov     esi, [eax]                ; esi = this[0x160] (also NULL)
0x256a: 89 72 20        mov     [edx + 0x20], esi         ; <<< TRAP
0x256d: 8b 11           mov     edx, [ecx]
0x256f: 89 10           mov     [eax], edx
0x2571: 83 21 00        and     [ecx], 0
0x2574: ff 15 40 f0 40 1c  call    [0x1c40f040]          ; LeaveCriticalSection
0x257a: 5f              pop     edi
0x257b: 33 c0           xor     eax, eax
0x257d: 5e              pop     esi
0x257e: 5d              pop     ebp
0x257f: c2 04 00        ret     4
```

Reading this as C-ish pseudocode:

```c
void push_to_lifo(struct codec *this, void **pNode) {
    EnterCriticalSection(&this->cs_148);
    struct node *node = *pNode;            // <<< assumes *pNode != NULL
    node->next = this->lifo_head_160;
    this->lifo_head_160 = node;            // (effectively done in subsequent code)
    *pNode = NULL;
    LeaveCriticalSection(&this->cs_148);
}
```

This is a list-prepend / **LIFO push** onto `this->lifo_head_160`.
The function **has no NULL check** on `*pNode`; if the caller
passes a slot that has not been populated with a node pointer,
the dereference at `mov [edx + 0x20], esi` faults.

## The caller — `IMemInputPin::Receive` body at RVA 0x1501

The trap function is reached from inside the codec's
`Receive` implementation (the function whose prologue is at
RVA 0x1501, called by the host's `call_method(mip,
SLOT_MEMINPUTPIN_RECEIVE, &[sample])`). The relevant control-
flow tail of that function is:

```
0x1501: 55 8b ec 83 ec 50 53 56 8b f1 57 ...    prologue, esi = this
0x1545: 89 5d f8 89 5d fc 89 5d d8 ...         ; locals zeroed:
                                                ;   [ebp-0x04] = 0
                                                ;   [ebp-0x08] = 0
                                                ;   [ebp-0x28] = 0
...
0x1608: e8 51 0d 00 00      call    POP_buffer  ; → 0x235e (populator)
                                                  ; writes [ebp-0x04]
                                                  ; on success
0x160d: 85 c0               test    eax, eax
0x160f: 7c 0e               jl      +0x0e → 0x161e  ; skip read on fail
0x1611: 8b 45 fc            mov     eax, [ebp-0x04] ; <-- only on success
...                                                 ; main decode body
0x16ad: e8 1f fe ff ff      call    0x14c6      ; last decode step
0x16b2: 3b c3 89 45 08 0f 8c 84 00 00 00         ; check HRESULT
                                                ;   jl 0x1736 (error exit)
0x16b3: 8b 4d fc            mov     ecx, [ebp-0x04]
0x16b6: 3b cb               cmp     ecx, ebx
0x16b7: 74 40               jz      +0x40 → 0x16f9  ; SKIP insert if NULL
0x16b9..0x16f1:                                     ; populate node fields
0x16ee: e8 da 0d 00 00      call    INSERT_sorted ; → 0x24ce
0x16f3: c7 45 d8 01 00 00 00   mov  [ebp-0x28], 1 ; "did insert" flag
0x16fa: 8b 45 f8            mov     eax, [ebp-0x08] ; (also reached via jz)
0x16fd: 50 8b 08 ff 51 08   push eax; mov ecx,[eax]; call [ecx+0x08]  ; Release
0x1703: 39 5d d8            cmp     [ebp-0x28], ebx
0x1706: 89 5d f8            mov     [ebp-0x08], ebx
0x1709: 75 0b               jnz     +0x0b → 0x1716  ; skip push-to-LIFO if insert ran
0x170b: 8d 45 fc            lea     eax, [ebp-0x04]
0x170e: 8b ce               mov     ecx, esi
0x1710: 50                  push    eax
0x1711: e8 33 0e 00 00      call    LIFO_push   ; → 0x2548 (TRAP target)
```

The trap is reached when **all three** of these are true:

1. `[ebp-0x04]` is NULL at `0x16b3` (jz takes the branch).
2. `[ebp-0x08]` was non-NULL and was Released cleanly (no trap).
3. `[ebp-0x28]` is still 0 (because the jz at `0x16b7` skipped
   the block that sets it to 1 alongside the insert call).

Under these conditions, the codec falls through to push the
(still-NULL) `[ebp-0x04]` onto its LIFO free-pool — which is
the bug that produces the trap.

## Forensic finding: WHO leaves `[ebp-0x04]` NULL?

The phase-2d test (`phase2d_did_populator_run_in_receive_body`)
proves empirically that:

- The enclosing `Receive` body at RVA `0x1501` IS entered.
- The populator function at RVA `0x235e` (called from `0x1608`)
  IS entered.
- The list-insert function at RVA `0x24ce` is **NOT** entered.

So `Receive` calls the populator (`0x235e`), the populator
returns, but `[ebp-0x04]` ends up NULL — meaning the populator
took its **error-return path** rather than its POP-success path.

### The populator (RVA 0x235e)

```c
HRESULT populate_buffer(struct codec *this, struct node **out) {
    helper_addref(this->helper_90 + 0x1c);                // 0x2374
    int kind = (this->wFormatTag_a8 != 0x0160) + 1;       // 0x238b
    int helper_result = helper(this->nSamplesPerSec_ac,
                               this->wBitsPerSample_b6 *
                               this->nSamplesPerSec_ac,
                               this->nChannels_aa,
                               kind);                     // 0x23a6
    int edi_count = (edi_addref_result * 10) / helper_result;  // 0x23ad
    EnterCriticalSection(&this->cs_148);                  // 0x23c2
    if (this->lifo_head_160 == NULL) {
        void *buf = operator_new(40);                     // 0x23d4
        if (!buf) goto err;                               // 0x23dc
        buffer_pool_ctor(buf);                            // 0x23e0
        this->lifo_head_160 = buf;
        int rc = buffer_pool_init(buf, edi_count);        // 0x23ff
        if (rc < 0) {
            // free + clear head
            this->lifo_head_160 = NULL;
            goto leave_critsec;
        }
    }
    // POP path:
    struct node *head = this->lifo_head_160;              // 0x242e
    *out = head;
    this->lifo_head_160 = head->next;                     // (pop)
    head->next = NULL;
leave_critsec:
    LeaveCriticalSection(&this->cs_148);
    return rc_or_zero;
err:
    rc = E_OUTOFMEMORY;                                   // 0x23f3
    goto leave_critsec;
}
```

The `buffer_pool_init` at RVA `0x25ac` then itself calls
`operator new(edi_count)` and stashes the result at
`buf[0]`; if that allocation returns NULL (which our stub does
for `size == 0`), `init` returns `E_OUTOFMEMORY` and the
populator's caller will see `eax < 0`.

So the chain is:

```
populator(0x235e) → buffer_pool_init(0x25ac) → operator_new(edi_count)
```

If `edi_count` ends up 0 (e.g. the `helper` call at `0x23a6`
returns a value larger than `edi_addref_result * 10`),
`operator new(0)` returns NULL and the whole populator path
fails.  `[ebp-0x04]` stays NULL.  The cleanup path at
`0x170b..0x1711` then dereferences it via the LIFO-push
trap-function.

The exact value of `edi_addref_result` (returned from
`helper_addref` at `0x2374`) depends on a codec-internal state
field at `this->helper_90 + 0x1c` whose initialisation we
have not yet finished tracing.  That state is normally set up
either by the codec's own constructor path (the
`DllGetClassObject → CreateInstance` chain we drive in round
57+) or by an event the codec expects between connection-time
and `Receive` — e.g. an upstream-issued `IPin::NewSegment`.

Empirically, calling `IPin::NewSegment(start=0, stop=1s,
rate=1.0)` on the input pin (vtable slot 17) **also traps**:
the codec dereferences the rate-high dword `0x3FF00000` as a
pointer.  That suggests either the codec's `NewSegment` slot is
not at 17 on this binary OR the rate parameter encoding our
test uses doesn't match what the codec expects.

## Round 63 — resolution

The size-0 chain was pinned to `helper_addref` (RVA `0x5cea`)
returning 0 on a fresh codec instance.  Round-63 disassembly
recovered both helper functions exhaustively:

### `helper_size_calc` (RVA `0x6ced..0x6d92`)

```text
0x6ced: 55              push ebp
0x6cee: 8b ec           mov  ebp, esp
0x6cf0: 8b 45 18        mov  eax, [ebp+0x18]    ; eax = kind  (arg4)
0x6cf3: 57              push edi
0x6cf4: 8b 7d 08        mov  edi, [ebp+0x08]    ; edi = sps   (arg0)
0x6cf7: f7 d8           neg  eax
0x6cf9: 1b c0           sbb  eax, eax           ; eax = -1 if kind!=0 else 0
0x6cfb: 83 e0 1f        and  eax, 0x1f          ; eax = 0x1f if kind!=0 else 0
0x6cfe: 40              inc  eax                ; eax = base (1 or 32)

0x6cff..0x6d49:  sps-tier branch ladder (see below) → eax = base << shift

0x6d4a: 8b c8           mov  ecx, eax           ; ecx = frame_samples
0x6d4c: 8b c7           mov  eax, edi           ; eax = sps
0x6d4e: 99              cdq                     ; (sps is positive, edx=0)
0x6d4f: 2b c2           sub  eax, edx
0x6d51: 56              push esi
0x6d52: 8b f0           mov  esi, eax           ; esi = sps
0x6d54: 8b c1           mov  eax, ecx
0x6d56: 0f af 45 0c     imul eax, [ebp+0x0c]    ; eax = frame_samples * (wbps*sps)
0x6d5a: d1 fe           sar  esi, 1             ; esi = sps/2  (rounding)
0x6d5c: 03 c6           add  eax, esi
0x6d5e: 33 d2           xor  edx, edx
0x6d60: f7 f7           div  edi                ; eax = (frame_samples*wbps + sps/2)
                                                ;       *= sps / sps = frame_samples*wbps
0x6d62: 83 c0 07        add  eax, 7
0x6d65: c1 e8 03        shr  eax, 3             ; eax = ceil(byte_count/8)
0x6d68: 83 f8 01        cmp  eax, 1
0x6d6b: 77 1c           ja   0x6d89             ; if byte_count >= 16 → done
0x6d6d: 85 c0           test eax, eax
0x6d6f: 75 18           jnz  0x6d89             ; if byte_count >= 8  → done

0x6d71..0x6d87:  doubling loop — frame_samples *= 2 until byte_count >= 8

0x6d89: 8b c1           mov  eax, ecx           ; return frame_samples (NOT bytes)
0x6d8b: 5e              pop  esi
0x6d8c: eb 02           jmp  0x6d90
0x6d8e: 33 c0           xor  eax, eax           ; sps > 48000 early-return path
0x6d90: 5f 5d           pop  edi, pop ebp
0x6d92: c2 14 00        ret  0x14               ; 5 stdcall args × 4 bytes
```

Sample-rate / channel-count tier ladder at `0x6cff..0x6d49`:

| `sps` range       | extra condition | `shift` | `frame_samples` (kind=2) |
|-------------------|-----------------|---------|--------------------------|
| `≤ 8000`          | —               | 9       | 16 384                   |
| `8001..11025`     | —               | 9       | 16 384                   |
| `11026..16000`    | —               | 9       | 16 384                   |
| `16001..22050`    | —               | 10      | 32 768                   |
| `22051..32000`    | `ch == 1`       | 10      | 32 768                   |
| `22051..32000`    | `ch != 1`       | 11      | 65 536                   |
| `32001..44100`    | —               | 11      | 65 536                   |
| `44101..48000`    | —               | 11      | 65 536                   |
| `> 48000`         | —               | —       | early-return `eax = 0`   |

For the round-62 WMA2 AMT (`sps=44100, wbps=16, ch=1, kind=2`):
`frame_samples = 65536`.

### `helper_addref` (RVA `0x5cea..0x5cf6`)

```text
0x5cea: 83 79 20 00     cmp [ecx+0x20], 0       ; "initialised" flag
0x5cee: 74 04           jz  0x5cf4
0x5cf0: 8b 41 28        mov eax, [ecx+0x28]     ; cached value
0x5cf3: c3              ret
0x5cf4: 33 c0           xor eax, eax            ; uninitialised → return 0
0x5cf6: c3              ret
```

A trivial getter.  The matching setter at `0x5cf7..0x5d12`:

```text
0x5cf7: 8b 44 24 04     mov eax, [esp+4]
0x5cfb: 85 c0           test eax, eax
0x5cfd: 75 07           jnz +7
0x5cff: e8 0f 00 00 00  call <err helper>
0x5d04: eb 0a           jmp +0xa
0x5d06: c7 41 20 01 00 00 00  mov [ecx+0x20], 1 ; flag = 1
0x5d0d: 89 41 28        mov [ecx+0x28], eax    ; cached = arg
0x5d10: c2 04 00        ret 4
```

So the `[+0x20]` flag is normally set during the codec's
`JoinFilterGraph` / `Pause` initialisation — when the codec
itself calls `set_value(helper, n)` with a positive `n` derived
from its run-state machinery.  Our scaffold does not drive that
path, hence the field stays zero on a fresh instance.

### Workaround

Round 63 lands [`Sandbox::msadds32_patch_helper_addref`] which
overwrites the first 6 bytes of `helper_addref` with
`mov eax, imm32; ret` (encoding `b8 XX XX XX XX c3`).  Patching
with any `value ≥ 6554` (so `(value * 10) / 65536 ≥ 1`)
empirically clears the LIFO-push trap and lets `Receive` run to
completion.  The HRESULT changes from a memory fault at
`0x00000020` to `0x8000ffff` (E_UNEXPECTED from the codec's
inner decode body) — the round-64 investigation surface.

### Round-64 hand-off (resolved → round-65)

The patch is a debugging workaround.  Round 64 chose option (2)
from the original menu and traced the `0x8000ffff` HRESULT to its
emission site.  Findings:

  - The value is NOT emitted from any of the 10 `mov eax,
    0x8000FFFF` (`b8 ff ff 00 80`) sites in the binary's `.text`;
    none is reached during the patched `Receive`.
  - The actual emission is `c7 45 08 ff ff 00 80` at RVA
    `0x172f` — `mov dword [ebp+0x08], 0x8000FFFF` writes
    `E_UNEXPECTED` into the caller's HRESULT out-slot, which the
    function's epilogue at `0x176c` loads back into `eax` before
    returning.
  - The branch that leads to `0x172f` is `jnz +0xce → 0x172f` at
    RVA `0x165b`, controlled by `cmp [ebp-0x24], ebx` at RVA
    `0x1658`.  `[ebp-0x24]` is the "we already drained one input
    frame without producing output" loop-counter flag, set to
    `1` at RVA `0x1661` on the first no-output inner-decode call.
  - So the bail-out fires on the **second consecutive** outer-loop
    iteration where the inner decode at RVA `0xc887` returned
    `eax = 0` (S_OK) yet didn't write samples to its
    `&[ebp-0x10]` "samples produced" out-pointer.

Full round-64 forensics live in
[`msadds32-receive-e-unexpected.md`](msadds32-receive-e-unexpected.md);
that doc names three round-65 candidate fixes:

1. Drive the proper `JoinFilterGraph` / `Pause` /
   `IFilterGraph::Run` init path so the codec populates its
   inner context AND `helper_struct[+0x20]` (retiring the
   round-63 patch).
2. Install codec-private-data in the `WAVEFORMATEX` tail of the
   `AmtBlueprint` so the inner decoder can configure its state
   machine.
3. Strip ASF Payload Parsing framing from the input bytes before
   passing them to `Receive`.

The round-63 test harness in
`tests/round63_msadds32_buffer_size_calc.rs` pins both the
formula (phase-1/2) and the cleared trap (phase-4/5); the round-
64 harness in `tests/round64_msadds32_e_unexpected.rs` pins the
inner-decode-no-output bail-out path so the next round can
replay without re-disassembling.
