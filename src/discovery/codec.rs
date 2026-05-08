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
    Packet, PixelFormat, Result, RuntimeContext, VideoFrame, VideoPlane,
};

use crate::win32::vfw32::{Bih, ICDECOMPRESS_NOTKEYFRAME};

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
// SandboxedVfwDecoder — Decoder impl that holds the Sandbox + the
// codec instance handle (HIC) across packets and dispatches
// `send_packet` → `ic_decompress` → `Frame::Video`.
//
// Round 29 wires the full ICDecompressQuery → ICDecompressBegin →
// ICDecompress → ICDecompressEnd handshake:
//
// * `ensure_open` (lazy on first `send_packet`) loads the DLL,
//   drives DllMain, opens the codec handle, runs the
//   query+begin handshake against a synthesised input
//   `BITMAPINFOHEADER` (FOURCC = record.fourcc, 24bpp coded
//   from the codec parameters) and a fixed BI_RGB 24bpp output
//   `BITMAPINFOHEADER`.
// * `receive_frame` consumes the pending packet, calls
//   `ic_decompress` with `ICDECOMPRESS_NOTKEYFRAME` set unless
//   `packet.flags.keyframe`, then materialises the codec's
//   bottom-up BGR24 output as a top-down `Frame::Video` with
//   `PixelFormat::Bgr24`.
// * `Drop` calls `ic_decompress_end` then `ic_close`.
//
// DirectShow codecs (`Kind::DirectShow`) still return
// `Error::Unsupported` — that path needs `IMemAllocator` +
// `IMediaSample` host stubs (round 30+).
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
    /// True once `ICDecompressBegin` has run successfully on
    /// `hic`. `Drop` calls `ICDecompressEnd` only when set.
    begin_done: bool,
    /// Stream width / height taken from `CodecParameters`. Required
    /// — VfW codecs need explicit dimensions in the input BIH
    /// (the bitstream alone is insufficient for pre-decode setup).
    width: u32,
    height: u32,
    /// Source BIH FOURCC, derived from `record.fourcc`.
    fourcc_bytes: [u8; 4],
    /// Pending packet awaiting `receive_frame`. Cleared on each
    /// frame surfaced.
    pending: Option<Packet>,
    eof: bool,
}

impl SandboxedVfwDecoder {
    fn new(record: DiscoveryRecord, params: CodecParameters) -> Result<Self> {
        let width = params.width.ok_or_else(|| {
            Error::invalid(
                "vfw discovery: CodecParameters.width is None — VfW codecs \
                 need an explicit coded width to populate BITMAPINFOHEADER \
                 before ICDecompressBegin",
            )
        })?;
        let height = params.height.ok_or_else(|| {
            Error::invalid(
                "vfw discovery: CodecParameters.height is None — VfW codecs \
                 need an explicit coded height to populate BITMAPINFOHEADER \
                 before ICDecompressBegin",
            )
        })?;
        let fourcc_bytes = fourcc_to_bytes(&record.fourcc).ok_or_else(|| {
            Error::other(format!(
                "vfw discovery: bad fourcc {:?} in record",
                record.fourcc
            ))
        })?;
        Ok(SandboxedVfwDecoder {
            codec_id: params.codec_id.clone(),
            record,
            sandbox: None,
            image: None,
            hic: 0,
            begin_done: false,
            width,
            height,
            fourcc_bytes,
            pending: None,
            eof: false,
        })
    }

    /// Build the input BIH used for query/begin — `size_image` is
    /// stubbed to 0 here; per-packet `ic_decompress` overrides the
    /// field with the actual encoded byte count.
    fn build_input_bih(&self, size_image: u32) -> Bih {
        Bih {
            bi_size: 40,
            width: self.width as i32,
            height: self.height as i32,
            planes: 1,
            bit_count: 24,
            compression: self.fourcc_bytes,
            size_image,
            x_pels_per_meter: 0,
            y_pels_per_meter: 0,
            clr_used: 0,
            clr_important: 0,
        }
    }

    /// Build the output BIH — fixed BI_RGB 24bpp (BGR byte order
    /// on disk; bottom-up since `height` is positive). round-24
    /// confirmed mpg4c32 honours BI_RGB but rejects 32bpp; Indeo
    /// (IR32_32 / IR50_32 round 7+) likewise.
    fn build_output_bih(&self) -> Bih {
        Bih {
            bi_size: 40,
            width: self.width as i32,
            height: self.height as i32,
            planes: 1,
            bit_count: 24,
            compression: [0; 4], // BI_RGB
            size_image: self.output_capacity(),
            x_pels_per_meter: 0,
            y_pels_per_meter: 0,
            clr_used: 0,
            clr_important: 0,
        }
    }

    fn output_capacity(&self) -> u32 {
        self.width * self.height * 3
    }

