//! Thin bridge from [`ud-emulator`](https://crates.io/crates/ud-emulator)'s
//! 32-bit x86 / PE32 / Video for Windows sandbox into the oxideav
//! codec registry.
//!
//! ## What this crate is
//!
//! * A **codec discovery layer** ([`discovery`]) that walks
//!   `~/.local/share/oxideav/codecs/` (or `OXIDEAV_VFW_CODEC_PATH`),
//!   probes each `*.dll` / `*.ax` through a fresh
//!   [`Sandbox`], and registers one
//!   [`oxideav_core::CodecInfo`] per recognised FourCC.
//! * A [`oxideav_core::Codec`] / [`oxideav_core::Decoder`] adapter
//!   that owns a [`Sandbox`] across packets and dispatches
//!   `send_packet` → `ic_decompress` → `Frame::Video`.
//! * The [`register`] entry point that the framework consumes
//!   via [`oxideav_core::register!`].
//!
//! ## What this crate is NOT
//!
//! The emulator, PE32 loader, Win32 host shims, COM scaffolding,
//! and forensic test harnesses **all live upstream in `ud-emulator`**.
//! For reverse-engineering work (per-DLL trace replay, opcode
//! coverage, allocator forensics, …) use the
//! [`ud`](https://crates.io/crates/ud) CLI's `ud vfw {probe,decode,encode}`
//! subcommands directly, not this crate.
//!
//! ## Re-exports for back-compat
//!
//! Downstream consumers that historically wrote
//! `oxideav_vfw::Sandbox` / `oxideav_vfw::Guid` / etc continue to
//! work via the re-exports below. New code should depend on
//! `ud-emulator` directly.

#![forbid(unsafe_code)]

#[cfg(feature = "auto-discovery")]
pub mod discovery;

// ── Re-exports from ud-emulator ──────────────────────────────────
//
// vfw historically re-exported the same surface from its own
// `runtime` / `com` / `win32` modules. After the v0.2 bridge
// rewrite those modules live in ud-emulator; we keep the public
// names stable so consumers don't have to chase the move.

pub use ud_emulator::com::{
    Guid, GuidParseError, CLSID_MEMORY_ALLOCATOR, IID_IBASEFILTER, IID_ICLASSFACTORY,
    IID_IENUMPINS, IID_IFILTERGRAPH, IID_IMEDIAFILTER, IID_IMEDIASAMPLE, IID_IMEMALLOCATOR,
    IID_IMEMINPUTPIN, IID_IPERSIST, IID_IPIN, IID_IUNKNOWN, MSADDS_AUDIO_DECODER_CLSID,
    MSADDS_AUDIO_PROPERTY_PAGE_CLSID,
};
pub use ud_emulator::win32::vfw32::Bih;
pub use ud_emulator::{Sandbox, DLL_PROCESS_ATTACH};
#[cfg(feature = "trace")]
pub use ud_emulator::{TraceState, WatchMode, Watchpoint};

/// Sibling registration entry point.
///
/// **With `auto-discovery` enabled (default):** walks the
/// configured discovery path (`OXIDEAV_VFW_CODEC_PATH` or the
/// platform-default codec dir), probes every `*.dll` / `*.ax`
/// for VfW or DirectShow entry points, and registers one
/// [`oxideav_core::CodecInfo`] per recognised FourCC into
/// `ctx.codecs`. Every codec lands at priority 200 — VfW
/// resolves only when no higher-priority crate already claims
/// the tag. See [`crate::discovery`] for the full contract.
///
/// **Without `auto-discovery`:** no-op. Consumers building with
/// `default-features = false` get the bare manual `Sandbox` API
/// without the FS scan / cache / log-and-serde dependency tail.
///
/// Hard contract: never panics. A missing discovery directory
/// (network-isolated CI, container without the user-data dir,
/// fresh dev box) cleanly registers zero codecs.
#[cfg(feature = "registry")]
pub fn register(_ctx: &mut oxideav_core::RuntimeContext) {
    #[cfg(feature = "auto-discovery")]
    {
        let _registered = discovery::discover_and_register(_ctx);
    }
}

#[cfg(feature = "registry")]
oxideav_core::register!("oxideav-vfw", register);
