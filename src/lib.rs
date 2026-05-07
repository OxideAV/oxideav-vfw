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
//! MMX is deliberately **deferred** to round 3: Cinepak does not
//! use it, and our test corpus first targets Cinepak. Indeo 5
//! (`ir50_32.dll`) and most later codecs do, so MMX lands when
//! the test corpus expands.
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

/// Sibling registration entry point. Round 2 is a no-op — the
/// `oxideav-core` `RuntimeContext` does not yet have a "register
/// codec discovery for opaque guest DLLs" hook, and the codec-id
/// story for VfW-loaded modules will land in round 3 (one
/// `CodecImplementation` per loaded DLL with a generic
/// `vfw_<fcc>` codec_id).
///
/// Today this exists purely so `oxideav-meta` can wire the crate
/// into the umbrella registration cascade without a special case.
#[cfg(feature = "registry")]
pub fn register(_ctx: &mut oxideav_core::RuntimeContext) {
    // Round-2 placeholder — see module doc for round-3 plans.
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
