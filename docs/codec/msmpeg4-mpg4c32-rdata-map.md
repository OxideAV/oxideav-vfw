# `mpg4c32.dll` Data-Section Map (MS-MPEG-4 v3 / DIV3)

This document maps the read-only data region of `mpg4c32.dll`
(the Microsoft Video for Windows MS-MPEG-4 v3 VfW decoder) to
candidate Huffman / VLC lookup-table regions that the docs
collaborator can transcribe into the workspace's clean-room
trace docs at `docs/video/msmpeg4/`.

**Reference binary** —
`docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/mpg4c32.dll`
(420 240 bytes, ImageBase `0x1c200000`, build 8.00.0.4477 from
Windows Media Player 8 redistributable).

**Tooling** — `oxideav-vfw`'s own PE-32 parser plus the trace
infrastructure under the `trace` Cargo feature.  No external
disassemblers, no Wine / ReactOS / MinGW / Microsoft DShow
source was consulted while compiling this map.

## PE Section Table

`mpg4c32.dll` exposes only four sections; unlike most PE32 DLLs
it has **no separate `.rdata`** — read-only data sits inside
`.data` (mapped R+W; the loader leaves it writeable for the IAT
patch). The VLC tables that the codec's decode hot loop reads
live in `.data`.

| Name     | RVA          | VirtualSize  | FileOff      | RawSize      | Perm |
| -------- | ------------ | ------------ | ------------ | ------------ | ---- |
| `.text`  | `0x00001000` | `0x00038db9` | `0x00000600` | `0x00038e00` | RX   |
| `.data`  | `0x0003a000` | `0x00074560` | `0x00039400` | `0x00028200` | RW   |
| `.rsrc`  | `0x000af000` | `0x00000958` | `0x00061600` | `0x00000a00` | R    |
| `.reloc` | `0x000b0000` | `0x00002860` | `0x00062000` | `0x00002a00` | R    |

(The `winxp/mpg4c32.dll` build shares the same layout shape, with
smaller `.text` (`0x2a709`) and `.data` (`0x415d8`) sizes; this
map enumerates RVAs in the wmpcdcs8-2001 build because that's the
build the existing round-20…round-54 corpus drives.)

## Candidate Huffman / VLC LUT Regions in `.data`

Identified heuristically: the `.data` raw bytes were swept for
runs of u16 entries where the **low byte is in `[1..24]`** (i.e.
a plausible VLC code length) and **at least four distinct length
values** appear across the run (rules out constant fillers and
relocation-stamp data).  Each candidate is a multi-entry packed
`(length, symbol)` LUT — the format MSVC's `__inline` Huffman
decoders typically generate, where one u16 holds `length` in the
low byte and the decoded symbol / next-table-index in the high
byte.

Eyeball inspection of the first 16 entries at each candidate's
RVA distinguishes:

* **Packed LUT** (real VLC table): entries vary in both byte
  components; lengths range over a wide span (e.g. 8..18).
* **Decoded fan-out LUT** (next-state precomputed): entries
  repeat in pairs / quads / octets — the codec's decode loop
  indexes by `next N bits`, so multiple input codes route to the
  same `(length, symbol)` slot.

Sorted by RVA:

| RVA          | Bytes  | Entries (u16) | Shape (lo=length) | Likely role                                                        | Confidence |
| ------------ | -----: | ------------: | ----------------- | ------------------------------------------------------------------ | ---------- |
| `0x0003a4c8` |     64 |            32 | lens `{1,4..10}`  | small bootstrap table, possibly DC-size or MB-type                 | medium     |
| `0x0003a708` |    128 |            64 | lens `{2..19}`    | DC-coefficient delta-size VLC (intra), 64 = `2 * 32` codes         | high       |
| `0x0004f938` | 16 376 |         8 188 | lens `{12..13}`   | combined intra+inter AC-coefficient `(run, level, last)` LUT       | **high**   |
| `0x00053940` |  1 024 |           512 | lens `{6}` & `{6}`| fan-out routing LUT into AC-coef LUT (`2^9 = 512` keys)            | medium     |
| `0x00053d42` |  1 660 |           830 | lens `{7..9}`     | MV component VLC (signed deltas, `[-128..+127]` ≈ 256 ± fan-out)   | **high**   |
| `0x000543c0` |    510 |           255 | lens `{2..8}`     | MB-type (`MBTYPE` / `mbtype_alt`) VLC, ≤ 256 codes                 | high       |
| `0x000545c0` | 12 288 |         6 144 | lens `{2..11}`    | alternate AC-coef LUT (inter-only) — second G-table family         | **high**   |
| `0x00057860` |    168 |            84 | seq u8/u16 ids    | zigzag scan-order or coef-position permutation                     | low        |
| `0x00057bf0` |    186 |            93 | seq u8/u16 ids    | alternate-scan order table                                         | low        |
| `0x00057f00` |    148 |            74 | seq u8/u16 ids    | scan permutation; smaller block-shape variant                      | low        |
| `0x000581a8` |    132 |            66 | seq u8/u16 ids    | scan permutation                                                   | low        |
| `0x00058230` |    102 |            51 | seq u8/u16 ids    | scan permutation                                                   | low        |
| `0x0005844c` |     74 |            37 | seq u8/u16 ids    | scan permutation                                                   | low        |

