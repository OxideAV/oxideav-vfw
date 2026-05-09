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
        Kind::DirectShow => Ok(Box::new(SandboxedDshowDecoder::new(
            record,
            params.clone(),
        )?)),
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
    /// Stream width / height. Resolved lazily from
    /// [`CodecParameters`] when present, or — if absent — probed
    /// from the codec via `ICM_DECOMPRESS_GET_FORMAT` after
    /// `ICDecompressQuery` accepts the input format. Round 30
    /// added the probe path; round 29 hard-required the caller
    /// to populate dims, which made the trait surface awkward
    /// for callers that don't know dims ahead of time.
    width: u32,
    height: u32,
    /// Whether `width`/`height` were known at construction time
    /// (false means we need to run the GET_FORMAT probe on first
    /// `ensure_open` after a successful `ICDecompressQuery`).
    dims_from_params: bool,
    /// Source BIH FOURCC, derived from `record.fourcc`.
    fourcc_bytes: [u8; 4],
    /// Pending packet awaiting `receive_frame`. Cleared on each
    /// frame surfaced.
    pending: Option<Packet>,
    eof: bool,
}

impl SandboxedVfwDecoder {
    fn new(record: DiscoveryRecord, params: CodecParameters) -> Result<Self> {
        let dims_from_params = params.width.is_some() && params.height.is_some();
        // Use placeholder dims when missing; the GET_FORMAT probe
        // populates them in `ensure_open` after `ICDecompressQuery`.
        let width = params.width.unwrap_or(0);
        let height = params.height.unwrap_or(0);
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
            dims_from_params,
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
        // Saturating so an as-yet-unprobed (0×0) decoder produces a
        // sensible 0 here rather than wrapping; the
        // `ICDecompressBegin` path validates dims afterwards.
        self.width.saturating_mul(self.height).saturating_mul(3)
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
        //
        // If the caller didn't supply dims on `CodecParameters`,
        // synthesise a placeholder input BIH and probe the codec
        // via `ICM_DECOMPRESS_GET_FORMAT` first. The codec writes
        // the output BIH (carrying the codec-known dims for the
        // bound stream); we then re-build the input/output BIHs
        // with the probed dims for the real query+begin.
        if !self.dims_from_params {
            // GET_FORMAT needs *some* input BIH. Use 0×0 — codecs
            // that key on dims will simply mirror the dims back
            // into the output BIH at decode time. For codecs that
            // refuse 0×0 here, dims_from_params stays the
            // hard-error path (callers can still pass them).
            let probe_in = self.build_input_bih(0);
            let hic = self.hic;
            let sb = self
                .sandbox
                .as_mut()
                .expect("sandbox just constructed above");
            match sb.ic_decompress_get_format(hic, &probe_in) {
                Ok((rc, out)) if rc == 0 && out.width > 0 && out.height > 0 => {
                    self.width = out.width.unsigned_abs();
                    self.height = out.height.unsigned_abs();
                }
                Ok((rc, out)) => {
                    return Err(Error::invalid(format!(
                        "vfw discovery: ICM_DECOMPRESS_GET_FORMAT \
                         could not establish dims (rc={rc:#010x}, \
                         out_width={}, out_height={}); pass \
                         CodecParameters.{{width,height}} explicitly",
                        out.width, out.height
                    )));
                }
                Err(e) => {
                    return Err(Error::invalid(format!(
                        "vfw discovery: ICM_DECOMPRESS_GET_FORMAT failed: {e}; \
                         pass CodecParameters.{{width,height}} explicitly"
                    )));
                }
            }
        }
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

// ────────────────────────────────────────────────────────────────
// SandboxedDshowDecoder — round 30.
//
// Wires a `oxideav_core::Decoder` against a DirectShow filter
// `.ax`. On `send_packet`:
//
// * `ensure_open` (lazy on first packet): load DLL, drive
//   DllMain, drive `DllGetClassObject(CLSID, IID_IClassFactory)`
//   then `IClassFactory::CreateInstance(NULL, IID_IBaseFilter,
//   &filter)`. Walk `IBaseFilter::EnumPins → IEnumPins::Next`
//   for the first input pin. Mint a host IFilterGraph, call
//   `IBaseFilter::JoinFilterGraph(host_graph, NULL)`. Stage an
//   AM_MEDIA_TYPE for the discovery FourCC + 320×240 (or
//   user-supplied dims), mint a host output pin advertising
//   the AMT, call `IPin::ReceiveConnection(host_out_pin, &amt)`.
// * Then QI the input pin for `IID_IMemInputPin`, mint a
//   HostIMemAllocator (4-sample pool, sample capacity =
//   `max(packet.data.len(), 64 KiB)`), call
//   `IMemInputPin::NotifyAllocator(host_alloc, FALSE)`. Stage
//   the packet bytes into the first sample, call
//   `IMemInputPin::Receive(sample)`.
//
// The codec's *output* path needs a downstream HostIPin::Receive
// callback wired into the codec's output pin — that's the r31
// gap the GOAL doc calls out. For r30 we observe via
// `Cpu::trace_ring` what the codec did during `Receive` and
// surface `Error::Unsupported` carrying the captured ring head/tail
// so the next round can mine it.
// ────────────────────────────────────────────────────────────────

struct SandboxedDshowDecoder {
    codec_id: CodecId,
    record: DiscoveryRecord,
    sandbox: Option<crate::Sandbox>,
    image: Option<crate::pe::Image>,
    /// IBaseFilter pointer (after CreateInstance).
    filter: u32,
    /// First input pin (after EnumPins → Next).
    input_pin: u32,
    /// Cached IMemInputPin (after QI).
    mem_input_pin: u32,
    /// Host IFilterGraph (after JoinFilterGraph).
    host_graph: u32,
    /// Host IMemAllocator (after NotifyAllocator).
    host_allocator: u32,
    /// Whether ReceiveConnection has succeeded — only after that
    /// can we safely proceed to NotifyAllocator + Receive.
    connection_done: bool,
    width: u32,
    height: u32,
    fourcc_bytes: [u8; 4],
    pending: Option<Packet>,
    eof: bool,
}

impl SandboxedDshowDecoder {
    fn new(record: DiscoveryRecord, params: CodecParameters) -> Result<Self> {
        let fourcc_bytes = fourcc_to_bytes(&record.fourcc).ok_or_else(|| {
            Error::other(format!(
                "vfw discovery: bad fourcc {:?} in record",
                record.fourcc
            ))
        })?;
        Ok(SandboxedDshowDecoder {
            codec_id: params.codec_id.clone(),
            record,
            sandbox: None,
            image: None,
            filter: 0,
            input_pin: 0,
            mem_input_pin: 0,
            host_graph: 0,
            host_allocator: 0,
            connection_done: false,
            // DShow path is more permissive than VfW; default to
            // 320×240 if dims missing — the negotiation may
            // override during ReceiveConnection.
            width: params.width.unwrap_or(320),
            height: params.height.unwrap_or(240),
            fourcc_bytes,
            pending: None,
            eof: false,
        })
    }

    fn ensure_open(&mut self) -> Result<()> {
        if self.connection_done {
            return Ok(());
        }
        if self.sandbox.is_none() {
            let bytes = std::fs::read(&self.record.dll_path).map_err(|e| {
                Error::other(format!("vfw discovery (DShow): read DLL failed: {e}"))
            })?;
            let mut sb = crate::Sandbox::new();
            sb.cpu.set_instr_limit(8_000_000_000);
            let img = sb.load("codec.ax", &bytes).map_err(|e| {
                Error::other(format!("vfw discovery (DShow): Sandbox::load failed: {e}"))
            })?;
            let _ = sb.call_dll_main(&img, crate::DLL_PROCESS_ATTACH);

            // Resolve the CLSID from the discovery record.
            let clsid_str = self.record.clsid.as_deref().ok_or_else(|| {
                Error::unsupported(
                    "vfw discovery (DShow): record carries no CLSID — \
                     can't drive DllGetClassObject",
                )
            })?;
            let clsid = crate::com::Guid::parse(clsid_str).map_err(|e| {
                Error::other(format!(
                    "vfw discovery (DShow): bad CLSID {clsid_str:?}: {e}"
                ))
            })?;
            let _factory = sb
                .dll_get_class_object(&img, clsid, crate::IID_ICLASSFACTORY)
                .map_err(|e| {
                    Error::other(format!(
                        "vfw discovery (DShow): DllGetClassObject failed: {e}"
                    ))
                })?;
            let filter = sb
                .co_create_instance(clsid, crate::IID_IBASEFILTER)
                .map_err(|e| {
                    Error::other(format!(
                        "vfw discovery (DShow): co_create_instance failed: {e}"
                    ))
                })?;
            if filter == 0 {
                return Err(Error::other(
                    "vfw discovery (DShow): CreateInstance returned NULL filter",
                ));
            }
            self.sandbox = Some(sb);
            self.image = Some(img);
            self.filter = filter;
        }

        let sb = self.sandbox.as_mut().expect("sandbox just constructed");

        // Walk EnumPins → Next for the first input pin.
        if self.input_pin == 0 {
            let pin = first_input_pin(sb, self.filter).ok_or_else(|| {
                Error::other("vfw discovery (DShow): could not enumerate input pin")
            })?;
            self.input_pin = pin;
        }

        // Mint host IFilterGraph + JoinFilterGraph.
        if self.host_graph == 0 {
            let host_graph = sb.mint_host_filter_graph().map_err(|e| {
                Error::other(format!("vfw discovery (DShow): mint host graph: {e}"))
            })?;
            let _ = crate::com::call::call_method(
                &mut sb.cpu,
                &mut sb.mmu,
                &sb.registry,
                &mut sb.host,
                self.filter,
                crate::com::SLOT_BASEFILTER_JOIN_FILTER_GRAPH,
                &[host_graph, 0],
            );
            self.host_graph = host_graph;
        }

        // Round 31 A — walk the codec's input pin AMT enumeration
        // first.  If it surfaces any AMTs, prefer them over the
        // fabricated VIH+BIH the round-30 path forced.
        let captured = crate::com::host_iface_r31::walk_codec_input_pin_amts(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            self.input_pin,
            8,
        )
        .map_err(|e| {
            Error::other(format!(
                "vfw discovery (DShow): walk_codec_input_pin_amts: {e}"
            ))
        })?;
        if !captured.is_empty() {
            log::debug!(
                "vfw discovery (DShow): captured {} codec-native AMT(s); first subtype={}",
                captured.len(),
                captured[0].subtype
            );
        }

        let synth_amt =
            stage_am_media_type_dshow(sb, self.fourcc_bytes, self.width as i32, self.height as i32)
                .map_err(|e| Error::other(format!("vfw discovery (DShow): stage AMT: {e}")))?;

        let mut accepted_amt = 0u32;
        let mut last_hr = 0u32;
        for (i, cap) in captured.iter().enumerate() {
            let host_out_pin = sb.mint_host_output_pin(cap.amt_addr).map_err(|e| {
                Error::other(format!(
                    "vfw discovery (DShow): mint host output pin (codec amt {i}): {e}"
                ))
            })?;
            let r = crate::com::call::call_method(
                &mut sb.cpu,
                &mut sb.mmu,
                &sb.registry,
                &mut sb.host,
                self.input_pin,
                4, // SLOT_PIN_RECEIVE_CONNECTION
                &[host_out_pin, cap.amt_addr],
            )
            .map_err(|e| {
                Error::other(format!(
                    "vfw discovery (DShow): IPin::ReceiveConnection (codec amt {i}) trapped: {e}"
                ))
            })?;
            last_hr = r;
            if r == 0 {
                accepted_amt = cap.amt_addr;
                break;
            }
        }
        if accepted_amt == 0 {
            // Fall back to synthetic AMT.
            let host_out_pin = sb.mint_host_output_pin(synth_amt).map_err(|e| {
                Error::other(format!(
                    "vfw discovery (DShow): mint host output pin (synth): {e}"
                ))
            })?;
            let r = crate::com::call::call_method(
                &mut sb.cpu,
                &mut sb.mmu,
                &sb.registry,
                &mut sb.host,
                self.input_pin,
                4, // SLOT_PIN_RECEIVE_CONNECTION
                &[host_out_pin, synth_amt],
            )
            .map_err(|e| {
                Error::other(format!(
                    "vfw discovery (DShow): IPin::ReceiveConnection (synth) trapped: {e}"
                ))
            })?;
            if r != 0 {
                return Err(Error::unsupported(format!(
                    "vfw discovery (DShow): IPin::ReceiveConnection rejected every \
                     candidate AMT (codec-native count={}, last codec-native HRESULT \
                     {last_hr:#010x}, synth HRESULT {r:#010x})",
                    captured.len()
                )));
            }
            accepted_amt = synth_amt;
        }
        let amt = accepted_amt;
        self.connection_done = true;

        // QI for IMemInputPin.
        let mip = sb
            .query_interface(self.input_pin, crate::IID_IMEMINPUTPIN)
            .map_err(|e| {
                Error::other(format!(
                    "vfw discovery (DShow): QI IMemInputPin failed: {e}"
                ))
            })?;
        self.mem_input_pin = mip;

        // Mint host IMemAllocator + drive NotifyAllocator(alloc, FALSE).
        // Sample capacity is 256 KiB by default — large enough for
        // 320×240 keyframes from the discovery FourCCs we drive
        // here. Pool size 4 leaves room for codec-side queueing
        // without exhausting the arena.
        let cap = 256 * 1024;
        let alloc = sb.mint_host_mem_allocator(4, cap, amt).map_err(|e| {
            Error::other(format!("vfw discovery (DShow): mint host allocator: {e}"))
        })?;
        self.host_allocator = alloc;
        let r_na = crate::com::call::call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            mip,
            4, // SLOT_MEMINPUTPIN_NOTIFY_ALLOCATOR
            &[alloc, 0],
        )
        .map_err(|e| {
            Error::other(format!(
                "vfw discovery (DShow): NotifyAllocator trapped: {e}"
            ))
        })?;
        // We accept any HRESULT here — some codecs return E_NOTIMPL
        // for NotifyAllocator and rely entirely on GetAllocator;
        // the host-allocator path is best-effort.
        log::debug!("vfw discovery (DShow): NotifyAllocator → {r_na:#010x}; alloc = {alloc:#010x}");

        // Round 31 B — wire a downstream HostIPin / HostIMemInputPin
        // pair into the codec's output pin so that when the codec
        // emits a decoded sample, our `Receive` callback captures
        // the bytes into the per-state queue.  The output pin's
        // ReceiveConnection is best-effort — some codecs don't
        // expose a separate output pin (transform-in-place); for
        // those we still pre-mint the host pair so that QI from the
        // input side can find it.
        let (h_pin, h_mip) = sb.host_iface_r31_mint_input_pin_pair().map_err(|e| {
            Error::other(format!(
                "vfw discovery (DShow): mint host input pin pair: {e}"
            ))
        })?;
        let _h_filter = sb.host_iface_r31_mint_base_filter(h_pin).map_err(|e| {
            Error::other(format!("vfw discovery (DShow): mint host base filter: {e}"))
        })?;
        if let Some(out_pin) = first_output_pin_dshow(sb, self.filter, self.input_pin) {
            // Stage a downstream RGB24 AMT.
            let dn_amt = stage_am_media_type_rgb24_dshow(sb, self.width as i32, self.height as i32)
                .map_err(|e| {
                    Error::other(format!(
                        "vfw discovery (DShow): stage downstream RGB24 AMT: {e}"
                    ))
                })?;
            let r_dn = crate::com::call::call_method(
                &mut sb.cpu,
                &mut sb.mmu,
                &sb.registry,
                &mut sb.host,
                out_pin,
                4, // SLOT_PIN_RECEIVE_CONNECTION
                &[h_pin, dn_amt],
            )
            .map_err(|e| {
                Error::other(format!(
                    "vfw discovery (DShow): output ReceiveConnection trapped: {e}"
                ))
            })?;
            log::debug!(
                "vfw discovery (DShow): downstream Connect → {r_dn:#010x} (out_pin={out_pin:#010x})"
            );
        }
        let _ = h_mip; // retained on the sandbox via QI on h_pin.
        Ok(())
    }
}

/// Round 31 — find a PIN_OUTPUT pin on the codec filter (skipping
/// `skip` which is already-bound input).  Returns `None` if the
/// filter has no output pin.
fn first_output_pin_dshow(sb: &mut crate::Sandbox, filter: u32, skip: u32) -> Option<u32> {
    use crate::com::call::call_method;
    // Walk EnumPins to gather all pin pointers.
    let mut pins = Vec::new();
    let scratch = sb.host.arena_alloc(8).ok()?;
    let _ = sb.mmu.write_initializer(scratch, &[0u8; 8]);
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        crate::com::SLOT_BASEFILTER_ENUM_PINS,
        &[scratch],
    );
    let pp = sb.mmu.load32(scratch).unwrap_or(0);
    if !matches!(r, Ok(0)) || pp == 0 {
        return None;
    }
    sb.host.com.intern(pp, None);
    for _ in 0..16 {
        let pin_slot = sb.host.arena_alloc(8).ok()?;
        let _ = sb.mmu.write_initializer(pin_slot, &[0u8; 8]);
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            pp,
            3, // IEnumPins::Next
            &[1, pin_slot, pin_slot + 4],
        );
        let pin = sb.mmu.load32(pin_slot).unwrap_or(0);
        let fetched = sb.mmu.load32(pin_slot + 4).unwrap_or(0);
        if !matches!(r, Ok(0) | Ok(1)) || pin == 0 || fetched == 0 {
            break;
        }
        sb.host.com.intern(pin, None);
        pins.push(pin);
        if matches!(r, Ok(1)) {
            break;
        }
    }
    let _ = sb.com_release(pp);
    for pin in pins {
        if pin == skip {
            continue;
        }
        let dir_slot = sb.host.arena_alloc(4).ok()?;
        let _ = sb.mmu.write_initializer(dir_slot, &0u32.to_le_bytes());
        let r = call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            pin,
            9, // IPin::QueryDirection
            &[dir_slot],
        );
        if !matches!(r, Ok(0)) {
            continue;
        }
        let dir = sb.mmu.load32(dir_slot).unwrap_or(0);
        if dir == 1 {
            return Some(pin);
        }
    }
    None
}

