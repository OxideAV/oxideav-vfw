//! Test-side RIFF/AVI chunk walker.
//!
//! Round 8 needs to feed Intel's `IR50_32.DLL` (Indeo 5) a real
//! IV50 keyframe extracted from `cat_attack.avi`. The fixture
//! corpus has no separate "first-frame" file, so we parse just
//! enough of the AVI container to locate the first video stream's
//! sample 0 and hand back its bytes. This lives in `tests/` so
//! the production crate gets no AVI parser surface.
//!
//! Reference (clean-room): the public RIFF specification (IBM /
//! Microsoft "Multimedia Programming Interface and Data
//! Specifications 1.0", §"RIFF Form Definition") + the
//! Microsoft AVI 1.0 documentation as published in the Windows
//! Multimedia SDK ("AVI RIFF File Reference") + the Microsoft
//! "OpenDML AVI File Format Extensions" specification (September
//! 1997, §3 "AVI 2.0 Extensions"). Together these describe the
//! container as a chunk-tagged byte stream:
//!
//! * Top-level: `RIFF` form. The first segment is type `AVI `
//!   and contains the `LIST hdrl` + first `LIST movi`.
//! * AVI 2.0 ("OpenDML") supports chained RIFF segments: when
//!   the AVI exceeds the 1 GiB safe-write limit of AVI 1.0, the
//!   muxer concatenates additional `RIFF size 'AVIX'` segments,
//!   each containing only a `LIST movi` body (no hdrl). The
//!   sample chunks across all segments share the same stream
//!   indexing + FourCC convention.
//! * `LIST` chunks have a 4-byte sub-form FourCC followed by
//!   nested chunks. For `LIST hdrl` the body holds `avih` +
//!   per-stream `LIST strl`. For `LIST strl` the body holds
//!   `strh` + `strf` + (`strn` / `strd` / `JUNK` / `indx`).
//! * `LIST movi` holds the actual sample chunks named
//!   `<2-digit stream index><2-byte 4cc>`. The two-byte 4cc
//!   varies — `dc` is the canonical "compressed video" tag,
//!   but some encoders write the codec letters directly
//!   (e.g. `iv` for Indeo video). We match by the leading
//!   stream-index digits + ignore the trailing 2 bytes. AVI 2.0
//!   movi bodies may also contain `ix##` standard-index chunks
//!   (the per-segment sibling of the `indx` super-index in
//!   `strl`); these have non-numeric leading bytes and are
//!   transparently skipped by the stream-index match.
//!
//! NEVER reference `libavformat/avi*.c`. The implementation
//! was authored from the public RIFF + AVI 1.0 + AVI 2.0
//! (OpenDML) documentation alone.

use std::convert::TryInto;

/// Metadata + payload for the first video sample in an AVI file.
#[derive(Debug, Clone)]
pub struct FirstSample {
    /// Codec FourCC from the BITMAPINFOHEADER inside `strf`,
    /// e.g. `IV50` reads as `u32::from_le_bytes(*b"IV50")`.
    pub codec_fourcc: u32,
    /// Coded width from the AVI main-header (`avih.dwWidth`).
    pub width: u32,
    /// Coded height from the AVI main-header (`avih.dwHeight`).
    pub height: u32,
    /// File offset of sample 0's bytes (skipping the 8-byte
    /// chunk header).
    pub sample_offset: u32,
    /// Byte length of sample 0 (the chunk's `cksize` field).
    pub sample_size: u32,
    /// The sample's bytes, length == `sample_size`.
    pub bytes: Vec<u8>,
}

/// Top-level entry: parse `avi_bytes`, find the first video
/// stream, return the first compressed-video chunk inside
/// `LIST movi`.
pub fn extract_first_video_sample(avi_bytes: &[u8]) -> Result<FirstSample, String> {
    extract_video_sample(avi_bytes, 0)
}

/// One RIFF segment view: form-type ('AVI ' or 'AVIX') and a
/// pointer to the body bytes plus their absolute file offset
/// (used to surface real `sample_offset` values to the caller).
#[derive(Debug, Clone, Copy)]
struct RiffSegment<'a> {
    form_type: [u8; 4],
    body: &'a [u8],
    body_file_off: usize,
}

