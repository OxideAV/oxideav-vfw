//! Test-side QuickTime/ISO BMFF chunk walker.
//!
//! Round 7 needs to feed Intel's `IR32_32.DLL` (Indeo 3) a real
//! IV31 keyframe extracted from `cubes.mov`. The fixture corpus
//! has no separate "first-frame" file, so we parse just enough
//! of the QuickTime container to locate sample 0 and hand back
//! its bytes. This lives in `tests/` so the production crate
//! gets no MOV parser surface.
//!
//! Reference: ISO/IEC 14496-12 (ISO BMFF) §4 (box structure),
//! §8.16 (sample-table boxes — `stsd`, `stco`, `stsz`, `stsc`).
//! QuickTime `.mov` extends BMFF with the same box graph and
//! the same `moov → trak → mdia → minf → stbl` path.
//!
//! This module is intentionally narrow:
//!
//! * Only the boxes round 7 needs are recognised; everything
//!   else is skipped over by size.
//! * Only the FIRST video track is followed.
//! * Only single-sample-per-chunk layouts are supported (the
//!   `cubes.mov` fixture matches; round 8 may need to lift this
//!   when a per-chunk layout shows up).
//! * No attempt at codec-specific framing — we just extract the
//!   sample bytes and let the caller parse the codec payload.
//!
//! NEVER reference `libavformat/mov.c`. The implementation
//! was authored from ISO/IEC 14496-12 alone.

use std::convert::TryInto;

/// Metadata + payload for the first video sample in a MOV file.
#[derive(Debug, Clone)]
pub struct FirstSample {
    /// Codec FourCC (LE u32 of the 4 ASCII bytes), e.g. `IV32`
    /// reads as `u32::from_le_bytes(*b"IV32")`.
    pub codec_fourcc: u32,
    /// Coded width from the `stsd` Visual Sample Entry.
    pub width: u16,
    /// Coded height from the `stsd` Visual Sample Entry.
    pub height: u16,
    /// File offset of sample 0's bytes (from `stco[0]`).
    pub sample_offset: u32,
    /// Byte length of sample 0 (from `stsz[0]` or `stsz`'s
    /// constant `sample_size`).
    pub sample_size: u32,
    /// The sample's bytes, length == `sample_size`.
    pub bytes: Vec<u8>,
}

/// Top-level entry: parse `mov_bytes`, find the first video
/// track, return the first sample.
pub fn extract_first_video_sample(mov_bytes: &[u8]) -> Result<FirstSample, String> {
    // Walk top-level boxes; we expect to encounter `mdat`
    // (sample-data heap) and `moov` (metadata graph). Order
    // varies — `cubes.mov` puts `mdat` first.
    let mut moov: Option<&[u8]> = None;
    let mut walker = TopBoxWalker::new(mov_bytes);
    while let Some(b) = walker.next()? {
        if &b.kind == b"moov" {
            moov = Some(b.payload);
        }
        // `ftyp` is optional in QuickTime; we don't validate
        // it. ISO BMFF requires it. Other top-level boxes
        // (`mdat`, `free`, `wide`) are skipped over by size.
    }
    let moov = moov.ok_or_else(|| "no `moov` box at top level".to_string())?;

    // Walk moov children. We want the first `trak` whose
    // hdlr.handler_type == 'vide'. Bail when found.
    let trak = find_video_trak(moov)?;
    let stbl = find_path(trak, &[*b"mdia", *b"minf", *b"stbl"])?
        .ok_or_else(|| "no stbl under trak/mdia/minf".to_string())?;

    let stsd = find_child(stbl, *b"stsd")?.ok_or_else(|| "no stsd in stbl".to_string())?;
    let (codec_fourcc, width, height) = parse_stsd_visual(stsd)?;

    let stco = find_child(stbl, *b"stco")?.ok_or_else(|| "no stco in stbl".to_string())?;
    let stsz = find_child(stbl, *b"stsz")?.ok_or_else(|| "no stsz in stbl".to_string())?;

    // First chunk offset.
    let first_chunk_off = parse_stco_first(stco)?;
    // First sample size.
    let first_sample_size = parse_stsz_first(stsz)?;

    // We only support the "one-sample-per-chunk" layout — sample
    // 0 = chunk 0's first byte, length = stsz[0].
    let sample_offset = first_chunk_off;
    let sample_size = first_sample_size;
    let start = sample_offset as usize;
    let end = start
        .checked_add(sample_size as usize)
        .ok_or_else(|| "sample offset+size overflows".to_string())?;
    if end > mov_bytes.len() {
        return Err(format!(
            "sample slice {start}..{end} exceeds file size {}",
            mov_bytes.len()
        ));
    }
    let bytes = mov_bytes[start..end].to_vec();
    Ok(FirstSample {
        codec_fourcc,
        width,
        height,
        sample_offset,
        sample_size,
        bytes,
    })
}

