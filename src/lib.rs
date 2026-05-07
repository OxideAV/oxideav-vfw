//! Pure-Rust 32-bit x86 emulator + PE loader + Video for Windows
//! host. Lets oxideav delegate decoding (and eventually encoding)
//! to legitimately-licensed Windows codec DLLs on any platform.
//!
//! **Round 0 — design + scaffold.** The full design contract is
//! at `OxideAV/docs/winmf/winmf-emulator.md`. This crate currently
//! exposes only [`Error::NotImplemented`]. Round 1 brings the PE32
//! loader + i386 integer ISA + ~10 `kernel32` stubs and loads
//! Cinepak's `iccvid.dll` end-to-end.
//!
//! See `README.md` for the rebuild scope, the four-layer
//! architecture (emulator → PE loader → Win32 stubs → codec
//! wrapper), and the safety story (codec never executes on the
//! host CPU; the entire crate is aimed at
//! `#![forbid(unsafe_code)]`).

#![forbid(unsafe_code)]

/// Crate-local error type. Concrete variants land as the
/// Implementer rounds populate the emulator + loader + stub
/// surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Reserved placeholder. Replaced by real variants in round 1
    /// (`PeLoader::*`, `Emulator::Trap`, `Win32::Unsupported`,
    /// `Codec::DriverProcReturned`, etc.).
    NotImplemented,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotImplemented => f.write_str(
                "oxideav-vfw: round-0 scaffold; the emulator + PE loader + VfW host \
                 implementation lands in round 1. See crates/oxideav-vfw/README.md.",
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Crate-local Result alias.
pub type Result<T> = core::result::Result<T, Error>;
