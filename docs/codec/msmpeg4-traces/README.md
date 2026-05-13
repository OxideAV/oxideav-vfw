# MS-MPEG-4 v3 (DIV3 / MP43) LUT-read trace artifacts

Round 66 deliverable.  One JSONL artifact per fixture, captured
by driving `mpg4c32.dll` (the Windows Media Player 8 redist
build of the MS-MPEG-4 v3 VfW decoder) through the
`oxideav-vfw` interpreter with `Sandbox::watch` armed on every
candidate VLC / scan-permutation table region listed in
[`../msmpeg4-mpg4c32-rdata-map.md`](../msmpeg4-mpg4c32-rdata-map.md).

Intended consumer: the docs collaborator who is blocked on
producing the G0..G3 packed-Huffman + alternate-MV VLC tables
for the workspace's clean-room trace docs at
`docs/video/msmpeg4/`.  Each JSONL record shows precisely which
LUT byte the codec read at a given guest EIP for a given
fixture's bitstream.

## Reproduction

```text
CARGO_TARGET_DIR=/tmp/oxideav-vfw-r66-target \
  cargo run --release -p oxideav-vfw \
    --features trace --example gen_msmpeg4_traces
```

The example binary lives at
[`../../../examples/gen_msmpeg4_traces.rs`](../../../examples/gen_msmpeg4_traces.rs).
It loads `docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/mpg4c32.dll`,
walks each fixture's bitstream via the AVI extractor in
`tests/common/avi_extractor.rs`, and writes one JSONL file per
fixture into this directory.

## Per-fixture artifact map

| Fixture                            | JSONL              | Frames | Exercises                                          |
| ---------------------------------- | ------------------ | -----: | -------------------------------------------------- |
| `gop-30-352x288`                   | `gop-30-352x288.jsonl`             | 6 | 6-frame deterministic GOP at CIF                  |
| `with-skip-mbs-352x288`            | `with-skip-mbs-352x288.jsonl`      | 5 | testsrc2 at qscale=16, ~38% SKIP MBs              |
| `motion-pan-352x288`               | `motion-pan-352x288.jsonl.gz`      | 4 | Mandelbrot pan: large-magnitude inter-frame MVs   |
| `intra-pred-active-352x288`        | `intra-pred-active-352x288.jsonl`  | 1 | mandelbrot intra-pred direction switching         |
| `qscale-high-352x288`              | `qscale-high-352x288.jsonl`        | 1 | I-frame at qscale=31, sparse coefficients         |
| `qscale-low-352x288`               | `qscale-low-352x288.jsonl`         | 1 | I-frame at qscale=2, dense coefficients           |
| `i-only-352x288-cif`               | `i-only-352x288-cif.jsonl`         | 1 | testsrc I-frame at CIF                            |
| `tiny-i-only-176x144`              | `tiny-i-only-176x144.jsonl`        | 1 | minimal QCIF baseline                             |
| `fourcc-MP43`                      | `fourcc-MP43.jsonl`                | 1 | identical bitstream to `tiny-i-only-176x144`      |
| `i-frame-then-p-frame-176x144`     | `i-frame-then-p-frame-176x144.jsonl` | 2 | I + P at QCIF                                   |

Where a single trace exceeds 1 MB uncompressed it is committed
gzipped (filename ends `.jsonl.gz`).  `gunzip --keep` restores
the raw text.

## JSONL schema

Each line is one of:

```json
{"kind":"mem_read", "addr":"0xVA8",  "size":N, "value":"0xVALN", "eip":"0xVA8"}
{"kind":"win32_call","dll":"name", "name":"sym",   "args":[…], "ret":"0xVAL8","eip":"0xVA8"}
```

Filter for VLC-LUT reads with:

```text
jq -c 'select(.kind=="mem_read")' <fixture>.jsonl
```

The `addr` field is an absolute guest VA.  The image base is
`0x1c200000` (mpg4c32.dll's PE32 preferred base, used directly —
the round-1 loader does not rebase), so RVA = `addr - 0x1c200000`.

## Empirical finding — only the scan-permutation tables are hot

Aggregating mem_read events across all 10 fixtures:

