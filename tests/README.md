# oxideav-vfw test fixtures

**Codec DLLs are never committed to this repository.** The
`tests/common/mod.rs` `fetch_or_load(name)` helper resolves
DLL bytes via, in order:

1. **`OXIDEAV_VFW_FIXTURE_DIR`** env var. If set, the helper
   reads `<dir>/<name>` (case-insensitive on the filename).
2. **Wine prefix** (Linux + macOS):
   `~/.wine/drive_c/windows/system32/`, then
   `~/.wine/drive_c/windows/syswow64/`. The 32-bit DLLs live
   in `syswow64` on a 64-bit Wine prefix.
3. **System paths** (Windows host): `%SystemRoot%\\SysWOW64\\`,
   then `%SystemRoot%\\System32\\`.
4. **Local cache**: `$CARGO_TARGET_DIR/test-fixture-cache/<NAME>`
   (default `target/test-fixture-cache/<NAME>`). Created on
   first successful HTTPS fetch.
5. **HTTPS fetch** from
   `https://samples.oxideav.org/codecs/windows/IV5PLAY/<NAME>`,
   then write to the cache.

**Exception**: when `CI=true` the cache is bypassed in both
directions. Every CI run exercises the network path so a stale
cache cannot mask a regression.

Round 3 dropped the `test-fixtures` Cargo feature. The fixture
helper handles every path on its own; CI runs the staged-DLL
tests every build.

## Available test fixtures (Intel IV5 driver bundle)

The full Intel IV5 redistributable is at:

* https://samples.oxideav.org/codecs/windows/IV5PLAY.EXE
  *— the original Dell-bundled installer (R19770;
  https://www.dell.com/support/home/en-us/drivers/driversdetails?driverid=r19770).
  Self-extracting CAB archive containing every component below.
  Use `cabextract IV5PLAY.EXE` if you want them in one shot, or
  fetch each one individually:*

Individual DLLs (replace the trailing filename to access each):

| Filename | Codec | Type | Notes |
|----------|-------|------|-------|
| `IR32_32.DLL` | Indeo 3 (RT21 / IV31) | VfW | Pre-MMX, simplest legacy fixture (round-3 target) |
| `IR41_32.AX`  | Indeo 4 (IV41) | DirectShow filter | `.AX` = DirectShow ActiveMovie module |
| `IR50_32.DLL` | Indeo 5 (IV50) | VfW | Uses MMX heavily (defer until MMX lands) |
| `IAC25_32.AX` | Indeo Audio (IAC25) | DirectShow | Audio codec |
| `IACENC.DLL`  | Indeo Audio encoder | VfW/ACM | |
| `NPINDEO.DLL` | Netscape plugin | NSAPI | Browser-side player; not used by oxideav-vfw |

Base URL: `https://samples.oxideav.org/codecs/windows/IV5PLAY/`.
Each filename above is a path under that base.

## Test paths

* `synth_dll_main_returns_through_sentinel`
  (`m1_load_dll_main.rs`) builds a minimal PE32 DLL byte-by-byte
  from the public Microsoft PE/COFF specification and exercises
  the full round-1 stack (MMU + integer ISA + PE loader +
  kernel32 stubs). No network. Always runs.

* `staged_codec_dll_lists_round_four_todo_imports`
  (`m1_load_dll_main.rs`) fetches `IR32_32.DLL` via the helper
  and asserts the exact set of Win32 imports the round-1 +
  round-2 stub registry does not yet satisfy. That set is
  round 4's todo list (49 entries: 8 gdi32, 24 kernel32, 16
  user32, 1 winmm). When round 4 closes a gap, this test fails
  — that is the trigger to update the asserted set.

* `synth_codec_walks_full_ic_pipeline`
  (`m2_indeo3_driverproc.rs`) drives the full
  `Sandbox::install_codec → ic_open → ic_decompress_query →
  ic_decompress_begin → ic_decompress → ic_decompress_end →
  ic_close` pipeline against a hand-rolled synthetic codec.
  No network. Always runs.

* `indeo3_driverproc_open_getinfo_close_smoke`
  (`m2_indeo3_driverproc.rs`) is forward-compatible: while
  `Sandbox::load(IR32_32.DLL)` rejects with
  `UnknownImportFunction` (round-3 state), the test asserts
  the rejection. Once round 4 lands the missing stubs and the
  load succeeds, the test walks
  `DllMain → ICOpen('VIDC','IV31',ICMODE_DECOMPRESS) →
  ICGetInfo → ICClose`, decoding `szName` from the codec's
  `ICINFO` block and asserting it is non-empty + ASCII-printable.

## Provenance

The IV5 driver bundle is freely redistributable under the
original Intel/Dell terms as packaged in R19770. No relicensing,
no rebundling — the URLs above are the canonical legitimate
source for the project's testing.

The synthesised-PE test path remains the no-network fallback so
CI is not strictly tied to the URL host being up. If
`samples.oxideav.org` is unreachable, the staged-DLL tests fail
loudly (so the failure is visible) rather than silently skipping.
