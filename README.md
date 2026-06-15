# oxideav-vfw

Thin bridge from
[`ud-emulator`](https://crates.io/crates/ud-emulator)'s 32-bit
x86 / PE32 / Video for Windows sandbox into the
[oxideav](https://github.com/OxideAV/oxideav-workspace) codec
registry, plus the FS-walking **discovery layer** that probes
`~/.local/share/oxideav/codecs/` for legitimately-licensed Windows
codec DLLs.

The codec never executes on the host CPU; it runs through
ud-emulator's software interpreter sandbox.

## What this crate does

1. **Discovers** `*.dll` / `*.ax` files on disk.
2. **Probes** each candidate through a fresh
   [`ud_emulator::Sandbox`](https://docs.rs/ud-emulator/0.1/ud_emulator/struct.Sandbox.html)
   (VfW first, DirectShow fallback) to classify the entry-point
   surface.
3. **Registers** one [`oxideav_core::CodecInfo`] per recognised
   FourCC into the runtime, wired to a `Decoder` factory that
   constructs lazily and drives the codec through the
   `ICDecompressQuery → ICDecompressBegin → ICDecompress →
   ICDecompressEnd` lifecycle on first `send_packet`. VfW
   (`Kind::Vfw`) codecs additionally register an `Encoder` factory
   that mirrors the decode path over the `ICCompressQuery →
   ICCompressGetFormat → ICCompressGetSize → ICCompressBegin →
   ICCompress → ICCompressEnd` lifecycle on first `send_frame`.
   The encoder threads the previous raw input frame through
   `ICCompress`'s `lpPrev` reference slot on non-keyframe encodes
   and honours three optional `CodecParameters.options` knobs:
   `"quality"` (u32 `0..10000`), `"keyint"` (u32 frames; force
   every Nth frame to a keyframe), and `"data_rate"` (u32 bytes;
   per-frame byte ceiling threaded into `ICCompress`'s
   `dwFrameSizeLimit` slot, useful for MTU-bounded transports).
   DirectShow (`Kind::DirectShow`) filters are decode-only through
   this bridge.

Everything below that — the i386 interpreter, the PE32 loader,
the kernel32 / user32 / gdi32 / vfw32 / msvfw32 / ole32 / winmm
shims, the DirectShow `IBaseFilter` / `IPin` / `IMemAllocator`
host scaffolding, the JSONL trace surface — lives upstream in
[`ud-emulator`](https://crates.io/crates/ud-emulator). The
discovery layer in this crate is the only oxideav-specific
piece.

## Discovery path

| env / scope                           | default                                        |
| ------------------------------------- | ---------------------------------------------- |
| `OXIDEAV_VFW_CODEC_PATH=<list>`       | overrides default (`:`-sep on UNIX, `;` Win)   |
| Linux / macOS (env unset)             | `$XDG_DATA_HOME/oxideav/codecs/` or            |
|                                       | `$HOME/.local/share/oxideav/codecs/`           |
| Windows (env unset)                   | `%LOCALAPPDATA%\oxideav\codecs\`               |

Discovery walks each directory **non-recursively** for `*.dll` /
`*.ax`. Files that aren't valid PE32, or that lack a `DriverProc`
or recognisable `DllGetClassObject` CLSID, are recorded as
`Kind::Unsupported` (so re-probe is skipped) and otherwise
silently ignored.

When `OXIDEAV_VFW_CODEC_PATH` is honoured, each path-list
component has leading and trailing ASCII whitespace stripped
before use, and components that are empty (or whitespace-only)
after the strip are filtered out. This makes the env var
forgiving of `.env` files, systemd `Environment=` lines, and
Docker / Kubernetes container manifests where shell expansion
doesn't run and YAML quoting frequently leaves stray whitespace
around each value — `OXIDEAV_VFW_CODEC_PATH="  /p1 : /p2\n"`
now resolves to `["/p1", "/p2"]` instead of two unreadable
paths. Interior whitespace inside a path (`~/Library/Application
Support/...`, `C:\Program Files\...`) is preserved untouched —
the strip is `trim_matches`, not a global `replace`.

Results are cached at:

- Linux / macOS: `$XDG_CACHE_HOME/oxideav/vfw-discovery.json` or
  `$HOME/.cache/oxideav/vfw-discovery.json`
- Windows: `%LOCALAPPDATA%\oxideav\Cache\vfw-discovery.json`

keyed by `(absolute_path, mtime_unix, size_bytes)`. Cache writes
are atomic (tempfile + rename); a corrupted (malformed-JSON or
zero-byte) cache is treated as empty rather than poisoning
`register()`, and is healed (re-probe → atomic overwrite) on the
next call. The on-disk cache is a versioned envelope
(`{ "version": 1, "entries": [...] }`); readers refuse a file whose
version doesn't match and fall into the corruption-recovery path.
Steady-state `register()` against a stable codec directory performs
zero filesystem writes (an interior dirty flag skips the no-op save).

### Encoder-knobs query API

The encoder honours three optional `CodecParameters.options` bridge
knobs — `"quality"` (u32 `0..10000`, clamped at `ENCODER_QUALITY_MAX
= 10_000`), `"keyint"` (u32 frames; force every Nth frame to a
keyframe), and `"data_rate"` (u32 bytes; per-frame byte ceiling). The
spellings live on named public constants (`ENCODER_KNOB_QUALITY` /
`ENCODER_KNOB_KEYINT` / `ENCODER_KNOB_DATA_RATE`, collected in
`ENCODER_KNOB_KEYS`), so the caller-side and resolver-side lookups
share one source of truth.

`discovery::resolve_encoder_knobs(&CodecParameters) -> EncoderKnobs`
is the typed pre-construction companion to `make_encoder` — it returns
the resolved values the encoder will see (after best-effort `u32`
parsing + the `quality` clamp) without constructing an encoder.
`EncoderKnobs` is `Copy + Default + PartialEq`; the default is the
"no opt-in" sentinel (all fields `0`). Parsing is best-effort: a
missing or unparseable value falls back to the per-knob default rather
than failing. `discovery::unrecognized_encoder_knobs(&CodecParameters)
-> Vec<&str>` reports, in insertion order, the option keys the encoder
will silently ignore (exact, case-sensitive matching), so a CLI /
pipeline pre-validator can warn about a typo'd knob before encode time.

### Single-shot DLL probe helper

`discovery::probe_dll(&Path) -> Option<ProbeResult>` is the single-shot
companion to `discover_and_register(ctx)`. A consumer that already holds
an absolute DLL path can classify the entry-point surface (VfW
`DriverProc` + FourCC sweep; DirectShow `DllGetClassObject` + CLSID
match; or `Unsupported`) without walking the configured discovery
directory, mutating a `RuntimeContext`, or touching the on-disk cache.
It returns `None` only when the file cannot be read; a file that reads
cleanly but doesn't load as PE32 / lacks both recognised entry-point
surfaces lands on `Some(ProbeResult { kind: Kind::Unsupported, .. })`.
The byte-accepting form `probe_bytes` and the `ProbeResult` type are
re-exported from `crate::discovery`.

## Codec registration priority

All discovered codecs land at **priority 200** — VfW is a
last-resort path and resolves only when no higher-priority crate
(pure-Rust = 100, hardware = 10) already claims the FourCC.

## For forensic debugging

This crate is **production-only** — it has no instruction trace
output, no opcode-coverage instrumentation, no per-call event
sinks. Those live one layer down in `ud-emulator`. For
reverse-engineering work, drive the
[`ud`](https://crates.io/crates/ud) CLI directly:

```
ud vfw probe ./codec.dll
ud vfw decode ./codec.dll ./stream.avi --out ./decoded/
ud vfw encode ./codec.dll ./raw.yuv  --out ./encoded.avi
```

Those subcommands give you the full ud-emulator surface
(watchpoints, instruction trace, per-syscall logs, COM-method
breakpoints, …) without dragging the dependency tail into the
production playback path.

## Back-compat re-exports

Downstream code that historically wrote `oxideav_vfw::Sandbox` /
`oxideav_vfw::Guid` / `oxideav_vfw::Bih` / etc. continues to
compile via re-exports:

```rust
pub use ud_emulator::{Sandbox, DLL_PROCESS_ATTACH};
pub use ud_emulator::com::{Guid, IID_IBASEFILTER, /* … */};
pub use ud_emulator::win32::vfw32::Bih;
#[cfg(feature = "trace")]
pub use ud_emulator::{TraceState, WatchMode, Watchpoint};
```

**New code should depend on `ud-emulator` directly** and use this
crate only for `discover_and_register` + the `Codec` trait
adapter.

## Features

| feature           | default | what it does                                        |
| ----------------- | :-----: | --------------------------------------------------- |
| `registry`        |   on    | enables `oxideav-core` dep + `register()` cascade   |
| `auto-discovery`  |   on    | enables the FS-walking + cache layer (+ `log` /     |
|                   |         | `serde` / `serde_json`)                             |
| `trace`           |   off   | passthrough to `ud-emulator/trace`                  |
| `trace-exec`      |   off   | passthrough to `ud-emulator/trace-exec`             |

Consumers building with `default-features = false` get the bare
`ud-emulator` re-exports and the `Decoder` factory — no FS scan,
no cache, no trace surface.

## License

MIT (same as upstream ud-emulator and oxideav).