/// Stage a downstream RGB24 AM_MEDIA_TYPE.  Used by round 31 B.
fn stage_am_media_type_rgb24_dshow(
    sb: &mut crate::Sandbox,
    width: i32,
    height: i32,
) -> Result<u32> {
    let to_oxide =
        |e: crate::Error| Error::other(format!("vfw discovery (DShow): stage RGB24 AMT: {e}"));
    let blob = sb
        .host
        .arena_alloc(72 + 88 + 16)
        .map_err(|e| to_oxide(crate::Error::Win32(e)))?;
    let amt = blob;
    let fmt = blob + 72;
    let mediatype_video =
        crate::com::Guid::parse("{73646976-0000-0010-8000-00AA00389B71}").unwrap();
    let format_videoinfo =
        crate::com::Guid::parse("{05589F80-C356-11CE-BF01-00AA0055595A}").unwrap();
    let mediasubtype_rgb24 =
        crate::com::Guid::parse("{E436EB7D-524F-11CE-9F53-0020AF0BA770}").unwrap();
    let trap = |e: crate::emulator::Trap| to_oxide(crate::Error::Trap(e));
    mediatype_video.stage(&mut sb.mmu, amt).map_err(trap)?;
    mediasubtype_rgb24
        .stage(&mut sb.mmu, amt + 16)
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 32, &1u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 36, &0u32.to_le_bytes())
        .map_err(trap)?;
    let stride = (width.unsigned_abs() * 3 + 3) & !3;
    let img = stride * height.unsigned_abs();
    sb.mmu
        .write_initializer(amt + 40, &img.to_le_bytes())
        .map_err(trap)?;
    format_videoinfo
        .stage(&mut sb.mmu, amt + 44)
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 60, &0u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 64, &88u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 68, &fmt.to_le_bytes())
        .map_err(trap)?;
    for i in 0..48u32 {
        sb.mmu.store8(fmt + i, 0).map_err(trap)?;
    }
    let bih = fmt + 48;
    sb.mmu
        .write_initializer(bih, &40u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(bih + 4, &(width as u32).to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(bih + 8, &(height as u32).to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(bih + 12, &1u16.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(bih + 14, &24u16.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(bih + 16, &0u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(bih + 20, &img.to_le_bytes())
        .map_err(trap)?;
    for off in [24u32, 28, 32, 36] {
        sb.mmu
            .write_initializer(bih + off, &0u32.to_le_bytes())
            .map_err(trap)?;
    }
    Ok(amt)
}

/// Stage an AM_MEDIA_TYPE (72 B) + VIDEOINFOHEADER (88 B) in arena
/// memory describing a video stream of (`fourcc`, `width × height`,
/// 24 bpp). Returns the AMT's guest VA. Thin equivalent of the
/// round-27 test helper, lifted into the production module so
/// `SandboxedDshowDecoder` can reuse it.
fn stage_am_media_type_dshow(
    sb: &mut crate::Sandbox,
    fourcc: [u8; 4],
    width: i32,
    height: i32,
) -> Result<u32> {
    let to_oxide = |e: crate::Error| Error::other(format!("vfw discovery (DShow): stage AMT: {e}"));
    let blob = sb
        .host
        .arena_alloc(72 + 88 + 16)
        .map_err(|e| to_oxide(crate::Error::Win32(e)))?;
    let amt = blob;
    let fmt = blob + 72;

    // AM_MEDIA_TYPE @ amt.
    let mediatype_video =
        crate::com::Guid::parse("{73646976-0000-0010-8000-00AA00389B71}").unwrap();
    let format_videoinfo =
        crate::com::Guid::parse("{05589F80-C356-11CE-BF01-00AA0055595A}").unwrap();
    let d1 = u32::from_le_bytes(fourcc);
    let subtype = crate::com::Guid::new(
        d1,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    );

    let trap = |e: crate::emulator::Trap| to_oxide(crate::Error::Trap(e));
    mediatype_video.stage(&mut sb.mmu, amt).map_err(trap)?;
    subtype.stage(&mut sb.mmu, amt + 16).map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 32, &1u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 36, &1u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 40, &0u32.to_le_bytes())
        .map_err(trap)?;
    format_videoinfo
        .stage(&mut sb.mmu, amt + 44)
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 60, &0u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 64, &88u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(amt + 68, &fmt.to_le_bytes())
        .map_err(trap)?;

    // VIH @ fmt — first 48 bytes (rcSource + rcTarget + dwBitRate +
    // dwBitErrorRate + AvgTimePerFrame) zeroed; BIH at fmt+48.
    for i in 0..48u32 {
        sb.mmu.store8(fmt + i, 0).map_err(trap)?;
    }
    let bih = fmt + 48;
    sb.mmu
        .write_initializer(bih, &40u32.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(bih + 4, &(width as u32).to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(bih + 8, &(height as u32).to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(bih + 12, &1u16.to_le_bytes())
        .map_err(trap)?;
    sb.mmu
        .write_initializer(bih + 14, &24u16.to_le_bytes())
        .map_err(trap)?;
    sb.mmu.write_initializer(bih + 16, &fourcc).map_err(trap)?;
    let size_image = (width.unsigned_abs() * height.unsigned_abs() * 3) / 2;
    sb.mmu
        .write_initializer(bih + 20, &size_image.to_le_bytes())
        .map_err(trap)?;
    for off in [24u32, 28, 32, 36] {
        sb.mmu
            .write_initializer(bih + off, &0u32.to_le_bytes())
            .map_err(trap)?;
    }
    Ok(amt)
}

/// Walk the codec's IBaseFilter::EnumPins → IEnumPins::Next chain
/// for the first pin (assumed to be the input pin — DirectShow
/// convention is input pins enumerate first, output pins next).
fn first_input_pin(sb: &mut crate::Sandbox, filter: u32) -> Option<u32> {
    use crate::com::call::call_method;
    // Stop the filter so ReceiveConnection is legal.
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        crate::com::SLOT_BASEFILTER_STOP,
        &[],
    );
    let scratch = sb.host.arena_alloc(8).ok()?;
    sb.mmu.write_initializer(scratch, &[0u8; 8]).ok()?;
    let r = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        filter,
        crate::com::SLOT_BASEFILTER_ENUM_PINS,
        &[scratch],
    );
    let pp = sb.mmu.load32(scratch).unwrap_or(0);
    if !matches!(r, Ok(0)) || pp == 0 {
        return None;
    }
    sb.host.com.intern(pp, None);
    let pin_slot = sb.host.arena_alloc(8).ok()?;
    sb.mmu.write_initializer(pin_slot, &[0u8; 8]).ok()?;
    let _ = call_method(
        &mut sb.cpu,
        &mut sb.mmu,
        &sb.registry,
        &mut sb.host,
        pp,
        3, // IEnumPins::Next
        &[1, pin_slot, pin_slot + 4],
    );
    let pin = sb.mmu.load32(pin_slot).unwrap_or(0);
    if pin != 0 {
        sb.host.com.intern(pin, None);
    }
    let _ = sb.com_release(pp);
    if pin == 0 {
        None
    } else {
        Some(pin)
    }
}

impl Decoder for SandboxedDshowDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        if self.pending.is_some() {
            return Err(Error::other(
                "vfw discovery (DShow): receive_frame must be called before sending another packet",
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
        let mip = self.mem_input_pin;
        if mip == 0 {
            return Err(Error::other(
                "vfw discovery (DShow): IMemInputPin not bound; ensure_open did not complete",
            ));
        }
        let sb = self
            .sandbox
            .as_mut()
            .ok_or_else(|| Error::other("vfw discovery (DShow): no sandbox"))?;

        // Acquire a sample from the host allocator: emulate the
        // codec calling `IMemAllocator::GetBuffer(&sample, 0, 0, 0)`
        // by driving the host stub directly (we own the allocator
        // so this is a no-emulation call).
        let pp = sb
            .host
            .arena_alloc(4)
            .map_err(|e| Error::other(format!("vfw discovery (DShow): arena: {e}")))?;
        sb.mmu
            .write_initializer(pp, &0u32.to_le_bytes())
            .map_err(|e| Error::other(format!("vfw discovery (DShow): mmu init: {e}")))?;
        let r_gb = crate::com::call::call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            self.host_allocator,
            7, // SLOT_MEMALLOCATOR_GET_BUFFER
            &[pp, 0, 0, 0],
        )
        .map_err(|e| Error::other(format!("vfw discovery (DShow): GetBuffer trapped: {e}")))?;
        if r_gb != 0 {
            return Err(Error::other(format!(
                "vfw discovery (DShow): host GetBuffer returned {r_gb:#010x} (pool exhausted?)"
            )));
        }
        let sample = sb
            .mmu
            .load32(pp)
            .map_err(|e| Error::other(format!("vfw discovery (DShow): load sample slot: {e}")))?;
        if sample == 0 {
            return Err(Error::other(
                "vfw discovery (DShow): host GetBuffer succeeded but sample = 0",
            ));
        }

        // Stage the packet bytes + sync flag into the sample.
        sb.media_sample_set_payload(sample, &packet.data, packet.flags.keyframe)
            .map_err(|e| {
                Error::other(format!(
                    "vfw discovery (DShow): media_sample_set_payload: {e}"
                ))
            })?;

        // Snapshot trace ring before Receive so we can document
        // what the codec did.
        sb.cpu.enable_trace_ring(64);

        // Drive IMemInputPin::Receive(sample).
        let r_recv = crate::com::call::call_method(
            &mut sb.cpu,
            &mut sb.mmu,
            &sb.registry,
            &mut sb.host,
            mip,
            6, // SLOT_MEMINPUTPIN_RECEIVE
            &[sample],
        )
        .map_err(|e| Error::other(format!("vfw discovery (DShow): Receive trapped: {e}")))?;

        // Round 31 — drain the host-side queue populated by the
        // downstream `HostIMemInputPin::Receive` callback.
        if let Some(rs) = sb.pop_received_sample() {
            return surface_received_dshow_frame(rs, packet.pts, self.width, self.height);
        }

        // Capture trace ring head / tail for diagnostics.
        let ring = sb.cpu.trace_ring.clone();
        let ring_summary = if ring.is_empty() {
            String::from("empty")
        } else {
            let head: Vec<String> = ring.iter().take(4).map(|e| format!("{e:#010x}")).collect();
            let tail: Vec<String> = ring
                .iter()
                .rev()
                .take(4)
                .map(|e| format!("{e:#010x}"))
                .collect();
            format!("len={} head={:?} tail={:?}", ring.len(), head, tail)
        };

        // Per round-31 goal: prefer Eof when codec accepted input
        // but emitted nothing (vs round 30's Unsupported path).
        if r_recv == 0 {
            log::debug!(
                "vfw discovery (DShow): Receive ok but no output sample queued \
                 (trace_ring {ring_summary})"
            );
            Err(Error::Eof)
        } else {
            Err(Error::unsupported(format!(
                "vfw discovery (DShow): IMemInputPin::Receive → {r_recv:#010x}; \
                 no decoded sample emitted (trace_ring {ring_summary})"
            )))
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }
}

impl Drop for SandboxedDshowDecoder {
    fn drop(&mut self) {
        if let Some(sb) = self.sandbox.as_mut() {
            if self.mem_input_pin != 0 {
                let _ = sb.com_release(self.mem_input_pin);
            }
            if self.input_pin != 0 {
                let _ = sb.com_release(self.input_pin);
            }
            if self.filter != 0 {
                let _ = sb.com_release(self.filter);
            }
            crate::com::host_iface_r31::clear_queue(&sb.host);
        }
    }
}

/// Round 31 — turn a host-captured `ReceivedSample` into a
/// `Frame::Video`.  Assumes the codec emitted RGB24 in the
/// negotiated dimensions; pads / truncates to `stride * height`.
/// Bottom-up storage is flipped to top-down to match the VfW
/// path's surface convention.
fn surface_received_dshow_frame(
    rs: crate::com::host_iface_r31::ReceivedSample,
    pts: Option<i64>,
    width: u32,
    height: u32,
) -> Result<Frame> {
    let w = width as usize;
    let h = height as usize;
    let stride = w * 3;
    let expected = stride * h;
    let raw = if rs.data.len() >= expected {
        rs.data[..expected].to_vec()
    } else {
        let mut padded = vec![0u8; expected];
        padded[..rs.data.len()].copy_from_slice(&rs.data);
        padded
    };
    let mut data = vec![0u8; expected];
    if h > 0 && stride > 0 {
        for row in 0..h {
            let src_off = (h - 1 - row) * stride;
            let dst_off = row * stride;
            data[dst_off..dst_off + stride].copy_from_slice(&raw[src_off..src_off + stride]);
        }
    }
    Ok(Frame::Video(VideoFrame {
        pts,
        planes: vec![VideoPlane { stride, data }],
    }))
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
    fn make_decoder_dshow_constructs_decoder_without_dll() {
        // Round 30 — DShow path now constructs a `SandboxedDshowDecoder`
        // at make_decoder time; the actual DLL load + handshake happen
        // lazily on the first `send_packet`. Constructor only validates
        // the FourCC. (Round 29 used to return Err(Unsupported) here.)
        let id = "vfw_dshow_make_decoder_test_round30";
        register_factory_for_id(
            id,
            DiscoveryRecord {
                dll_path: PathBuf::from("/dev/null"),
                fourcc: "WMV3".to_string(),
                kind: Kind::DirectShow,
                clsid: Some("{82CCD3E0-F71A-11D0-9FE5-00609778EA66}".into()),
            },
        );
        let params = CodecParameters::video(CodecId::new(id));
        let decoder = make_decoder(&params).expect("DShow make_decoder constructs lazily");
        assert_eq!(decoder.codec_id().as_str(), id);
    }
}