/// Walk top-level RIFF chunks. The first MUST be `RIFF AVI `;
/// any further `RIFF AVIX` segments (OpenDML AVI 2.0 chained
/// segments per Microsoft "OpenDML AVI File Format Extensions"
/// §3) are appended in order. Other top-level forms are
/// skipped silently. Returns the segments in file order.
fn walk_riff_segments(avi_bytes: &[u8]) -> Result<Vec<RiffSegment<'_>>, String> {
    if avi_bytes.len() < 12 {
        return Err("file shorter than RIFF header".into());
    }
    if &avi_bytes[0..4] != b"RIFF" {
        return Err(format!(
            "not a RIFF file: leading bytes {:02x?}",
            &avi_bytes[0..4]
        ));
    }

    let mut segs: Vec<RiffSegment<'_>> = Vec::new();
    let mut pos: usize = 0;
    while pos + 12 <= avi_bytes.len() {
        let kind: &[u8] = &avi_bytes[pos..pos + 4];
        let size = u32::from_le_bytes(avi_bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let form_bytes: [u8; 4] = avi_bytes[pos + 8..pos + 12].try_into().unwrap();

        // Clamp to what's available — `size` may overrun the
        // file (capture-card crash dumps, truncated mirrors).
        // For chained-RIFF traversal we want every segment we
        // can see, even truncated ones. The 4-byte form-type
        // already lives inside `size`, so the body (everything
        // after the form-type) is `clamped_size - 4` bytes.
        // Some real fixtures (e.g. round-14's `indeo5.avi`)
        // declare a `size` 4 bytes shorter than they really
        // are — the muxer forgot to count the form-type. In
        // that case `clamped_size < 4`, which would underflow
        // the body slice; we treat the body as empty and
        // fall through to the next-segment scan rather than
        // erroring out.
        let avail = avi_bytes.len() - pos - 8;
        let clamped_size = size.min(avail);
        let body_start = pos + 12;
        let body_end_unclamped = pos + 8 + clamped_size;
        let body_end = body_end_unclamped.max(body_start);
        let body = &avi_bytes[body_start..body_end];
        let body_file_off = body_start;

        if kind == b"RIFF" {
            // First segment must be AVI ; subsequent are
            // typically AVIX (OpenDML). Surface every accepted
            // form; `extract_video_sample` decides which carries
            // hdrl + which carries movi.
            if segs.is_empty() && &form_bytes != b"AVI " {
                return Err(format!(
                    "RIFF form-type is not AVI: {:?}",
                    std::str::from_utf8(&form_bytes).unwrap_or("???"),
                ));
            }
            segs.push(RiffSegment {
                form_type: form_bytes,
                body,
                body_file_off,
            });
        } else if kind == b"\0\0\0\0" || size == 0 {
            // Padding / zero-bytes after the last RIFF. Many
            // muxers pad to a 2 KiB boundary with zeros; the
            // round-14 IV50 corpus's `indeo5.avi` is one
            // example. Stop scanning — there are no more
            // RIFF segments.
            break;
        }
        // Any other top-level FourCC (e.g. a stray `JUNK`) is
        // ignored and we advance past it.

        // Advance to next top-level chunk.
        let advance = if clamped_size < size {
            // Truncated — no further segments are reachable.
            avi_bytes.len() - pos
        } else {
            8 + size + (size & 1)
        };
        let next = pos.checked_add(advance).ok_or("walker pos overflow")?;
        if next <= pos {
            // Defensive — never loop on a zero-size chunk.
            break;
        }
        pos = next;
    }

    if segs.is_empty() {
        return Err("no RIFF segments in input".into());
    }
    Ok(segs)
}

