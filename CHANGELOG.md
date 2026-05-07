# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Round 0: scaffold for the WinMF emulator system. The full design
  contract is at `OxideAV/docs/winmf/winmf-emulator.md` (659 lines,
  13 sections). This crate currently exposes only an
  `Error::NotImplemented` placeholder; round 1 brings the PE32
  loader + i386 integer ISA + ~10 `kernel32` stubs and demonstrates
  loading Cinepak's `iccvid.dll` end-to-end.

### Notes on scope

- The crate's purpose is **rare-codec compatibility**, not
  day-to-day playback. Modern codecs (H.264, HEVC, AV1, VP9, Opus,
  AAC, …) all have pure-Rust decoders elsewhere in the workspace.
  This crate exists for codecs the project would otherwise
  permanently shelve: Indeo 4/5, Sorenson Video 1/3, MS-MPEG-4 v3,
  Cook, On2 VP3-pre-Theora, MS speech codecs, etc.
- 32-bit x86 only. Every target codec ships a 32-bit version; many
  never had a 64-bit port.
- Safety > performance. Pure interpreter, no JIT, the entire crate
  aimed at `#![forbid(unsafe_code)]`. Codec runs through a
  bounded-MMU sandbox; never executes on the host CPU.
