//! Test-side RIFF/AVI 1.0 chunk walker.
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
//! Multimedia SDK ("AVI RIFF File Reference"). Both describe the
//! container as a chunk-tagged byte stream:
//!
//! * Top-level: `RIFF` form, type `AVI `, body = a list of
//!   chunks.
//! * `LIST` chunks have a 4-byte sub-form FourCC followed by
//!   nested chunks. For `LIST hdrl` the body holds `avih` +
//!   per-stream `LIST strl`. For `LIST strl` the body holds
//!   `strh` + `strf` + (`strn` / `strd` / `JUNK`).
//! * `LIST movi` holds the actual sample chunks named
//!   `<2-digit stream index><2-byte 4cc>`. The two-byte 4cc
//!   varies — `dc` is the canonical "compressed video" tag,
//!   but some encoders write the codec letters directly
//!   (e.g. `iv` for Indeo video). We match by the leading
//!   stream-index digits + ignore the trailing 2 bytes.
//!
//! NEVER reference `libavformat/avi*.c`. The implementation
//! was authored from the public RIFF + AVI 1.0 documentation
//! alone.

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

/// Extract sample `n` (0-indexed) of the first video stream
/// inside `LIST movi`. Round 13 needs samples 1..N (P-frames
/// referencing the keyframe) so the decode driver can be re-run
/// through the same `hic` for sequential decoding.
///
/// Audio / palette chunks are skipped per the same convention as
/// `extract_first_video_sample`. Errors if the stream has fewer
/// than `n+1` video samples.
pub fn extract_video_sample(avi_bytes: &[u8], n: u32) -> Result<FirstSample, String> {
    // Top-level RIFF chunk: 'RIFF' + 4-byte size + 4-byte form
    // type ('AVI ') + body.
    if avi_bytes.len() < 12 {
        return Err("file shorter than RIFF header".into());
    }
    if &avi_bytes[0..4] != b"RIFF" {
        return Err(format!(
            "not a RIFF file: leading bytes {:02x?}",
            &avi_bytes[0..4]
        ));
    }
    let riff_size = u32::from_le_bytes(avi_bytes[4..8].try_into().unwrap()) as usize;
    if &avi_bytes[8..12] != b"AVI " {
        return Err(format!(
            "RIFF form-type is not AVI: {:?}",
            std::str::from_utf8(&avi_bytes[8..12]).unwrap_or("???")
        ));
    }
    // The RIFF payload spans bytes 8.. (riff_size + 8). Body
    // starts at byte 12 (skipping the 4-byte form-type).
    let body_end = 8usize
        .checked_add(riff_size)
        .ok_or_else(|| "RIFF size overflows".to_string())?;
    let body_end = body_end.min(avi_bytes.len());
    if body_end < 12 {
        return Err("RIFF body region empty".into());
    }
    let body = &avi_bytes[12..body_end];
    let body_file_off: usize = 12;

    // Walk top-level chunks within the AVI body. We expect:
    //   LIST hdrl  → header metadata.
    //   LIST INFO  → optional comments (skip).
    //   JUNK       → padding (skip).
    //   LIST movi  → the sample chunks.
    //   idx1       → optional index (we ignore — chunks are
    //                self-describing inside movi).
    let mut hdrl: Option<&[u8]> = None;
    // movi tracks the file offset of LIST's own *body* — i.e.
    // 4 bytes past the LIST header's sub-form FourCC.
    let mut movi: Option<(&[u8], usize)> = None;
    let mut walker = ChunkWalker::new(body, body_file_off);
    while let Some(c) = walker.next()? {
        if c.kind == *b"LIST" {
            // First 4 bytes of payload = sub-form FourCC.
            if c.payload.len() < 4 {
                continue;
            }
            let sub: [u8; 4] = c.payload[0..4].try_into().unwrap();
            let inner = &c.payload[4..];
            // Body starts 4 bytes past the LIST payload's start.
            let inner_file_off = c.payload_file_off + 4;
            match &sub {
                b"hdrl" => hdrl = Some(inner),
                b"movi" => movi = Some((inner, inner_file_off)),
                _ => {}
            }
        }
    }

    let hdrl = hdrl.ok_or_else(|| "no LIST hdrl in AVI body".to_string())?;
    let (movi, movi_file_off) = movi.ok_or_else(|| "no LIST movi in AVI body".to_string())?;

    // ---- hdrl: pick avih + first vids strl --------------------
    //
    // hdrl layout:
    //   avih: 56 bytes after the 8-byte chunk header.
    //   N × LIST strl (one per stream) — first vids stream wins.
    let (avih_w, avih_h) = parse_avih(hdrl)?;

    // First strl with stream-handler == 'vids'.
    let (codec_fourcc, _bih_w, _bih_h) = find_first_vids_strl(hdrl)?;

    // ---- movi: first chunk for stream 0 -----------------------
    //
    // The chunk FourCC is `<stream-index><2-letter 2cc>`. We
    // match on the leading two ASCII digits == '00' (stream 0).
    // Common 2cc tail values:
    //   'dc' — compressed video (canonical).
    //   'db' — uncompressed video.
    //   'wb' — audio chunk (skip).
    //   'pc' — palette change (skip).
    //   '<codec letters>' — some encoders write 'iv', 'cv', etc.
    //
    // We accept anything that is NOT a known audio / palette tag
    // for stream 0, so this walker is robust across encoders.
    let mut w = ChunkWalker::new(movi, movi_file_off);
    let mut seen: u32 = 0;
    while let Some(c) = w.next()? {
        if !is_stream_chunk(c.kind, 0) {
            continue;
        }
        let two_cc = [c.kind[2], c.kind[3]];
        // Skip audio / palette chunks for stream 0 (rare in a
        // pure-video AVI, but we honour the convention so the
        // walker stays correct on multi-stream files).
        if &two_cc == b"wb" || &two_cc == b"pc" {
            continue;
        }
        if seen == n {
            return Ok(FirstSample {
                codec_fourcc,
                width: avih_w,
                height: avih_h,
                sample_offset: c.payload_file_off as u32,
                sample_size: c.payload.len() as u32,
                bytes: c.payload.to_vec(),
            });
        }
        seen += 1;
    }
    Err(format!(
        "stream 0 has fewer than {} video samples in LIST movi (saw {})",
        n + 1,
        seen
    ))
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
}
