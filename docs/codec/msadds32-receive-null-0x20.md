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

## Round-63 blocker

The next round needs to do one of:

1. **Pin the value of `edi_addref_result` at populator entry
   under our run**, then identify why
   `(edi_addref_result * 10) / helper(...)` is rounding to 0.
   This likely requires either:
   - Inspecting `this->helper_90` and `this->helper_90 + 0x1c`
     to see whether the helper object was properly initialised
     by `JoinFilterGraph` / `Pause`.
   - Disassembling `helper_addref` (RVA 0x5ce8) and `helper`
     (RVA 0x6ceb) to understand exactly what they compute.
2. **Drive `NewSegment` correctly** — verify the codec's
   IPin vtable has NewSegment at slot 17 (not at some non-
   standard index) and pass the args with the exact codec-
   expected encoding (REFERENCE_TIME LONGLONG + double).
3. **Pre-seed the codec's `this[0x160]` LIFO head** with a
   host-minted node so the populator's POP path bypasses the
   malloc+init failure entirely.  Note this only fixes one of
   two simultaneous NULL fields (the `[esi+0x160]` read at the
   trap site), not the `[ecx]` read where ecx is the caller's
   out-slot — so additional wiring is needed to ensure the
   populator's POP also writes that slot.

The forensics test harness in
`tests/round62_msadds32_null_0x20_forensics.rs` captures the
trap state + disassembly windows + IAT resolution + visited-EIP
set for the receive body, so each subsequent round can replay
without re-disassembling from raw bytes.