/// One BMFF box: 4-byte size + 4-byte FourCC kind + payload.
#[derive(Debug, Clone, Copy)]
struct Box<'a> {
    kind: [u8; 4],
    payload: &'a [u8],
}

/// Iterator over a flat sequence of BMFF boxes (`mov_bytes` for
/// top level; a parent's payload for children).
struct TopBoxWalker<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> TopBoxWalker<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn next(&mut self) -> Result<Option<Box<'a>>, String> {
        if self.pos >= self.data.len() {
            return Ok(None);
        }
        let remaining = &self.data[self.pos..];
        if remaining.len() < 8 {
            return Err(format!(
                "truncated box header at pos {} ({} bytes left)",
                self.pos,
                remaining.len()
            ));
        }
        let raw_size = u32::from_be_bytes(remaining[0..4].try_into().unwrap());
        let kind: [u8; 4] = remaining[4..8].try_into().unwrap();
        // Two extension forms per ISO/IEC 14496-12 §4.2:
        //   size == 1 → 64-bit size at bytes 8..16.
        //   size == 0 → "to end of file".
        let (size_bytes, header_bytes) = if raw_size == 1 {
            if remaining.len() < 16 {
                return Err("truncated 64-bit size".into());
            }
            let large = u64::from_be_bytes(remaining[8..16].try_into().unwrap());
            (large, 16)
        } else if raw_size == 0 {
            (remaining.len() as u64, 8)
        } else {
            (raw_size as u64, 8)
        };
        if size_bytes < header_bytes as u64 {
            return Err(format!(
                "box size {size_bytes} smaller than header {header_bytes} at pos {}",
                self.pos
            ));
        }
        let total = size_bytes as usize;
        if total > remaining.len() {
            return Err(format!(
                "box at pos {} declares size {total} but only {} bytes remain",
                self.pos,
                remaining.len()
            ));
        }
        let payload = &remaining[header_bytes..total];
        self.pos += total;
        Ok(Some(Box { kind, payload }))
    }
}

/// Find the first immediate child with the given FourCC.
fn find_child(parent: &[u8], kind: [u8; 4]) -> Result<Option<&[u8]>, String> {
    let mut walker = TopBoxWalker::new(parent);
    while let Some(b) = walker.next()? {
        if b.kind == kind {
            return Ok(Some(b.payload));
        }
    }
    Ok(None)
}

/// Walk a chain of FourCCs from `start`, returning the last
/// payload. Returns `Ok(None)` at the first miss.
fn find_path<'a>(start: &'a [u8], path: &[[u8; 4]]) -> Result<Option<&'a [u8]>, String> {
    let mut cur = start;
    for kind in path {
        match find_child(cur, *kind)? {
            Some(p) => cur = p,
            None => return Ok(None),
        }
    }
    Ok(Some(cur))
}

