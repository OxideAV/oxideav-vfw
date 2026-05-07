# oxideav-vfw test fixtures

This directory is **empty in git** by design. The integration
test in `m1_load_dll_main.rs` has two paths:

* The unconditional `synth_dll_main_returns_through_sentinel`
  test, which builds a minimal PE32 DLL byte-by-byte from the
  public Microsoft PE/COFF specification and exercises the full
  round-1 stack (MMU + integer ISA + PE loader + kernel32
  stubs).

* The `test-fixtures`-gated `staged_codec_dll_runs_dll_main_cleanly`
  test, which loads a real legacy codec DLL.

The legacy codec DLLs themselves are **not committed to this
repository**. The crate's design contract
(`OxideAV/docs/winmf/winmf-emulator.md` §"Test corpus") explains
why: each DLL is the codec vendor's redistributable, the user
already owns it through the redistributable's licence terms, and
shipping it bundled here would muddy the licensing story.

## Where to legitimately source a Cinepak DLL

Cinepak's `iccvid.dll` (Radius / Provenance Systems / SuperMatch
Cinepak Toolkit, 1991) is in particular freely redistributable as
shipped in:

* Old Microsoft Windows redistributables (Windows Media Player
  6.4 was the last bundled-Cinepak version; the DLL is in the
  `Codecs` directory of the install).
* The K-Lite Mega Codec Pack (`klcp_mega_*.exe`,
  https://codecguide.com), which redistributes vendor codec
  packages with the original licences intact.
* The free `mscodec.zip` from various legacy multimedia
  archives.

After staging, place the file at:

```
crates/oxideav-vfw/tests/fixtures/iccvid.dll
```

And re-run the test with the `test-fixtures` feature enabled:

```
cargo test -p oxideav-vfw --features test-fixtures \
    -- staged_codec_dll
```

The test loads the DLL through the round-1 PE32 loader, calls
`DllMain(DLL_PROCESS_ATTACH)` through the interpreter, and
expects the call to return cleanly via the synthetic
return-address sentinel without an unhandled trap. If a trap
fires, the trap variant + EIP point at exactly which Win32 stub
or ISA opcode round 2 needs to add.

## Other supported fixtures

Round-1 also tries `tests/fixtures/ir50_32.dll` (Indeo 5) if
`iccvid.dll` is absent. Indeo 5 uses MMX heavily, so DllMain
will likely complete fine but the round-3 decode-frame test will
need MMX support landed first.

Future round-3 candidates (any one or more):

* `tsvqdll.dll` — Sorenson Video 3 (QuickTime variant).
* `mpg4ds32.ax` — MS-MPEG-4 v3 (DivX-:-) era).
* `voxmsdec.ax` — Voxware MetaSound.

All are legitimately redistributable under the same vendor terms
as the Cinepak DLL above; their staging path is the same.

## Why fixtures are not bundled

Two reasons:

1. **Licensing clarity.** The codec licences allow
   redistribution with the explicit attribution + EULA of the
   original codec pack. Re-bundling them here without those
   notices would be sloppy. The user already complied when
   they installed the codec pack on their own machine.
2. **Repository size.** Even Cinepak's DLL (~30 KiB) is fine
   alone, but a complete corpus across every round-3 codec is
   several megabytes of binary blobs in git history we don't
   need.

The synthesised-PE test path is sufficient for round-1 CI green;
the staged-fixture path is for the orchestrator's manual
post-merge verification.
