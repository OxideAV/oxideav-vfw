# oxideav-vfw test fixtures

**Codec DLLs are never committed to this repository and never
staged on disk.** The fixture-gated tests fetch them on demand
from `samples.oxideav.org` over HTTPS at test time, every run, as
many times as the test suite needs. There is no local cache, no
`tests/fixtures/` directory, no `fetch-fixtures.sh` script — the
fetch is part of each test's body.

This works because:

* Each DLL is the codec vendor's redistributable, legitimately
  hosted on `samples.oxideav.org` for the project's own
  development + CI use.
* The fetch is small (the DLLs are tens to a few hundred KB
  each); refetching every test run is fine.
* No on-disk state means no licensing-clarity question about the
  repo, no `.gitignore`-trap for accidentally-committed binaries,
  no stale-fixture maintenance burden.
* CI runs with network access; the `test-fixtures` Cargo feature
  is what gates the network-dependent tests so air-gapped
  environments still get the synthesised-PE coverage from the
  no-fixture path.

## Available test fixtures (Intel IV5 driver bundle)

The full Intel IV5 redistributable is at:

* https://samples.oxideav.org/video/windows/IV5PLAY.EXE
  *— the original Dell-bundled installer (R19770;
  https://www.dell.com/support/home/en-us/drivers/driversdetails?driverid=r19770).
  Self-extracting CAB archive containing every component below.
  Use `cabextract IV5PLAY.EXE` if you want them in one shot, or
  fetch each one individually:*

Individual DLLs (replace the trailing filename to access each):

| Filename | Codec | Type | Notes |
|----------|-------|------|-------|
| `IR32_32.DLL` | Indeo 3 (RT21 / IV31) | VfW | Pre-MMX, simplest legacy fixture |
| `IR41_32.AX`  | Indeo 4 (IV41) | DirectShow filter | `.AX` = DirectShow ActiveMovie module |
| `IR50_32.DLL` | Indeo 5 (IV50) | VfW | Uses MMX heavily |
| `IAC25_32.AX` | Indeo Audio (IAC25) | DirectShow | Audio codec |
| `IACENC.DLL`  | Indeo Audio encoder | VfW/ACM | |
| `NPINDEO.DLL` | Netscape plugin | NSAPI | Browser-side player; not used by oxideav-vfw |

Base URL: `https://samples.oxideav.org/video/windows/IV5PLAY/`.
Each filename above is a path under that base.

Example test-side fetch:

```rust
let url = "https://samples.oxideav.org/video/windows/IV5PLAY/IR32_32.DLL";
let bytes: Vec<u8> = ureq::get(url).call().unwrap().into_reader()
    .bytes().collect::<std::io::Result<_>>().unwrap();
let img = oxideav_vfw::pe::load_dll(&bytes)?;
// …
```

## Test paths

* The unconditional `synth_dll_main_returns_through_sentinel`
  test in `m1_load_dll_main.rs` builds a minimal PE32 DLL
  byte-by-byte from the public Microsoft PE/COFF specification
  and exercises the full round-1 stack (MMU + integer ISA + PE
  loader + kernel32 stubs). No network. Always runs.

* The `test-fixtures`-gated `staged_codec_dll_runs_dll_main_cleanly`
  test fetches one of the IV5 DLLs from the URL above and runs
  its `DllMain(DLL_PROCESS_ATTACH)` through the interpreter,
  expecting a clean return without an unhandled trap. If a trap
  fires, the trap variant + EIP point at exactly which Win32
  stub or ISA opcode the next round needs to add.

* Round-2's fixture-gated test in `m2_cinepak_decode.rs` was
  written for Cinepak's `iccvid.dll`, which is **not** in the
  IV5 bundle. Round 3 will rewrite this to target an Indeo
  fixture from the bundle (`IR32_32.DLL` is the round-3
  candidate; `IR50_32.DLL` for round-4 once MMX lands).

## Provenance

The IV5 driver bundle is freely redistributable under the
original Intel/Dell terms as packaged in R19770. No relicensing,
no rebundling — the URLs above are the canonical legitimate
source for the project's testing.

The synthesised-PE test path remains the no-network fallback so
CI is not strictly tied to the URL host being up. If
`samples.oxideav.org` is unreachable, the `test-fixtures`-gated
tests fail loudly (so the failure is visible) rather than
silently skipping.
