//! Wire each [`super::DiscoveryEntry`] into the framework codec
//! registry as a [`oxideav_core::CodecInfo`].
//!
//! Because [`oxideav_core::DecoderFactory`] is a bare `fn`
//! pointer, the per-codec context (DLL path / FourCC / kind /
//! CLSID) cannot be captured. Instead we maintain a process-wide
//! lookup keyed by `codec_id`, and a single shared factory function
//! looks up the [`DiscoveryRecord`] there at construction time.
//!
//! Codec id format: `vfw_<fourcc-lowercase>_<dll-basename-stem>`,
//! e.g. `"vfw_mp43_mpg4ds32"`. Avoids collisions when multiple
//! DLLs claim the same FourCC.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecTag, Decoder, Error, Frame,
    Packet, Result, RuntimeContext,
};

use super::probe::{fourcc_to_bytes, Kind};

/// Backing-store record for one discovered codec. Stashed in the
/// per-process [`record_table`] so the bare `fn`
/// [`oxideav_core::DecoderFactory`] can reach it at
/// `make_decoder` time.
#[derive(Debug, Clone)]
pub struct DiscoveryRecord {
    pub dll_path: PathBuf,
    pub fourcc: String,
    pub kind: Kind,
    pub clsid: Option<String>,
}

/// Process-wide lookup of `codec_id` → [`DiscoveryRecord`].
///
/// Populated by [`register_factory_for_id`] before `register()`
/// returns. Read by the `make_decoder` factory below.
///
/// `OnceLock<Mutex<…>>` keeps initialisation lazy — most consumers
/// won't ever use the auto-discovery path and we don't want to
/// pay for the table.
fn record_table() -> &'static Mutex<HashMap<String, DiscoveryRecord>> {
    static TABLE: OnceLock<Mutex<HashMap<String, DiscoveryRecord>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Insert `record` under `codec_id`. Overwrites any prior entry
/// — `register()` may legitimately be called multiple times in a
/// single process (CLI's `--list` then a real run, tests, …).
pub fn register_factory_for_id(codec_id: &str, record: DiscoveryRecord) {
    if let Ok(mut t) = record_table().lock() {
        t.insert(codec_id.to_string(), record);
    }
}

/// Look up the discovery record stashed for `codec_id`. Returns
/// `None` for any codec id that wasn't registered through
/// [`register_factory_for_id`].
pub fn lookup_record(codec_id: &str) -> Option<DiscoveryRecord> {
    let t = record_table().lock().ok()?;
    t.get(codec_id).cloned()
}

/// Build the canonical codec id string for a given DLL + FourCC.
///
/// Format: `vfw_<lowercase-fourcc>_<dll-basename-stem-lowercase>`.
/// Sanitisation: any byte outside `[a-z0-9]` is replaced by `_`
/// so the id stays JSON / CLI / shell-safe.
pub fn codec_id_for(dll_path: &Path, fourcc: &str) -> String {
    let stem = dll_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let mut id = String::with_capacity(8 + 4 + 1 + stem.len());
    id.push_str("vfw_");
    push_sanitised(&mut id, fourcc);
    id.push('_');
    push_sanitised(&mut id, stem);
    id
}

fn push_sanitised(out: &mut String, s: &str) {
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
}

/// Register one [`CodecInfo`] for a discovered DLL+FourCC pair.
///
/// Priority is fixed at 200 — VfW is a last-resort path that
/// resolves only when a native crate doesn't already claim the
/// tag. The shared `make_decoder` factory below pulls the
/// matching [`DiscoveryRecord`] out of [`record_table`] at
/// construction time.
pub fn register_codec_info(ctx: &mut RuntimeContext, codec_id: &str, fourcc: &str) {
    let id = CodecId::new(codec_id.to_string());
    let caps = CodecCapabilities::video("vfw_sandboxed")
        .with_decode()
        .with_lossy(true)
        .with_priority(200);

    let mut info = CodecInfo::new(id).capabilities(caps).decoder(make_decoder);
    if let Some(bytes) = fourcc_to_bytes(fourcc) {
        info = info.tag(CodecTag::fourcc(&bytes));
    }
    ctx.codecs.register(info);
}