| Candidate RVA region   | Bytes  | Unique addrs read | Coverage |
| ---------------------- | -----: | ----------------: | -------: |
| `0x0003a4c8` bootstrap |     64 |                 0 |     0 %  |
| `0x0003a708` DC-size   |    128 |                 0 |     0 %  |
| `0x0004f938` AC-coef G0| 16 376 |                 0 |     0 %  |
| `0x00053940` fan-out   |  1 024 |                 0 |     0 %  |
| `0x00053d42` MV-VLC    |  1 660 |                 0 |     0 %  |
| `0x000543c0` MB-type   |    510 |                 0 |     0 %  |
| `0x000545c0` AC-coef G1| 12 288 |                 0 |     0 %  |
| **`0x00057860` scan-a**|    168 |               144 |  85.7 %  |
| `0x00057bf0` scan-b    |    186 |                 4 |   2.2 %  |
| `0x00057f00` scan-c    |    148 |                 0 |     0 %  |
| `0x000581a8` scan-d    |    132 |                 0 |     0 %  |
| **`0x00058230` scan-e**|    102 |                96 |  94.1 %  |
| **`0x0005844c` scan-f**|     74 |                71 |  95.9 %  |

The MP43 decode hot loop reads ONLY from the small scan-
permutation tables in `0x57800..0x58500`.  The two big packed
AC-coefficient LUTs at `0x4f938` and `0x545c0` — which were the
strongest a-priori candidates for the docs collaborator's G0..G3
packed-Huffman tables — are NEVER touched during decode, across
every one of the 10 fixtures.

This is a **substantive empirical finding for the docs
collaborator**.  It means one of the following is true and the
docs collaborator should resolve which:

1. **The decoder embeds the AC Huffman tables in code.**
   Bit-arithmetic / shift-and-mask sequences inside the entropy
   loop at `.text` RVAs `0x16e42`, `0x16ea8`, `0x16f2f`, and
   `0x15f33` reconstruct the symbols inline, with no LUT load.
   The big tables at `0x4f938`/`0x545c0` would then be encoder-
   side helpers (used by `ICCompress`) not decoder-side.

2. **The decoder copies the AC LUTs to heap at codec-instance
   init time.**  Round 23's `DRV_OPEN` / `ICDecompressBegin`
   paths allocate state via `kernel32!HeapAlloc`; if the table
   bytes are memcpy'd into that heap, the runtime decode reads
   would be heap-VA reads, invisible to a `.data` watchpoint.
   To verify, install `WatchMode::Read` on the entire heap
   arena (`0x6000_0000..0x7000_0000`) before `ic_decompress` and
   look for read clusters whose values match the `.data`
   contents at `0x4f938`/`0x545c0`.

3. **The big LUTs are dead.**  Some MSVC release builds emit
   `.data` blocks that the linker did not garbage-collect; the
   tables would then be a relic of a different (encoder /
   wmvds32) decode path that was never reached in our fixtures.

Hypotheses 1 and 2 are equally plausible from the trace alone.
Hypothesis 1 implies the docs collaborator needs to disassemble
the entropy hot loop (eips `0x16e42` / `0x16ea8` / `0x16f2f` /
`0x15f33`) to recover the table contents arithmetically.
Hypothesis 2 implies a follow-up round that watches the heap
during init.  Hypothesis 3 is the least useful but is testable
by simply searching the `.text` for `mov` instructions whose
operand RVA lands in `0x4f938..0x53930` or `0x545c0..0x575c0` —
if there are none, the tables are dead.

## Caller EIPs at the entropy hot loop

The five `.text` addresses that perform LUT reads during decode
across every fixture, in descending hit count:

| `.text` RVA  | Read count (sum) | Reads from        |
| ------------ | ---------------: | ----------------- |
| `0x00016e42` |           26 919 | scan-a / scan-f   |
| `0x00015f33` |            3 860 | scan-e            |
| `0x00016ea8` |            2 630 | scan-f            |
| `0x00016f2f` |              109 | scan-b            |
| `0x0001601c` |               45 | scan-a            |
| `0x00015f97` |               40 | scan-e            |

Disassembling these six EIPs is the docs collaborator's
shortest path to ground-truth on the entropy decode loop.

## Provenance

Trace JSONL emitted by the
[`oxideav-vfw`](../../..) crate built with `--features trace`.
The trace emitter sits inside `Mmu::load{8,16,32,64}` and only
fires for ranges previously armed via `Sandbox::watch` — by
construction it surfaces the exact set of LUT bytes the codec
read, with no transformation.

Workspace policy: `mpg4c32.dll` itself is a Microsoft-shipped
redistributable binary used here as a black-box validator,
per the project's "binaries OK as black-box validators"
clean-room policy.  None of `mpg4c32.dll`'s `.text` was
disassembled to produce this trace map; the entry to disassemble
the entropy loop is the docs collaborator's task.