/// Extract sample `n` (0-indexed) of the first video stream,
/// counting across every `LIST movi` in every `RIFF AVI ` /
/// `RIFF AVIX` segment in file order.
///
/// Round 13 needs samples 1..N (P-frames referencing the
/// keyframe) so the decode driver can be re-run through the same
/// `hic` for sequential decoding. Round 16 extends this across
/// chained RIFF segments so OpenDML / AVI 2.0 fixtures work too.
///
/// Audio / palette / index chunks are skipped per the same
/// convention. Errors if the stream has fewer than `n+1` video
/// samples.
pub fn extract_video_sample(avi_bytes: &[u8], n: u32) -> Result<FirstSample, String> {
    let segments = walk_riff_segments(avi_bytes)?;

    // ---- hdrl + first vids strl: scan the first segment ----
    //
    // The OpenDML spec is explicit that the `LIST hdrl` lives
    // in the FIRST segment only. Subsequent `RIFF AVIX`
    // segments carry `LIST movi` bodies (and optional
    // `LIST INFO`s) — never an hdrl.
    let first = segments
        .first()
        .ok_or_else(|| "walk_riff_segments returned empty".to_string())?;
    if &first.form_type != b"AVI " {
        return Err(format!(
            "first RIFF form-type is not 'AVI ': {:?}",
            std::str::from_utf8(&first.form_type).unwrap_or("???"),
        ));
    }

    let mut hdrl: Option<&[u8]> = None;
    {
        let mut walker = ChunkWalker::new(first.body, first.body_file_off);
        while let Some(c) = walker.next()? {
            if c.kind != *b"LIST" || c.payload.len() < 4 {
                continue;
            }
            let sub: [u8; 4] = c.payload[0..4].try_into().unwrap();
            if &sub == b"hdrl" {
                hdrl = Some(&c.payload[4..]);
                break;
            }
        }
    }
    let hdrl = hdrl.ok_or_else(|| "no LIST hdrl in first AVI segment".to_string())?;

    // hdrl layout:
    //   avih: 56 bytes after the 8-byte chunk header.
    //   N × LIST strl (one per stream) — first vids stream wins.
    let (avih_w, avih_h) = parse_avih(hdrl)?;
    let (codec_fourcc, _bih_w, _bih_h) = find_first_vids_strl(hdrl)?;

    // ---- collect every LIST movi across every segment ----
    //
    // For AVI 1.0 there's exactly one. For OpenDML AVI 2.0
    // the first segment's movi holds the prefix samples and
    // each AVIX segment's movi holds a successive chunk of
    // samples (per "OpenDML AVI File Format Extensions" §3.2,
    // "Chunked AVI Files"). Sample 0 of stream 0 is always
    // the first stream-0 video chunk in the first movi.
    let mut movi_bodies: Vec<(&[u8], usize, usize)> = Vec::new(); // (body, file_off, seg_idx)
    for (seg_idx, seg) in segments.iter().enumerate() {
        // Only `AVI ` and `AVIX` carry sample chunks. Skip any
        // other form-type silently.
        if &seg.form_type != b"AVI " && &seg.form_type != b"AVIX" {
            continue;
        }
        let mut walker = ChunkWalker::new(seg.body, seg.body_file_off);
        while let Some(c) = walker.next()? {
            if c.kind != *b"LIST" || c.payload.len() < 4 {
                continue;
            }
            let sub: [u8; 4] = c.payload[0..4].try_into().unwrap();
            if &sub == b"movi" {
                movi_bodies.push((&c.payload[4..], c.payload_file_off + 4, seg_idx));
            }
        }
    }
    if movi_bodies.is_empty() {
        return Err("no LIST movi in any AVI segment".into());
    }

    // ---- walk every movi in order, count stream-0 video samples ----
    //
    // Match by the chunk FourCC's first byte == '0' / second
    // byte == '0' (stream-0). Skip 'wb' (audio) / 'pc'
    // (palette). Other 2cc values are accepted as compressed
    // video — some encoders write `00iv` instead of `00dc`,
    // and OpenDML's `ix00` / `ix01` standard-index chunks
    // start with 'i' / 'x' so they're naturally rejected by
    // the stream-index test (no leading '0').
    //
    // Round 17 — `LIST rec ` recursion. Microsoft's AVI 1.0
    // spec ("OpenDML AVI File Format Extensions" §1.4 + the
    // original AVI RIFF spec) allows a `movi` body to contain
    // `LIST rec ` blocks that group physically-adjacent sample
    // chunks (typically one video sample + the audio samples
    // covering its duration). Some encoders, including the
    // Indeo 4 reference encoder used to produce
    // `indeo41.avi`, ALWAYS wrap samples this way — so a
    // walker that doesn't recurse into `LIST rec ` finds zero
    // samples in those files. Treat `LIST rec ` as a
    // transparent container: every sample chunk inside is
    // surfaced at the same level as a flat-movi chunk.
    let mut seen: u32 = 0;
    for (movi_body, movi_file_off, _seg_idx) in &movi_bodies {
        if let Some(found) = find_stream0_video_sample(movi_body, *movi_file_off, n, &mut seen)? {
            return Ok(FirstSample {
                codec_fourcc,
                width: avih_w,
                height: avih_h,
                sample_offset: found.payload_file_off as u32,
                sample_size: found.payload.len() as u32,
                bytes: found.payload.to_vec(),
            });
        }
    }
    Err(format!(
        "stream 0 has fewer than {} video samples across {} LIST movi block(s) (saw {})",
        n + 1,
        movi_bodies.len(),
        seen,
    ))
}