### Sample decoded entries

**`0x0003a708` (DC-size VLC family, 64 × u16):**

```
[ 0] 0x0d07  length= 7  symbol_byte=13
[ 1] 0x0f0c  length=12  symbol_byte=15
[ 2] 0x120c  length=12  symbol_byte=18
[ 3] 0x1011  length=17  symbol_byte=16
[ 4] 0x110c  length=12  symbol_byte=17
[ 5] 0x1010  length=16  symbol_byte=16
[ 6] 0x120d  length=13  symbol_byte=18
[ 7] 0x1010  length=16  symbol_byte=16
```

**`0x0004f938` (combined intra+inter AC-coef LUT, 8 188 × u16):**

```
[ 0] 0x400d  length=13  symbol_byte=0x40  (last=1, run=0,  level=0)
[ 1] 0x000d  length=13  symbol_byte=0x00  (last=0, run=0,  level=0)
[ 2] 0x3f0d  length=13  symbol_byte=0x3f  (last=0, run=0,  level=63)
[ 3] 0x010d  length=13  symbol_byte=0x01  (last=0, run=0,  level=1)
[ 4] 0x3e0c  length=12  symbol_byte=0x3e  (last=0, run=0,  level=62)
[ 5] 0x3e0c  length=12  symbol_byte=0x3e  (duplicate fan-out)
[ 6] 0x020c  length=12  symbol_byte=0x02  (last=0, run=0,  level=2)
[ 7] 0x020c  length=12  symbol_byte=0x02  (duplicate fan-out)
```

The repeating-pair structure (`[4]==[5]`, `[6]==[7]`, …) is a
strong signal that this is a fan-out LUT — multiple raw codes
land at the same decoded `(last,run,level)` triple because the
decoder is indexed by `peek_next_13_bits` and a 12-bit code
occupies two adjacent 13-bit slots.

**`0x000545c0` (alternate AC-coef LUT, 6 144 × u16):**

```
[ 0] 0xff0b  length=11  symbol_byte=0xff  (EOB / escape)
[ 1] 0x0c0b  length=11  symbol_byte=12
[ 2] 0x0b0a  length=10  symbol_byte=11
[ 3] 0x0b0a  length=10  symbol_byte=11   (fan-out duplicate)
[ 4] 0x0a09  length= 9  symbol_byte=10
[ 5] 0x0a09  length= 9  symbol_byte=10
[ 6] 0x0a09  length= 9  symbol_byte=10
[ 7] 0x0a09  length= 9  symbol_byte=10
```

## Coverage Expectations

The decode-hot-loop's hot LUT reads will come from a small
subset of these candidates:

| Candidate         | Expected per-frame reads on a 352×288 I-frame   |
| ----------------- | ----------------------------------------------- |
| `0x0004f938` (AC) | thousands (one per DCT coefficient × 6 blocks)  |
| `0x000545c0` (AC) | thousands (alternate scan in P frames)          |
| `0x00053d42` (MV) | dozens (one per MB motion vector component)     |
| `0x000543c0` (MB) | dozens (one per macroblock)                     |
| `0x0003a708` (DC) | dozens (one per intra block, when DC is coded)  |
| `0x00053940` (fanout)| as above for fan-out routing                 |
| `0x0003a4c8`      | a few (bootstrap)                               |
| `0x00057860..`    | indirectly via zigzag scan permutation          |

For the smaller fixtures (`tiny-i-only-176x144` = QCIF I-only =
99 MBs ≈ 4 000 coefficients) the AC-LUT reads still dominate.

## Next Steps for the Docs Collaborator

For each candidate region in the table above, the per-fixture
trace JSONLs under `crates/oxideav-vfw/docs/codec/msmpeg4-traces/`
record exactly which u16 entries the codec reads for that
fixture's bitstream.  Specifically:

1. **Filter** each JSONL for `"kind":"mem_read"` events whose
   `addr` falls in the RVA range of one candidate.  The `value`
   field is the u16 the codec consumed.
2. **Aggregate** across the 10 fixtures (5 multi-frame + 5
   single-I-frame): the union of unique addresses + values
   covers the LUT entries the docs collaborator needs to
   reconstruct G0..G3 (the 4 packed-Huffman tables MS-MPEG-4 v3
   uses for AC coefficients + alt-MV).
3. **Cross-check** with the per-table count expectations in the
   "Coverage Expectations" table above.

The traces here do **not** disassemble the codec's `.text` — that
would require a separate effort, and is not what the docs
collaborator is blocked on.  The blocker is the **table
contents**, and the table contents are what the LUT-region
memory reads directly surface.

## Provenance

This map was produced by walking the PE-32 byte layout against
the Microsoft PE/COFF Specification (revision 11.0) alone.  The
LUT-region heuristic (low-byte ∈ `[1..24]` + ≥ 4 distinct length
values across a ≥ 16-entry run) is a property of canonical
packed-Huffman LUT shape; it does not encode any assumption
about which entry maps to which symbol.
