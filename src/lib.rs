//! Pure-Rust 32-bit x86 emulator + PE loader + Video for Windows
//! host. Lets oxideav delegate decoding (and eventually encoding)
//! to legitimately-licensed Windows codec DLLs on any platform.
//!
//! **Round 1 — "Load + DllMain + clean exit".** The crate ships:
//!
//! * [`emulator::mmu`] — flat 4 GiB virtual address space, sparse
//!   4 KiB pages with R/W/X permission bits.
//! * [`emulator::regs`], [`emulator::decode`], [`emulator::isa_int`]
//!   — i386 register file, instruction decoder, executor for the
//!   integer base ISA.
//! * [`pe`] — PE32-only loader: DOS + PE header parse, section
//!   mapping into the MMU, base relocation, IAT resolution against
//!   the Win32 stub registry, export-by-name lookup.
//! * [`win32::kernel32`] — minimum stub set to satisfy a
//!   Cinepak-class DLL: `GetProcessHeap` / `HeapAlloc` /
//!   `HeapFree` / `HeapReAlloc` / `LocalAlloc` / `LocalFree` /
//!   `OutputDebugStringA` / `GetTickCount` /
//!   `InterlockedIncrement` / `InterlockedDecrement` /
//!   `LoadLibraryA` / `GetProcAddress`.
//!
//! **Round 2 — "Decode one Cinepak frame".** Adds:
//!
//! * [`Sandbox::call_export`] — generic stdcall guest-call helper.
//! * [`win32::vfw32`] — `BITMAPINFOHEADER` marshalling, `ICDECOMPRESS`
//!   layout, and the `IC*` host surface (`ICOpen`, `ICClose`,
//!   `ICGetInfo`, `ICDecompressBegin`, `ICDecompressQuery`,
//!   `ICDecompress`, `ICDecompressEnd`) that drives the codec
//!   DLL's `DriverProc` end-to-end.
//! * [`Sandbox::install_codec`] / [`Sandbox::ic_open`] etc — the
//!   ergonomic Rust-side wrappers the integration test uses.
//!
//! **Round 3 — "Real-codec smoke against IR32_32.DLL".** Adds:
//!
//! * `tests/common/mod.rs` — fixture-discovery helper:
//!   `OXIDEAV_VFW_FIXTURE_DIR` env var → Wine prefix → Windows
//!   system32 → on-disk cache → HTTPS fetch from
//!   `samples.oxideav.org`. CI=true bypasses the cache.
//! * Round-3 m1 test asserted the exact set of 49 Win32 imports
//!   (gdi32 / user32 / winmm + 24 extra kernel32) the
//!   round-1+2 stub registry did not satisfy — round 4's
//!   concrete dispatch budget. Round 4 closed every gap; the
//!   m1 test now asserts zero unresolved imports.
//! * `tests/m2_indeo3_driverproc.rs` retained the
//!   synthetic-codec walkthrough; a forward-compatible Indeo 3
//!   `DllMain → ICOpen → ICGetInfo → ICClose` walkthrough that
//!   activated once round 4 closed the import gaps.
//!
//! **Round 4 — "Close the 49 round-3 import gaps".** Adds the
//! 49 stubs round 3 surfaced:
//!
//! * [`win32::gdi32`] — 8 fail-soft stubs for `BitBlt` /
//!   `CreateCompatibleDC` / `DeleteDC` / `GetDeviceCaps` /
//!   `GetNearestColor` / `GetObjectA` /
//!   `GetSystemPaletteEntries` / `SelectObject`.
//! * [`win32::kernel32`] — 24 round-4 stubs covering the CRT
//!   init surface (`ExitProcess`, `GetACP` / `GetOEMCP` /
//!   `GetCPInfo`, `GetCommandLineA` / `GetEnvironmentStrings` /
//!   `GetFileType`, `GetLastError` / `SetLastError`,
//!   `GetModuleFileNameA` / `GetModuleHandleA`,
//!   `GetStartupInfoA` / `GetStdHandle` / `GetSystemInfo` /
//!   `GetVersion`, `GlobalAlloc` / `GlobalFree` / `GlobalLock`
//!   / `GlobalUnlock`, `MultiByteToWideChar` /
//!   `WideCharToMultiByte`, `RtlUnwind`, `VirtualAlloc` /
//!   `VirtualFree`, `WriteFile`).
//! * [`win32::user32`] — 16 fail-soft stubs covering the
//!   dialog / paint surface; `MessageBoxA` logs to stderr +
//!   `host.message_box_log`; `wsprintfA` is a real cdecl
//!   variadic implementation.
//! * [`win32::winmm`] — `DefDriverProc` (returns 0 / DRVCNF_OK).
//! * [`emulator::mmu::Mmu::unmap`] +
//!   [`emulator::mmu::Mmu::find_free_range`] for the
//!   `VirtualAlloc` / `VirtualFree` family.
//!
//! With round 4 in place, `IR32_32.DLL` loads cleanly and
//! `DllMain` runs until it hits the first ISA opcode our integer
//! interpreter does not yet decode: `ADD AL, imm8` (opcode
//! `0x04`) at `eip = 0x1000_612A`. That was round-4's hand-off
//! to round 5.
//!
//! **Round 5 — "DllMain + ICOpen + ICGetInfo + ICClose against
//! Intel IR32_32.DLL".** Adds:
//!
//! * The 8-bit primary ALU opcodes (`0x00..=0x05` ADD,
//!   `0x08..=0x0D` OR, `0x10..=0x15` ADC, …, `0x38..=0x3D` CMP)
//!   plus `r/m8 imm8` group-1 (`0x80`), `r/m8` group-3 (`0xF6`),
//!   `r/m8` group-4 (`0xFE`).
//! * Group-2 `r/m8` shifts (`0xC0/0xD0/0xD2`) plus the
//!   `r/m32` 1/cl variants (`0xD1/0xD3`).
//! * `IMUL r32, r/m32, imm32`/`imm8` (`0x69/0x6B`),
//!   `XCHG r/m, r` (`0x86/0x87`), `SAHF/LAHF` (`0x9E/0x9F`),
//!   `CMC` (`0xF5`), `PUSHAD/POPAD` (`0x60/0x61`), `ENTER`
//!   (`0xC8`), the full `MOVS/CMPS/STOS/LODS/SCAS` family with
//!   REP / REPE / REPNE prefixes.
//! * `0F 40..4F CMOVcc`, `0F A3 BT`, `0F AB BTS`, `0F A4..A5
//!   SHLD`, `0F AC..AD SHRD`, `0F BA` group-8 (BT/BTS/BTR/BTC
//!   imm8), `0F B1 CMPXCHG`, `0F C1 XADD`, `0F C8..CF BSWAP`.
//! * Per-instruction segment-override prefix routing through
//!   [`emulator::Cpu::set_fs_base`] / `set_gs_base`. The
//!   runtime maps a 4 KiB TEB at `0x7FFD_E000`, primes
//!   `FS:[0]` (SEH chain end-of-list = `-1`) and `FS:[0x18]`
//!   (TEB self-pointer), and points FS at it.
//! * Corrected `vfw32::ICM_*` numeric values
//!   (`ICM_GETINFO = 0x5002`, `ICM_DECOMPRESS = 0x400D`, etc).
//! * [`win32::vfw32::ic_open`] now stages a real 36-byte
//!   `ICOPEN` so the codec's `DRV_OPEN` allocates per-instance
//!   state (round 4 passed NULL).
//! * [`win32::vfw32::ic_get_info`] falls back to an
//!   fcc-derived `szName` when the codec leaves it NUL
//!   (real `vfw32!ICGetInfo` fills it from the registry).
//! * Bug-fix: round-4's `0xC6` (MOV r/m8, imm8) handler
//!   fetched the immediate BEFORE resolving the displacement.
//!
//! MMX is deliberately **deferred** to round 6+: Indeo 3 is
//! pre-MMX, so it stays unblocked. Indeo 5 (`ir50_32.dll`) and
//! most later codecs use MMX, so MMX support lands when the test
//! corpus expands to one of those.
//!
//! Modern codecs (H.264 / HEVC / AV1 / Opus / AAC / …) are decoded
//! natively elsewhere in the workspace; this crate exists for
//! **rare/legacy** codecs the project would otherwise permanently
//! shelve. Codec DLLs never execute on the host CPU; they run
//! through the bounded-MMU interpreter.
//!
//! See `OxideAV/docs/winmf/winmf-emulator.md` (659 lines, 13
//! sections) for the full design contract.