/// Walk `body` (a `LIST movi` body OR a `LIST rec ` body) and
/// look for the `n`-th stream-0 video sample. Recurses into
/// `LIST rec ` blocks transparently. `seen` is mutated so the
/// caller can track progress across multiple movi bodies.
///
/// Returns `Ok(Some(chunk))` when sample `n` is located; the
/// chunk's `kind` / `payload` / `payload_file_off` fields are
/// already correct. Returns `Ok(None)` when this body is
/// fully walked without reaching sample `n` — the caller moves
/// on to the next movi body. Errors propagate from the
/// underlying [`ChunkWalker`].
fn find_stream0_video_sample<'a>(
    body: &'a [u8],
    body_file_off: usize,
    n: u32,
    seen: &mut u32,
) -> Result<Option<Chunk<'a>>, String> {
    let mut w = ChunkWalker::new(body, body_file_off);
    while let Some(c) = w.next()? {
        // `LIST rec ` recursion — Microsoft AVI 1.0 reference
        // §"Interleaved AVI files".
        if c.kind == *b"LIST" && c.payload.len() >= 4 && &c.payload[0..4] == b"rec " {
            if let Some(found) =
                find_stream0_video_sample(&c.payload[4..], c.payload_file_off + 4, n, seen)?
            {
                return Ok(Some(found));
            }
            continue;
        }
        if !is_stream_chunk(c.kind, 0) {
            continue;
        }
        let two_cc = [c.kind[2], c.kind[3]];
        if &two_cc == b"wb" || &two_cc == b"pc" {
            continue;
        }
        if *seen == n {
            return Ok(Some(c));
        }
        *seen += 1;
    }
    Ok(None)
}

/// Round-16 diagnostic helper: report how many `RIFF AVI ` /
/// `RIFF AVIX` segments the file contains and how many
/// `LIST movi` bodies the walker found (one per OpenDML
/// chunk). Returns `(segment_form_types, movi_count)` —
/// the test harness uses this to assert chained-RIFF
/// recognition without needing to peek at private state.
#[allow(dead_code)]
pub fn riff_segment_inventory(avi_bytes: &[u8]) -> Result<(Vec<[u8; 4]>, usize), String> {
    let segments = walk_riff_segments(avi_bytes)?;
    let forms: Vec<[u8; 4]> = segments.iter().map(|s| s.form_type).collect();
    let mut movi_count = 0usize;
    for seg in &segments {
        if &seg.form_type != b"AVI " && &seg.form_type != b"AVIX" {
            continue;
        }
        let mut walker = ChunkWalker::new(seg.body, seg.body_file_off);
        while let Some(c) = walker.next()? {
            if c.kind == *b"LIST" && c.payload.len() >= 4 && &c.payload[0..4] == b"movi" {
                movi_count += 1;
            }
        }
    }
    Ok((forms, movi_count))
}

/// Stream chunk FourCC predicate. The first two bytes are the
/// ASCII decimal stream index ('0','0' for stream 0). Reject
/// anything else.
fn is_stream_chunk(kind: [u8; 4], stream_idx: u8) -> bool {
    let want_b0 = b'0' + (stream_idx / 10);
    let want_b1 = b'0' + (stream_idx % 10);
    kind[0] == want_b0 && kind[1] == want_b1
}

/// Parse the AVI main-header (`avih`) inside `hdrl`. Layout
/// (Microsoft AVI 1.0 reference):
///
///   DWORD dwMicroSecPerFrame
///   DWORD dwMaxBytesPerSec
///   DWORD dwPaddingGranularity
///   DWORD dwFlags
///   DWORD dwTotalFrames
///   DWORD dwInitialFrames
///   DWORD dwStreams
///   DWORD dwSuggestedBufferSize
///   DWORD dwWidth      ← we want this
///   DWORD dwHeight     ← we want this
///   DWORD dwReserved[4]
fn parse_avih(hdrl: &[u8]) -> Result<(u32, u32), String> {
    // hdrl is the body of LIST hdrl, with chunks inside.
    let mut w = ChunkWalker::new(hdrl, 0); // file offsets in
                                           // hdrl are not used past avih, so anchor at 0.
    while let Some(c) = w.next()? {
        if c.kind == *b"avih" {
            if c.payload.len() < 40 {
                return Err(format!("avih too short: {} bytes", c.payload.len()));
            }
            let w_off = 8 * 4; // dwWidth at byte 32
            let h_off = 9 * 4; // dwHeight at byte 36
            let ww = u32::from_le_bytes(c.payload[w_off..w_off + 4].try_into().unwrap());
            let hh = u32::from_le_bytes(c.payload[h_off..h_off + 4].try_into().unwrap());
            return Ok((ww, hh));
        }
    }
    Err("no avih in hdrl".into())
}