/// Shared `make_decoder` factory — looks up the per-codec record
/// stashed by [`register_factory_for_id`] at register-time.
///
/// VfW codecs return a real [`SandboxedVfwDecoder`]. DirectShow
/// codecs return `Err(Unsupported)` for round 28 — the full
/// `IPin::Receive → IMemAllocator → IMediaSample` host wiring
/// arrives in round 29.
pub fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    let id_str = params.codec_id.as_str();
    let record = lookup_record(id_str).ok_or_else(|| {
        Error::other(format!(
            "vfw discovery: codec id {id_str:?} not registered (call \
             oxideav_vfw::register first, or ensure OXIDEAV_VFW_CODEC_PATH \
             points at a codec directory)"
        ))
    })?;

    match record.kind {
        Kind::Vfw => Ok(Box::new(SandboxedVfwDecoder::new(record, params.clone())?)),
        Kind::DirectShow => Err(Error::unsupported(format!(
            "vfw discovery: decode through DirectShow IPin::Receive not yet \
             wired — needs IMemAllocator / IMediaSample host stubs (round 29). \
             CLSID = {:?}",
            record.clsid
        ))),
        Kind::Unsupported => Err(Error::unsupported(
            "vfw discovery: this codec was probed but found unsupported",
        )),
    }
}

// ────────────────────────────────────────────────────────────────
// SandboxedVfwDecoder — thin Decoder impl that holds the Sandbox
// + the codec FourCC and dispatches `send_packet` →
// `ic_decompress`. Round 28 keeps the implementation minimal: we
// hold the sandbox open across packets but defer real frame
// reception (with an actual ICDecompressBegin/End handshake) to
// the existing manual API. The auto-discovery path's job is to
// **register** the codec; full per-frame decode through the
// generic `Decoder` trait can ride on the sandboxed manual API
// once consumers wire the pixel-format negotiation.
// ────────────────────────────────────────────────────────────────

struct SandboxedVfwDecoder {
    codec_id: CodecId,
    record: DiscoveryRecord,
    /// Sandbox is constructed lazily on the first `send_packet`
    /// — `make_decoder` runs synchronously on every codec lookup
    /// and consumers may discard the result without ever calling
    /// `send_packet`, so we don't pay for the DLL load until it
    /// matters.
    sandbox: Option<crate::Sandbox>,
    /// Loaded image — kept alive alongside the sandbox.
    image: Option<crate::pe::Image>,
    /// Currently-open ICOpen handle. `0` means "not opened yet".
    hic: u32,
    pending: Option<Packet>,
    eof: bool,
}

impl SandboxedVfwDecoder {
    fn new(record: DiscoveryRecord, params: CodecParameters) -> Result<Self> {
        Ok(SandboxedVfwDecoder {
            codec_id: params.codec_id.clone(),
            record,
            sandbox: None,
            image: None,
            hic: 0,
            pending: None,
            eof: false,
        })
    }

    fn ensure_open(&mut self) -> Result<()> {
        if self.sandbox.is_some() {
            return Ok(());
        }
        let bytes = std::fs::read(&self.record.dll_path)
            .map_err(|e| Error::other(format!("vfw discovery: read DLL failed: {e}")))?;
        let mut sb = crate::Sandbox::new();
        let img = sb
            .load("codec.dll", &bytes)
            .map_err(|e| Error::other(format!("vfw discovery: Sandbox::load failed: {e}")))?;
        sb.install_codec(&img)
            .map_err(|e| Error::other(format!("vfw discovery: install_codec failed: {e}")))?;
        // Drive DllMain so any per-DLL CRT init runs.
        let _ = sb.call_dll_main(&img, crate::DLL_PROCESS_ATTACH);
        let fcc = fourcc_to_bytes(&self.record.fourcc).ok_or_else(|| {
            Error::other(format!(
                "vfw discovery: bad fourcc {:?} in record",
                self.record.fourcc
            ))
        })?;
        let fcc_handler = u32::from_le_bytes(fcc);
        let fcc_type = u32::from_le_bytes(*b"VIDC");
        let hic = sb
            .ic_open(fcc_type, fcc_handler, 0)
            .map_err(|e| Error::other(format!("vfw discovery: ic_open failed: {e}")))?;
        if hic == 0 {
            return Err(Error::other(
                "vfw discovery: ICOpen returned NULL (codec rejected handler FourCC)",
            ));
        }
        self.sandbox = Some(sb);
        self.image = Some(img);
        self.hic = hic;
        Ok(())
    }
}