/// Walk `moov` and return the first `trak` whose nested
/// `mdia/hdlr.handler_type == 'vide'`.
fn find_video_trak(moov: &[u8]) -> Result<&[u8], String> {
    let mut w = TopBoxWalker::new(moov);
    while let Some(b) = w.next()? {
        if b.kind == *b"trak" {
            // Look at trak/mdia/hdlr.
            if let Some(hdlr) = find_path(b.payload, &[*b"mdia", *b"hdlr"])? {
                // hdlr layout (ISO/IEC 14496-12 §8.4.3):
                //   1B version + 3B flags
                //   4B pre_defined (= 0)
                //   4B handler_type
                //   12B reserved
                //   ASCIIZ name
                if hdlr.len() >= 12 {
                    let handler_type: [u8; 4] = hdlr[8..12].try_into().unwrap();
                    if &handler_type == b"vide" {
                        return Ok(b.payload);
                    }
                }
            }
        }
    }
    Err("no video trak under moov".into())
}

/// Parse the first Visual Sample Entry inside `stsd`.
///
/// stsd layout (ISO/IEC 14496-12 §8.5.2):
///   1B version + 3B flags
///   4B entry_count
///   N × SampleEntry
///
/// Visual Sample Entry layout (§12.1.3 + QuickTime extensions):
///   8B box header (size + 4-byte codec FourCC)
///   6B reserved (= 0)
///   2B data_reference_index
///   16B reserved (QuickTime predefined, = 0 in ISO BMFF)
///   2B width
///   2B height
///   ... rest unused for round 7.
fn parse_stsd_visual(stsd: &[u8]) -> Result<(u32, u16, u16), String> {
    if stsd.len() < 8 {
        return Err("stsd too short".into());
    }
    // Skip 4-byte version+flags + 4-byte entry_count.
    let entries = &stsd[8..];
    let mut w = TopBoxWalker::new(entries);
    let entry = w.next()?.ok_or_else(|| "no entry in stsd".to_string())?;
    let codec_fourcc = u32::from_le_bytes(entry.kind);
    // Visual Sample Entry layout starts at payload byte 0:
    //   6B reserved + 2B data_reference_index = 8B
    //   16B QT predefined / version block
    //   2B width @ +24, 2B height @ +26.
    if entry.payload.len() < 28 {
        return Err(format!(
            "Visual Sample Entry too short: {} bytes (need 28)",
            entry.payload.len()
        ));
    }
    let width = u16::from_be_bytes(entry.payload[24..26].try_into().unwrap());
    let height = u16::from_be_bytes(entry.payload[26..28].try_into().unwrap());
    Ok((codec_fourcc, width, height))
}

/// `stco` (32-bit chunk-offset table) — return `entries[0]`.
///
/// Layout (ISO/IEC 14496-12 §8.7.5):
///   1B version + 3B flags
///   4B entry_count
///   entry_count × 4B chunk_offset (BE).
fn parse_stco_first(stco: &[u8]) -> Result<u32, String> {
    if stco.len() < 12 {
        return Err("stco too short".into());
    }
    let count = u32::from_be_bytes(stco[4..8].try_into().unwrap());
    if count == 0 {
        return Err("stco has zero entries".into());
    }
    Ok(u32::from_be_bytes(stco[8..12].try_into().unwrap()))
}

/// `stsz` (sample-size table) — return the first sample's
/// size in bytes.
///
/// Layout (ISO/IEC 14496-12 §8.7.3):
///   1B version + 3B flags
///   4B sample_size  (constant, or 0 = per-sample table)
///   4B sample_count
///   if sample_size == 0:
///     sample_count × 4B per_sample_size (BE)
fn parse_stsz_first(stsz: &[u8]) -> Result<u32, String> {
    if stsz.len() < 12 {
        return Err("stsz too short".into());
    }
    let constant_size = u32::from_be_bytes(stsz[4..8].try_into().unwrap());
    if constant_size != 0 {
        return Ok(constant_size);
    }
    let count = u32::from_be_bytes(stsz[8..12].try_into().unwrap());
    if count == 0 {
        return Err("stsz has zero entries".into());
    }
    if stsz.len() < 16 {
        return Err("stsz table truncated before first sample size".into());
    }
    Ok(u32::from_be_bytes(stsz[12..16].try_into().unwrap()))
}