/// Walk `LIST strl` blocks inside `hdrl`; return the first one
/// whose `strh.fccType == 'vids'`. From its `strf` parse the
/// `BITMAPINFOHEADER` and return `(biCompression, biWidth,
/// biHeight)`.
fn find_first_vids_strl(hdrl: &[u8]) -> Result<(u32, i32, i32), String> {
    let mut w = ChunkWalker::new(hdrl, 0);
    while let Some(c) = w.next()? {
        if c.kind != *b"LIST" || c.payload.len() < 4 {
            continue;
        }
        let sub: [u8; 4] = c.payload[0..4].try_into().unwrap();
        if sub != *b"strl" {
            continue;
        }
        let strl_body = &c.payload[4..];
        // strl: strh (stream header) + strf (stream format) + …
        let mut s = ChunkWalker::new(strl_body, 0);
        let mut is_video = false;
        let mut bih_compression = 0u32;
        let mut bih_w = 0i32;
        let mut bih_h = 0i32;
        let mut found_strh = false;
        let mut found_strf = false;
        while let Some(sc) = s.next()? {
            if sc.kind == *b"strh" {
                found_strh = true;
                if sc.payload.len() < 4 {
                    return Err("strh too short for fccType".into());
                }
                let fcc_type: [u8; 4] = sc.payload[0..4].try_into().unwrap();
                is_video = &fcc_type == b"vids";
            } else if sc.kind == *b"strf" {
                found_strf = true;
                if !is_video {
                    // Skip non-video format blocks.
                    continue;
                }
                // BITMAPINFOHEADER layout:
                //   DWORD biSize       (= 40 typically)
                //   LONG  biWidth
                //   LONG  biHeight
                //   WORD  biPlanes
                //   WORD  biBitCount
                //   DWORD biCompression  ← codec FourCC
                //   DWORD biSizeImage
                //   …
                if sc.payload.len() < 20 {
                    return Err(format!(
                        "video strf shorter than BITMAPINFOHEADER \
                         prefix: {} bytes",
                        sc.payload.len()
                    ));
                }
                bih_w = i32::from_le_bytes(sc.payload[4..8].try_into().unwrap());
                bih_h = i32::from_le_bytes(sc.payload[8..12].try_into().unwrap());
                bih_compression = u32::from_le_bytes(sc.payload[16..20].try_into().unwrap());
            }
            if found_strh && found_strf && is_video {
                return Ok((bih_compression, bih_w, bih_h));
            }
        }
    }
    Err("no LIST strl with fccType == 'vids' in hdrl".into())
}

/// One RIFF chunk: 4-byte FourCC + 4-byte LE size + payload.
/// `payload_file_off` is the byte offset, into the original
/// file, of the payload's first byte. Used so the caller can
/// report `sample_offset` as a real file offset.
#[derive(Debug, Clone, Copy)]
struct Chunk<'a> {
    kind: [u8; 4],
    payload: &'a [u8],
    payload_file_off: usize,
}

/// Iterator over a flat sequence of RIFF chunks. Each chunk is
/// padded to a 2-byte boundary per the RIFF spec; the walker
/// silently consumes the pad byte after odd-size payloads.
struct ChunkWalker<'a> {
    data: &'a [u8],
    pos: usize,
    /// File-offset of `data[0]`. Used to translate intra-`data`
    /// positions into file offsets for the caller.
    base_file_off: usize,
}

impl<'a> ChunkWalker<'a> {
    fn new(data: &'a [u8], base_file_off: usize) -> Self {
        Self {
            data,
            pos: 0,
            base_file_off,
        }
    }