impl Decoder for SandboxedVfwDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        if self.pending.is_some() {
            return Err(Error::other(
                "vfw discovery: receive_frame must be called before sending another packet",
            ));
        }
        self.ensure_open()?;
        self.pending = Some(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        if self.pending.take().is_none() {
            return if self.eof {
                Err(Error::Eof)
            } else {
                Err(Error::NeedMore)
            };
        }
        // Round 28: the auto-discovery path REGISTERS the codec —
        // full per-frame decode through the generic `Decoder`
        // trait still leans on the manual `Sandbox::ic_decompress`
        // API (consumers must drive ICDecompressBegin / pixel-format
        // negotiation manually). The trait surface returns a clear
        // "use the manual API" error rather than silently dropping
        // the packet.
        Err(Error::unsupported(
            "vfw discovery: per-frame Frame production through the generic \
             Decoder trait is not yet wired (round 28 registers the codec; \
             round 29 wires ICDecompress* + pixel-format negotiation). \
             Use the manual `Sandbox::ic_decompress` API in the meantime.",
        ))
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }
}

impl Drop for SandboxedVfwDecoder {
    fn drop(&mut self) {
        if let (Some(sb), hic) = (self.sandbox.as_mut(), self.hic) {
            if hic != 0 {
                let _ = sb.ic_close(hic);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn codec_id_format_matches_spec() {
        assert_eq!(
            codec_id_for(&PathBuf::from("/tmp/MPG4DS32.AX"), "MP43"),
            "vfw_mp43_mpg4ds32"
        );
        assert_eq!(
            codec_id_for(&PathBuf::from("foo/IR32_32.DLL"), "IV31"),
            "vfw_iv31_ir32_32"
        );
    }

    #[test]
    fn codec_id_sanitises_dashes_and_dots() {
        // dashes / dots / spaces collapse to underscores.
        assert_eq!(
            codec_id_for(&PathBuf::from("/p/some-name.with.dot.dll"), "ABCD"),
            "vfw_abcd_some_name_with_dot"
        );
    }

    #[test]
    fn record_table_round_trip() {
        let id = "vfw_test_record_round_trip_unique";
        register_factory_for_id(
            id,
            DiscoveryRecord {
                dll_path: PathBuf::from("/x/y.dll"),
                fourcc: "MP43".to_string(),
                kind: Kind::Vfw,
                clsid: None,
            },
        );
        let r = lookup_record(id).expect("present");
        assert_eq!(r.fourcc, "MP43");
        assert_eq!(r.kind, Kind::Vfw);
    }

    #[test]
    fn make_decoder_unknown_id_errors_cleanly() {
        let params = CodecParameters::video(CodecId::new(
            "vfw_unknown_codec_id_should_not_match_anything",
        ));
        let r = make_decoder(&params);
        assert!(r.is_err());
    }

    #[test]
    fn make_decoder_dshow_returns_unsupported() {
        let id = "vfw_dshow_make_decoder_test";
        register_factory_for_id(
            id,
            DiscoveryRecord {
                dll_path: PathBuf::from("/x/foo.ax"),
                fourcc: "WMV3".to_string(),
                kind: Kind::DirectShow,
                clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".into()),
            },
        );
        let params = CodecParameters::video(CodecId::new(id));
        match make_decoder(&params) {
            Err(Error::Unsupported(msg)) => assert!(msg.contains("DirectShow")),
            Err(other) => panic!("expected Unsupported, got {other:?}"),
            Ok(_) => panic!("expected Err(Unsupported), got Ok(_)"),
        }
    }
}
