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

Results are cached at:

- Linux / macOS: `$XDG_CACHE_HOME/oxideav/vfw-discovery.json` or
  `$HOME/.cache/oxideav/vfw-discovery.json`
- Windows: `%LOCALAPPDATA%\oxideav\Cache\vfw-discovery.json`

keyed by `(absolute_path, mtime_unix, size_bytes)`. Cache writes
are atomic (tempfile + rename); a corrupted cache is treated as
empty rather than poisoning `register()`. Round 189 added an
end-to-end integration test
(`tests/round189_corrupted_cache_recovery.rs`) covering both the
malformed-JSON and zero-byte cache cases — the existing unit
test only exercised `Cache::load` in isolation; the new test wires
the full `discover() → re-probe → atomic-overwrite → next-call hits
the healed cache` round-trip with the cache file redirected via
`XDG_CACHE_HOME` / `LOCALAPPDATA` so the dev box's real cache is
never touched.

### Schema versioning (round 197)

The on-disk cache is now a **versioned envelope**:

```json
{
  "version": 1,
  "entries": [ /* CacheEntry, ... */ ]
}
```

The `version` field is stamped at `discovery::CURRENT_SCHEMA_VERSION`
on every save. Readers refuse any file whose version doesn't match
their own — both downgrades (a `v2` file read by a `v1` reader) and
forward-incompatible upgrades fall into the round-189
corruption-recovery path: discard, re-probe, heal on next save.
Pre-round-197 caches (top-level JSON array, no version field) are
still loadable on first call, then promoted to the envelope shape
on the same call's atomic-write tail — no user intervention
required. Three integration tests in
`tests/round197_cache_schema_versioning.rs` cover legacy-upgrade,
future-version refusal, and the round-trip stability invariant; six
new unit tests in `discovery::cache::tests` lock in the envelope
shape, the version stamp, and the negative paths
(unknown/older/malformed envelope = `None`).

Round 197 also closed a long-latent same-binary test race in the
round-189 corrupted-cache test pair: parallel test execution +
process-global `XDG_CACHE_HOME` made the two tests' env-var writes
interleave under `--test-threads >= 2`. Both binaries now serialise
their env-var mutations through a process-global `Mutex`.

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