    fn next(&mut self) -> Result<Option<Chunk<'a>>, String> {
        if self.pos >= self.data.len() {
            return Ok(None);
        }
        let remaining = &self.data[self.pos..];
        if remaining.len() < 8 {
            // Trailing partial bytes — treat as end-of-stream
            // rather than an error. Some files have a JUNK
            // padding tail that aligns the file size up to the
            // next 2 KiB boundary.
            return Ok(None);
        }
        let kind: [u8; 4] = remaining[0..4].try_into().unwrap();
        let size = u32::from_le_bytes(remaining[4..8].try_into().unwrap()) as usize;
        // Round 15 — `crashtest.avi` (a truncated 5 MiB head
        // of a 20 MiB AVI; commonly produced by capture-card
        // crash dumps) declares `LIST movi size=20353990`
        // larger than the bytes we actually have, so the
        // round-8 walker's strict `8 + size > remaining.len()`
        // check would skip the chunk entirely + bail out at
        // "no LIST movi". Real AVI parsers accept this case
        // by clamping the chunk's payload to the bytes that
        // remain and then walking the (necessarily truncated)
        // chunk body for as long as it parses cleanly. We
        // surface a truncation by clamping `size` to what's
        // available, so the inner walker can still find the
        // first few sample chunks.
        let avail = remaining.len() - 8;
        let clamped = size.min(avail);
        let payload = &remaining[8..8 + clamped];
        let payload_file_off = self.base_file_off + self.pos + 8;
        // Advance past header + payload + RIFF padding (chunks
        // align to 2-byte boundaries). When clamped, advance to
        // the end of the buffer so the next iteration returns
        // `Ok(None)`.
        let advance = if clamped < size {
            remaining.len()
        } else {
            8 + size + (size & 1)
        };
        self.pos = self
            .pos
            .checked_add(advance)
            .ok_or_else(|| "walker pos overflow".to_string())?;
        Ok(Some(Chunk {
            kind,
            payload,
            payload_file_off,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest legal AVI: RIFF + 'AVI ' + LIST hdrl (with
    /// avih + LIST strl with strh+strf) + LIST movi (with one
    /// '00dc' chunk holding 4 bytes 0xDE 0xAD 0xBE 0xEF).
    fn make_minimal_avi() -> Vec<u8> {
        // BITMAPINFOHEADER (40 bytes).
        let mut bih = Vec::new();
        bih.extend_from_slice(&40u32.to_le_bytes()); // biSize
        bih.extend_from_slice(&64i32.to_le_bytes()); // biWidth
        bih.extend_from_slice(&48i32.to_le_bytes()); // biHeight
        bih.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        bih.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
        bih.extend_from_slice(b"FAKE"); // biCompression
        bih.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
        bih.extend_from_slice(&0i32.to_le_bytes()); // biXPels
        bih.extend_from_slice(&0i32.to_le_bytes()); // biYPels
        bih.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
        bih.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

        // strh (AVISTREAMHEADER, ~56 bytes)
        let mut strh = Vec::new();
        strh.extend_from_slice(b"vids"); // fccType
        strh.extend_from_slice(b"FAKE"); // fccHandler
        strh.extend_from_slice(&[0u8; 56 - 8]); // pad to 56 bytes

        // strl: strh + strf
        let mut strl = Vec::new();
        strl.extend_from_slice(b"strl"); // sub-form
        push_chunk(&mut strl, b"strh", &strh);
        push_chunk(&mut strl, b"strf", &bih);

        // avih (56 bytes)
        let mut avih = vec![0u8; 56];
        avih[32..36].copy_from_slice(&64u32.to_le_bytes()); // dwWidth
        avih[36..40].copy_from_slice(&48u32.to_le_bytes()); // dwHeight

        // hdrl: avih + LIST strl
        let mut hdrl = Vec::new();
        hdrl.extend_from_slice(b"hdrl"); // sub-form
        push_chunk(&mut hdrl, b"avih", &avih);
        push_chunk(&mut hdrl, b"LIST", &strl);

        // movi: 00dc + 4-byte payload
        let mut movi = Vec::new();
        movi.extend_from_slice(b"movi"); // sub-form
        push_chunk(&mut movi, b"00dc", &[0xDE, 0xAD, 0xBE, 0xEF]);

        let mut body = Vec::new();
        body.extend_from_slice(b"AVI "); // form-type
        push_chunk(&mut body, b"LIST", &hdrl);
        push_chunk(&mut body, b"LIST", &movi);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((body.len()) as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn push_chunk(buf: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
        buf.extend_from_slice(kind);
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(payload);
        // 2-byte pad if odd size.
        if payload.len() & 1 == 1 {
            buf.push(0);
        }
    }

    #[test]
    fn minimal_avi_decodes_to_synthetic_first_sample() {
        let avi = make_minimal_avi();
        let s = extract_first_video_sample(&avi).expect("walker");
        assert_eq!(s.codec_fourcc, u32::from_le_bytes(*b"FAKE"));
        assert_eq!(s.width, 64);
        assert_eq!(s.height, 48);
        assert_eq!(s.bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(s.sample_size, 4);
    }

    #[test]
    fn rejects_non_riff_input() {
        let mut bad = vec![0u8; 32];
        bad[0..4].copy_from_slice(b"WAVE");
        let err = extract_first_video_sample(&bad).unwrap_err();
        assert!(err.contains("not a RIFF"));
    }

    #[test]
    fn rejects_riff_non_avi() {
        let mut bad = b"RIFF\x10\x00\x00\x00WAVEjunk\x00\x00\x00\x00".to_vec();
        bad.resize(32, 0);
        let err = extract_first_video_sample(&bad).unwrap_err();
        assert!(err.contains("RIFF form-type"));
    }

    #[test]
    fn is_stream_chunk_matches_two_digit_indices() {
        assert!(is_stream_chunk(*b"00dc", 0));
        assert!(is_stream_chunk(*b"00iv", 0));
        assert!(is_stream_chunk(*b"01wb", 1));
        assert!(!is_stream_chunk(*b"01dc", 0));
    }

    /// Build a synthetic OpenDML AVI 2.0 file: `RIFF AVI ` with
    /// hdrl + `LIST movi` containing one sample, followed by a
    /// chained `RIFF AVIX` carrying a second `LIST movi` with
    /// two more samples. The walker should surface all three
    /// samples in file order.
    fn make_opendml_avi() -> Vec<u8> {
        // BITMAPINFOHEADER (40 bytes) — biCompression=`OPDM`.
        let mut bih = Vec::new();
        bih.extend_from_slice(&40u32.to_le_bytes());
        bih.extend_from_slice(&64i32.to_le_bytes());
        bih.extend_from_slice(&48i32.to_le_bytes());
        bih.extend_from_slice(&1u16.to_le_bytes());
        bih.extend_from_slice(&24u16.to_le_bytes());
        bih.extend_from_slice(b"OPDM");
        bih.extend_from_slice(&0u32.to_le_bytes());
        bih.extend_from_slice(&0i32.to_le_bytes());
        bih.extend_from_slice(&0i32.to_le_bytes());
        bih.extend_from_slice(&0u32.to_le_bytes());
        bih.extend_from_slice(&0u32.to_le_bytes());

        // strh (AVISTREAMHEADER, ~56 bytes).
        let mut strh = Vec::new();
        strh.extend_from_slice(b"vids");
        strh.extend_from_slice(b"OPDM");
        strh.extend_from_slice(&[0u8; 56 - 8]);

        // strl: strh + strf + a synthetic `indx` super-index
        // chunk (16 bytes — just enough payload to round-trip
        // the walker; the contents aren't read).
        let mut strl = Vec::new();
        strl.extend_from_slice(b"strl");
        push_chunk(&mut strl, b"strh", &strh);
        push_chunk(&mut strl, b"strf", &bih);
        push_chunk(&mut strl, b"indx", &[0u8; 16]);

        // avih (56 bytes).
        let mut avih = vec![0u8; 56];
        avih[32..36].copy_from_slice(&64u32.to_le_bytes());
        avih[36..40].copy_from_slice(&48u32.to_le_bytes());

        // hdrl: avih + LIST strl + a `LIST odml` (dmlh) tail
        // typical of OpenDML files (we ignore it but its
        // presence shouldn't sink the walker).
        let mut odml = Vec::new();
        odml.extend_from_slice(b"odml");
        push_chunk(&mut odml, b"dmlh", &[0u8; 4]);

        let mut hdrl = Vec::new();
        hdrl.extend_from_slice(b"hdrl");
        push_chunk(&mut hdrl, b"avih", &avih);
        push_chunk(&mut hdrl, b"LIST", &strl);
        push_chunk(&mut hdrl, b"LIST", &odml);

        // First segment movi: one sample (index 0) + one
        // `ix00` super-index that the walker should skip.
        let mut movi1 = Vec::new();
        movi1.extend_from_slice(b"movi");
        push_chunk(&mut movi1, b"ix00", &[0u8; 8]);
        push_chunk(&mut movi1, b"00dc", b"AAAA");

        let mut body1 = Vec::new();
        body1.extend_from_slice(b"AVI ");
        push_chunk(&mut body1, b"LIST", &hdrl);
        push_chunk(&mut body1, b"LIST", &movi1);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((body1.len()) as u32).to_le_bytes());
        out.extend_from_slice(&body1);

        // Chained RIFF AVIX segment: holds movi only.
        let mut movi2 = Vec::new();
        movi2.extend_from_slice(b"movi");
        push_chunk(&mut movi2, b"00dc", b"BBBB");
        push_chunk(&mut movi2, b"00dc", b"CCCC");

        let mut body2 = Vec::new();
        body2.extend_from_slice(b"AVIX");
        push_chunk(&mut body2, b"LIST", &movi2);

        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((body2.len()) as u32).to_le_bytes());
        out.extend_from_slice(&body2);

        out
    }

    #[test]
    fn opendml_avi_walks_chained_riff_segments() {
        let avi = make_opendml_avi();
        let (forms, movi_count) = riff_segment_inventory(&avi).expect("inventory");
        assert_eq!(forms.len(), 2, "expected exactly 2 RIFF segments");
        assert_eq!(&forms[0], b"AVI ", "first segment must be AVI ");
        assert_eq!(&forms[1], b"AVIX", "second segment must be AVIX");
        assert_eq!(movi_count, 2, "one LIST movi per RIFF segment");

        let s0 = extract_video_sample(&avi, 0).expect("sample 0");
        assert_eq!(s0.bytes, b"AAAA", "sample 0 from first segment movi");
        assert_eq!(s0.codec_fourcc, u32::from_le_bytes(*b"OPDM"));
        assert_eq!(s0.width, 64);
        assert_eq!(s0.height, 48);

        let s1 = extract_video_sample(&avi, 1).expect("sample 1");
        assert_eq!(s1.bytes, b"BBBB", "sample 1 from chained AVIX movi");

        let s2 = extract_video_sample(&avi, 2).expect("sample 2");
        assert_eq!(s2.bytes, b"CCCC", "sample 2 also from chained AVIX movi");

        // The walker correctly errors when asked for a sample
        // beyond the chained corpus.
        let err = extract_video_sample(&avi, 3).unwrap_err();
        assert!(
            err.contains("fewer than 4"),
            "expected 'fewer than 4' diagnostic; got: {err}",
        );
    }

    /// AVI 2.0 super-index `indx` lives in `strl`; a plain
    /// stream-0 index chunk `ix00` lives in `movi`. Both must
    /// be transparently skipped by the walker so the first
    /// stream-0 sample chunk is sample 0.
    #[test]
    fn opendml_walker_skips_indx_and_ix_chunks() {
        let avi = make_opendml_avi();
        // The synthetic file's first movi is `ix00` (8 bytes)
        // followed by `00dc AAAA`. If the walker did not skip
        // `ix00` it would either trip on the leading 'i' or
        // mis-count it as sample 0. The previous test asserts
        // sample 0 == b"AAAA"; here we additionally confirm
        // the walker doesn't error trying to scan the `ix00`
        // payload as if it were a sample chunk header.
        let s0 = extract_first_video_sample(&avi).expect("first sample");
        assert_eq!(s0.bytes, b"AAAA");
    }

    /// Round-17 — `LIST rec ` recursion. Build a minimal AVI
    /// 1.0 file whose `LIST movi` body wraps the sample chunks
    /// inside two `LIST rec ` blocks (the interleaved-AVI shape
    /// from Microsoft's AVI 1.0 reference §"Interleaved AVI
    /// files"). The walker must descend into each `LIST rec `
    /// transparently, surfacing the inner sample chunks at the
    /// same depth as flat-movi chunks.
    fn make_interleaved_avi() -> Vec<u8> {
        let mut bih = Vec::new();
        bih.extend_from_slice(&40u32.to_le_bytes());
        bih.extend_from_slice(&64i32.to_le_bytes());
        bih.extend_from_slice(&48i32.to_le_bytes());
        bih.extend_from_slice(&1u16.to_le_bytes());
        bih.extend_from_slice(&24u16.to_le_bytes());
        bih.extend_from_slice(b"RECx");
        bih.extend_from_slice(&[0u8; 5 * 4]);

        let mut strh = Vec::new();
        strh.extend_from_slice(b"vids");
        strh.extend_from_slice(b"RECx");
        strh.extend_from_slice(&[0u8; 56 - 8]);

        let mut strl = Vec::new();
        strl.extend_from_slice(b"strl");
        push_chunk(&mut strl, b"strh", &strh);
        push_chunk(&mut strl, b"strf", &bih);

        let mut avih = vec![0u8; 56];
        avih[32..36].copy_from_slice(&64u32.to_le_bytes());
        avih[36..40].copy_from_slice(&48u32.to_le_bytes());

        let mut hdrl = Vec::new();
        hdrl.extend_from_slice(b"hdrl");
        push_chunk(&mut hdrl, b"avih", &avih);
        push_chunk(&mut hdrl, b"LIST", &strl);

        // Two `LIST rec ` blocks, each carrying one stream-0
        // video sample plus a stream-1 audio chunk that the
        // walker should skip.
        let mut rec1 = Vec::new();
        rec1.extend_from_slice(b"rec ");
        push_chunk(&mut rec1, b"00dc", b"AAAA");
        push_chunk(&mut rec1, b"01wb", b"audio1");

        let mut rec2 = Vec::new();
        rec2.extend_from_slice(b"rec ");
        push_chunk(&mut rec2, b"00dc", b"BBBB");
        push_chunk(&mut rec2, b"01wb", b"audio2");

        let mut movi = Vec::new();
        movi.extend_from_slice(b"movi");
        push_chunk(&mut movi, b"LIST", &rec1);
        push_chunk(&mut movi, b"LIST", &rec2);

        let mut body = Vec::new();
        body.extend_from_slice(b"AVI ");
        push_chunk(&mut body, b"LIST", &hdrl);
        push_chunk(&mut body, b"LIST", &movi);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((body.len()) as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn interleaved_avi_walker_descends_list_rec() {
        let avi = make_interleaved_avi();
        let s0 = extract_video_sample(&avi, 0).expect("sample 0 from rec1");
        assert_eq!(s0.bytes, b"AAAA");
        assert_eq!(s0.codec_fourcc, u32::from_le_bytes(*b"RECx"));
        let s1 = extract_video_sample(&avi, 1).expect("sample 1 from rec2");
        assert_eq!(s1.bytes, b"BBBB");
        // The walker must NOT surface `01wb` (audio) chunks as
        // video samples — sample 2 should not exist.
        let err = extract_video_sample(&avi, 2).unwrap_err();
        assert!(
            err.contains("fewer than 3"),
            "expected 'fewer than 3' diagnostic; got: {err}",
        );
    }
}