    /// Lazy: load DLL, install codec, ICOpen, run ICDecompressQuery
    /// + ICDecompressBegin so the codec is primed for per-packet
    ///   `ic_decompress` calls.
    fn ensure_open(&mut self) -> Result<()> {
        if self.begin_done {
            return Ok(());
        }
        if self.sandbox.is_none() {
            let bytes = std::fs::read(&self.record.dll_path)
                .map_err(|e| Error::other(format!("vfw discovery: read DLL failed: {e}")))?;
            let mut sb = crate::Sandbox::new();
            // VfW codecs (esp. mpg4c32) need a generous instruction
            // budget to walk the larger fixtures' P-frames; the
            // round-24 manual path uses 8 G instructions for the
            // 5-6-frame 352×288 fixtures.
            sb.cpu.set_instr_limit(8_000_000_000);
            let img = sb
                .load("codec.dll", &bytes)
                .map_err(|e| Error::other(format!("vfw discovery: Sandbox::load failed: {e}")))?;
            sb.install_codec(&img)
                .map_err(|e| Error::other(format!("vfw discovery: install_codec failed: {e}")))?;
            // Drive DllMain so any per-DLL CRT init runs.
            let _ = sb.call_dll_main(&img, crate::DLL_PROCESS_ATTACH);

            let fcc_handler = u32::from_le_bytes(self.fourcc_bytes);
            let fcc_type = u32::from_le_bytes(*b"VIDC");
            // Mode 2 = ICMODE_COMPRESS in vfw.h — but the round-24
            // manual path uses 2 here for MP43 because Microsoft's
            // codecs have historically been permissive about the
            // mode word at DRV_OPEN; keep the same value as the
            // manual path so the trait + manual paths exercise the
            // identical DRV_OPEN sequence (round-29 byte-equality
            // requirement). See `tests/round24_mp43_multiframe_and_wmv.rs`.
            let hic = sb
                .ic_open(fcc_type, fcc_handler, 2)
                .map_err(|e| Error::other(format!("vfw discovery: ic_open failed: {e}")))?;
            if hic == 0 {
                return Err(Error::other(
                    "vfw discovery: ICOpen returned NULL (codec rejected handler FourCC)",
                ));
            }
            self.sandbox = Some(sb);
            self.image = Some(img);
            self.hic = hic;
        }

        // ICDecompressQuery + ICDecompressBegin against the input/
        // output BIH templates. We run these here (not in `new`)
        // because the sandbox is constructed lazily and the
        // pre-handshake DllMain call may have already mutated the
        // codec's internal state — running query/begin before the
        // first packet keeps the trait path's lifecycle predictable.
        let bih_in = self.build_input_bih(0);
        let bih_out = self.build_output_bih();
        let hic = self.hic;
        let sb = self
            .sandbox
            .as_mut()
            .expect("sandbox just constructed above");
        let q = sb
            .ic_decompress_query(hic, &bih_in, Some(&bih_out))
            .map_err(|e| Error::other(format!("vfw discovery: ic_decompress_query failed: {e}")))?;
        if q != 0 {
            return Err(Error::other(format!(
                "vfw discovery: ic_decompress_query returned {q:#010x} \
                 (want 0 = ICERR_OK; codec rejected the input/output format)"
            )));
        }
        let b = sb
            .ic_decompress_begin(hic, &bih_in, &bih_out)
            .map_err(|e| Error::other(format!("vfw discovery: ic_decompress_begin failed: {e}")))?;
        if b != 0 {
            return Err(Error::other(format!(
                "vfw discovery: ic_decompress_begin returned {b:#010x} \
                 (want 0 = ICERR_OK)"
            )));
        }
        self.begin_done = true;
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
        let packet = match self.pending.take() {
            Some(p) => p,
            None => {
                return if self.eof {
                    Err(Error::Eof)
                } else {
                    Err(Error::NeedMore)
                };
            }
        };

        let hic = self.hic;
        let bih_in = self.build_input_bih(packet.data.len() as u32);
        let bih_out = self.build_output_bih();
        let cap = self.output_capacity();
        let flags = if packet.flags.keyframe {
            0
        } else {
            ICDECOMPRESS_NOTKEYFRAME
        };
        let sb = self.sandbox.as_mut().ok_or_else(|| {
            Error::other("vfw discovery: receive_frame called without prior send_packet")
        })?;
        let (lr, raw) = sb
            .ic_decompress(hic, flags, &bih_in, &packet.data, &bih_out, cap)
            .map_err(|e| Error::other(format!("vfw discovery: ic_decompress trapped: {e}")))?;
        if lr != 0 {
            return Err(Error::other(format!(
                "vfw discovery: ic_decompress returned {lr:#010x} (want 0 = ICERR_OK)"
            )));
        }

        // Codec wrote BGR24 bottom-up because `bih_out.height >= 0`.
        // Convert to top-down so consumers can treat the buffer as
        // a row-major frame without container-aware flipping.
        let width = self.width as usize;
        let height = self.height as usize;
        let stride = width * 3;
        let mut data = vec![0u8; stride * height];
        if raw.len() < stride * height {
            return Err(Error::other(format!(
                "vfw discovery: ic_decompress returned {} bytes, expected {} (stride*height)",
                raw.len(),
                stride * height,
            )));
        }
        for row in 0..height {
            let src_off = (height - 1 - row) * stride;
            let dst_off = row * stride;
            data[dst_off..dst_off + stride].copy_from_slice(&raw[src_off..src_off + stride]);
        }

        Ok(Frame::Video(VideoFrame {
            pts: packet.pts,
            planes: vec![VideoPlane { stride, data }],
        }))
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }
}

impl Drop for SandboxedVfwDecoder {
    fn drop(&mut self) {
        if let Some(sb) = self.sandbox.as_mut() {
            if self.hic != 0 {
                if self.begin_done {
                    let _ = sb.ic_decompress_end(self.hic);
                }
                let _ = sb.ic_close(self.hic);
            }
        }
    }
}

/// Stream-level pixel format for [`Frame::Video`]s emitted by
/// [`SandboxedVfwDecoder`]. The decoder always renders to
/// [`PixelFormat::Bgr24`] — VfW codecs reliably honour BI_RGB
/// 24bpp output (round-24 confirmed mpg4c32 rejects 32bpp + most
/// YUV outputs); the bottom-up storage is flipped to top-down
/// before the frame leaves the decoder, so consumers always
/// receive a row-major BGR24 buffer.
pub fn output_pixel_format() -> PixelFormat {
    PixelFormat::Bgr24
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