#![forbid(unsafe_code)]

pub mod emulator;
pub mod pe;
pub mod runtime;
pub mod win32;

pub use runtime::{Sandbox, DLL_PROCESS_ATTACH};
pub use win32::vfw32::Bih;

/// Sibling registration entry point. Currently a no-op — the
/// `oxideav-core` `RuntimeContext` does not yet have a "register
/// codec discovery for opaque guest DLLs" hook, and the codec-id
/// story for VfW-loaded modules waits for the loader to clear
/// the round-4 import-stub gap before any `CodecImplementation`
/// can be advertised (one `vfw_<fcc>` entry per loaded DLL).
///
/// Today this exists purely so `oxideav-meta` can wire the crate
/// into the umbrella registration cascade without a special case.
#[cfg(feature = "registry")]
pub fn register(_ctx: &mut oxideav_core::RuntimeContext) {
    // Placeholder — see module-level doc for the milestone plan.
}

#[cfg(feature = "registry")]
oxideav_core::register!("oxideav-vfw", register);

/// Crate-local error type. Each layer (MMU / decoder / executor /
/// PE loader / Win32 stub) has its own variant; sublayers nest
/// their detail enums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Reserved placeholder. Removed in a later round once every
    /// caller has migrated to a more specific variant.
    NotImplemented,
    /// Guest tripped a CPU trap (memory fault, illegal opcode,
    /// privileged instruction, division by zero, …).
    Trap(emulator::Trap),
    /// PE loader rejected the input bytes — bad signature,
    /// unsupported PE32+ / .NET / packed binary, malformed
    /// directory entries, missing import, etc.
    PeLoader(pe::PeError),
    /// A Win32 stub was called with an argument the round-1 stub
    /// surface cannot satisfy (unknown DLL, unknown ordinal,
    /// invalid heap handle, etc.).
    Win32(win32::Win32Error),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotImplemented => f.write_str(
                "oxideav-vfw: round-1 does not yet implement this code path; \
                 see crates/oxideav-vfw/README.md for the milestone schedule.",
            ),
            Error::Trap(t) => write!(f, "oxideav-vfw emulator trap: {t}"),
            Error::PeLoader(e) => write!(f, "oxideav-vfw PE loader: {e}"),
            Error::Win32(e) => write!(f, "oxideav-vfw Win32 stub: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<emulator::Trap> for Error {
    fn from(t: emulator::Trap) -> Self {
        Error::Trap(t)
    }
}

impl From<pe::PeError> for Error {
    fn from(e: pe::PeError) -> Self {
        Error::PeLoader(e)
    }
}

impl From<win32::Win32Error> for Error {
    fn from(e: win32::Win32Error) -> Self {
        Error::Win32(e)
    }
}

/// Crate-local Result alias.
pub type Result<T> = core::result::Result<T, Error>;
