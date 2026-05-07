//! `vfw32.dll` Installable-Compressor (`IC*`) stub surface.
//!
//! Each `IC*` entry point — the API the host side of the sandbox
//! drives the codec DLL with — boils down to "package the
//! arguments into the message-specific structure and dispatch to
//! `DriverProc`". This module owns:
//!
//! * The stdcall `DriverProc` guest invocation.
//! * The `BITMAPINFOHEADER` / `ICINFO` / `ICDECOMPRESS` /
//!   `ICDECOMPRESSEX` marshalling helpers.
//! * `IC*` host-side wrappers ([`ic_open`], [`ic_close`],
//!   [`ic_get_info`], [`ic_decompress_query`],
//!   [`ic_decompress_begin`], [`ic_decompress`],
//!   [`ic_decompress_end`]).
//!
//! These wrappers are what the round-2 end-to-end test calls.
//! Because no caller DLL imports `vfw32!IC*` into the sandbox
//! today (we're driving it from outside, not from another guest
//! codec), there is no IAT thunk to register; the wrappers are
//! plain Rust functions that take a `&mut Sandbox`-shaped tuple
//! and synchronously invoke `DriverProc` until it returns.
//!
//! Reference: MSDN "Installable Compressor / IC functions"
//! (`docs.microsoft.com/en-us/windows/win32/api/vfw/`),
//! `winddi.h` / `mmsystem.h` / `vfw.h` constant definitions
//! transcribed below.
//!
//! All structures here are POD with explicit field-by-field
//! marshalling; no use of `#[repr(C)]` types crossing the host /
//! guest boundary, by design — the entire crate is
//! `#![forbid(unsafe_code)]`.

use super::{call_guest, HicEntry, HostState, Registry, Win32Error};
use crate::emulator::{Cpu, Mmu};

// --- Constants — vfw.h transcriptions --------------------------------

/// `mmsystem.h`: Driver-proc message — the driver was just opened.
pub const DRV_OPEN: u32 = 0x0003;
/// `mmsystem.h`: Driver-proc message — the driver is being closed.
pub const DRV_CLOSE: u32 = 0x0004;

/// vfw.h: `ICM_USER = DRV_USER + 0x0000 = 0x4000`. Start of the
/// IC message space.
pub const ICM_USER: u32 = 0x4000;

/// vfw.h: `ICM_RESERVED_LOW = DRV_USER + 0x1000 = 0x5000`. Used
/// as the base for `ICM_GETINFO` etc.
pub const ICM_RESERVED: u32 = 0x5000;

// Authoritative `ICM_*` numeric values (vfw.h, Windows 10 SDK):
//   ICM_GETINFO                  = ICM_RESERVED + 2   (0x5002)
//   ICM_DECOMPRESS_GET_FORMAT    = ICM_USER + 10      (0x400A)
//   ICM_DECOMPRESS_QUERY         = ICM_USER + 11      (0x400B)
//   ICM_DECOMPRESS_BEGIN         = ICM_USER + 12      (0x400C)
//   ICM_DECOMPRESS               = ICM_USER + 13      (0x400D)
//   ICM_DECOMPRESS_END           = ICM_USER + 14      (0x400E)
//   ICM_DECOMPRESS_SET_PALETTE   = ICM_USER + 29      (0x401D)
//   ICM_DECOMPRESS_GET_PALETTE   = ICM_USER + 30      (0x401E)
//
// Round-4's table was wrong (used `ICM_USER + 0/0x29/0x2A/0x2B/0x2C`)
// — round-5 corrected QUERY/END/DECOMPRESS but kept BEGIN at the wrong
// value (ICM_USER + 16 = 0x4010). Round-7 fixes BEGIN to ICM_USER + 12
// after disassembling IR32_32.DLL's dispatch table at 0x10001760: the
// real BEGIN handler at 0x10001339 calls 0x10002a30 which sets up the
// per-instance state2 buffer (`inc [state2_ptr]`), without which
// ICM_DECOMPRESS bails immediately at the `cmp [state2_ptr], 0`
// validation in 0x10002b30 (returns ICERR_BADIMAGE = 0xFFFFFF9C).
//
// And GET_FORMAT was at +8 (0x4008) — round-7 fixes to +10 (0x400A).

/// `vfw.h`: `ICM_GETINFO` (request the codec's identity card).
pub const ICM_GETINFO: u32 = ICM_RESERVED + 2;
/// `vfw.h`: `ICM_DECOMPRESS_GET_FORMAT`.
pub const ICM_DECOMPRESS_GET_FORMAT: u32 = ICM_USER + 10;
/// `vfw.h`: `ICM_DECOMPRESS_QUERY` — can we decompress this format?
pub const ICM_DECOMPRESS_QUERY: u32 = ICM_USER + 11;
/// `vfw.h`: `ICM_DECOMPRESS_BEGIN`.
pub const ICM_DECOMPRESS_BEGIN: u32 = ICM_USER + 12;
/// `vfw.h`: `ICM_DECOMPRESS`.
pub const ICM_DECOMPRESS: u32 = ICM_USER + 13;
/// `vfw.h`: `ICM_DECOMPRESS_END`.
pub const ICM_DECOMPRESS_END: u32 = ICM_USER + 14;

// vfw.h: ICDECOMPRESS dwFlags
/// "This is a key/intra frame" — set on the first frame and on
/// any frame the bitstream marks as a keyframe.
pub const ICDECOMPRESS_HURRYUP: u32 = 0x80000000;
pub const ICDECOMPRESS_UPDATE: u32 = 0x40000000;
pub const ICDECOMPRESS_PREROL: u32 = 0x20000000;
pub const ICDECOMPRESS_NULLFRAME: u32 = 0x10000000;
pub const ICDECOMPRESS_NOTKEYFRAME: u32 = 0x08000000;

/// `vfw.h`: `ICERR_OK = 0`. Most query-style messages should
/// return this when the answer is "yes".
pub const ICERR_OK: i32 = 0;
/// `vfw.h`: `ICERR_UNSUPPORTED = -1`.
pub const ICERR_UNSUPPORTED: i32 = -1;
/// `vfw.h`: `ICERR_BADFORMAT = -2`.
pub const ICERR_BADFORMAT: i32 = -2;

// --- BITMAPINFOHEADER ------------------------------------------------

/// `wingdi.h` `BITMAPINFOHEADER` — 40 bytes. Round-2 only models
/// the canonical fields; the trailing extension area (used by
/// codecs that ship a private blob after the header) is not
/// touched by these helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bih {
    /// `biSize` — must be `>= 40`.
    pub bi_size: u32,
    pub width: i32,
    pub height: i32,
    pub planes: u16,
    pub bit_count: u16,
    /// 4-byte FOURCC stored little-endian (`b'cvid'` for Cinepak,
    /// 0 for `BI_RGB`).
    pub compression: [u8; 4],
    pub size_image: u32,
    pub x_pels_per_meter: i32,
    pub y_pels_per_meter: i32,
    pub clr_used: u32,
    pub clr_important: u32,
}

impl Default for Bih {
    fn default() -> Self {
        Bih {
            bi_size: 40,
            width: 0,
            height: 0,
            planes: 1,
            bit_count: 24,
            compression: [0; 4],
            size_image: 0,
            x_pels_per_meter: 0,
            y_pels_per_meter: 0,
            clr_used: 0,
            clr_important: 0,
        }
    }
}

/// Total size of a canonical `BITMAPINFOHEADER` record.
pub const BIH_SIZE: u32 = 40;

/// Marshal a host-side [`Bih`] into 40 bytes at `addr` in guest
/// memory. The page must be R+W and have at least 40 bytes
/// mapped.
pub fn host_bih_to_guest(mmu: &mut Mmu, bih: &Bih, addr: u32) -> Result<(), Win32Error> {
    let trap = |t: crate::emulator::Trap| Win32Error::InvalidArgument {
        stub: "host_bih_to_guest",
        reason: format!("{t}"),
    };
    mmu.store32(addr, bih.bi_size).map_err(trap)?;
    mmu.store32(addr + 4, bih.width as u32).map_err(trap)?;
    mmu.store32(addr + 8, bih.height as u32).map_err(trap)?;
    mmu.store16(addr + 12, bih.planes).map_err(trap)?;
    mmu.store16(addr + 14, bih.bit_count).map_err(trap)?;
    mmu.write(addr + 16, &bih.compression).map_err(trap)?;
    mmu.store32(addr + 20, bih.size_image).map_err(trap)?;
    mmu.store32(addr + 24, bih.x_pels_per_meter as u32)
        .map_err(trap)?;
    mmu.store32(addr + 28, bih.y_pels_per_meter as u32)
        .map_err(trap)?;
    mmu.store32(addr + 32, bih.clr_used).map_err(trap)?;
    mmu.store32(addr + 36, bih.clr_important).map_err(trap)?;
    Ok(())
}

/// Read a guest [`Bih`] back into a host struct.
pub fn guest_bih_to_host(mmu: &Mmu, addr: u32) -> Result<Bih, Win32Error> {
    let trap = |t: crate::emulator::Trap| Win32Error::InvalidArgument {
        stub: "guest_bih_to_host",
        reason: format!("{t}"),
    };
    let bi_size = mmu.load32(addr).map_err(trap)?;
    let width = mmu.load32(addr + 4).map_err(trap)? as i32;
    let height = mmu.load32(addr + 8).map_err(trap)? as i32;
    let planes = mmu.load16(addr + 12).map_err(trap)?;
    let bit_count = mmu.load16(addr + 14).map_err(trap)?;
    let mut compression = [0u8; 4];
    for (i, c) in compression.iter_mut().enumerate() {
        *c = mmu.load8(addr + 16 + i as u32).map_err(trap)?;
    }
    let size_image = mmu.load32(addr + 20).map_err(trap)?;
    let x_pels_per_meter = mmu.load32(addr + 24).map_err(trap)? as i32;
    let y_pels_per_meter = mmu.load32(addr + 28).map_err(trap)? as i32;
    let clr_used = mmu.load32(addr + 32).map_err(trap)?;
    let clr_important = mmu.load32(addr + 36).map_err(trap)?;
    Ok(Bih {
        bi_size,
        width,
        height,
        planes,
        bit_count,
        compression,
        size_image,
        x_pels_per_meter,
        y_pels_per_meter,
        clr_used,
        clr_important,
    })
}

// --- ICDECOMPRESS structure ------------------------------------------

/// `vfw.h` `ICDECOMPRESS` — the lParam1 of `ICM_DECOMPRESS`.
///
/// ```c
/// typedef struct {
///     DWORD              dwFlags;
///     LPBITMAPINFOHEADER lpbiInput;
///     LPVOID             lpInput;
///     LPBITMAPINFOHEADER lpbiOutput;
///     LPVOID             lpOutput;
///     DWORD              ckid;
/// } ICDECOMPRESS;
/// ```
pub const ICDECOMPRESS_SIZE: u32 = 24;

/// `vfw.h` `ICDECOMPRESSEX` — the lParam1 of
/// `ICM_DECOMPRESS_BEGIN` / `ICM_DECOMPRESS_END`. The full
/// definition has source/dst rect arrays; we model the header
/// fields the codec actually reads during BEGIN / END (input
/// bih, output bih, dwFlags). Round-2 doesn't yet drive
/// per-region decode.
pub const ICDECOMPRESSEX_SIZE: u32 = 88;

// --- IC* host-side wrappers ------------------------------------------

/// Open a codec instance.
///
/// `fcc_type` is a 4CC stored as `u32::from_le_bytes(b"VIDC")`
/// for video.  `fcc_handler` is the codec's 4CC (`b"cvid"` for
/// Cinepak; case-insensitive in real vfw32, but the codec
/// dispatch is up to the codec). `mode` is one of:
///
/// * `1` — `ICMODE_DECOMPRESS`
/// * `2` — `ICMODE_COMPRESS`
/// * `3` — `ICMODE_FASTDECOMPRESS`
/// * `4` — `ICMODE_QUERY`
///
/// On success a synthetic `HIC` is minted and the codec's
/// `DriverProc(0, 0, DRV_OPEN, 0, 0)` is invoked. If `DriverProc`
/// returns 0, the open fails and the HIC is not retained.
pub fn ic_open(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    registry: &Registry,
    state: &mut HostState,
    fcc_type: u32,
    fcc_handler: u32,
    mode: u32,
) -> Result<u32, crate::Error> {
    let driver_proc = state.default_driver_proc;
    if driver_proc == 0 {
        return Err(Win32Error::InvalidArgument {
            stub: "ICOpen",
            reason: "no codec image staged (host-side)".into(),
        }
        .into());
    }
    // Real vfw32 calls
    //   DriverProc(dwDriverId=0, hdrvr=0, DRV_OPEN, 0, &ICOPEN).
    // The ICOPEN structure (vfw.h) is what triggers the codec to
    // allocate per-instance state and return a real pointer-as-
    // dwDriverId. Round-4 passed NULL for `lParam2`; Indeo 3 then
    // returns the magic sentinel 0xFFFF0000, which is NOT a real
    // pointer — every subsequent message that dereferences
    // `dwDriverId` faults. Round-5 stages a real ICOPEN.
    //
    // ICOPEN layout (vfw.h, 36 bytes, 9 dwords):
    //   +0  DWORD   dwSize       = 36
    //   +4  DWORD   fccType      = caller's `fcc_type`  ('VIDC')
    //   +8  DWORD   fccHandler   = caller's `fcc_handler` ('IV31')
    //   +12 DWORD   dwVersion    = 0x00010000 (vfw 1.0)
    //   +16 DWORD   dwFlags      = caller's `mode`
    //   +20 LRESULT dwError      = 0 (out — codec sets on err)
    //   +24 LPVOID  pV1Reserved  = 0
    //   +28 LPVOID  pV2Reserved  = 0
    //   +32 DWORD   dnDevNode    = 0
    const ICOPEN_SIZE: u32 = 36;
    let icopen = state.arena_alloc(ICOPEN_SIZE)?;
    let bytes: [u8; 36] = {
        let mut b = [0u8; 36];
        b[0..4].copy_from_slice(&ICOPEN_SIZE.to_le_bytes());
        b[4..8].copy_from_slice(&fcc_type.to_le_bytes());
        b[8..12].copy_from_slice(&fcc_handler.to_le_bytes());
        b[12..16].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        b[16..20].copy_from_slice(&mode.to_le_bytes());
        // dwError, pV1, pV2, dnDevNode all left as 0
        b
    };
    mmu.write_initializer(icopen, &bytes)
        .map_err(|t| Win32Error::InvalidArgument {
            stub: "ICOpen",
            reason: format!("{t}"),
        })?;
    let driver_id = call_guest(
        cpu,
        mmu,
        registry,
        state,
        driver_proc,
        &[0, 0, DRV_OPEN, 0, icopen],
    )?;
    if driver_id == 0 {
        return Ok(0);
    }
    let hic = state.next_hic;
    state.next_hic = state.next_hic.wrapping_add(1);
    state.hics.insert(
        hic,
        HicEntry {
            fcc_type,
            fcc_handler,
            mode,
            driver_proc_va: driver_proc,
            driver_id,
        },
    );
    Ok(hic)
}

/// Close a codec instance. Calls `DriverProc(driver_id, hic,
/// DRV_CLOSE, 0, 0)` and removes the HIC entry.
pub fn ic_close(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    registry: &Registry,
    state: &mut HostState,
    hic: u32,
) -> Result<u32, crate::Error> {
    let entry = match state.hics.remove(&hic) {
        Some(e) => e,
        None => {
            return Err(Win32Error::InvalidArgument {
                stub: "ICClose",
                reason: format!("unknown HIC {hic}"),
            }
            .into());
        }
    };
    call_guest(
        cpu,
        mmu,
        registry,
        state,
        entry.driver_proc_va,
        &[entry.driver_id, hic, DRV_CLOSE, 0, 0],
    )
}

/// Synthesise an `ICINFO` record by calling
/// `DriverProc(_, _, ICM_GETINFO, &scratch, cb)` and reading
/// back what the codec wrote.
///
/// Real `vfw32!ICGetInfo` populates `fccType`, `fccHandler`, and
/// the three string fields (`szName`, `szDescription`, `szDriver`)
/// from registry data **before** posting `ICM_GETINFO`, so that
/// codecs that don't fill the string fields still report a name.
/// We have no registry; if the codec leaves `szName` empty we
/// synthesise a four-character ASCII rendering of the fcc handler
/// so callers (the integration test, the eventual codec wrapper)
/// see a non-empty descriptor. The fcc-derived fallback is purely
/// the host-side "I have no registry" cushion, **not** a claim
/// about what the codec returned.
pub fn ic_get_info(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    registry: &Registry,
    state: &mut HostState,
    hic: u32,
    cb: u32,
) -> Result<Vec<u8>, crate::Error> {
    let entry = state
        .hics
        .get(&hic)
        .cloned()
        .ok_or_else(|| Win32Error::InvalidArgument {
            stub: "ICGetInfo",
            reason: format!("unknown HIC {hic}"),
        })?;
    let scratch = state.arena_alloc(cb)?;
    // Stamp the mapped bytes to zero (arena_alloc already zeroed
    // the host-mirror buffer, but we want to make sure mmu pages
    // are actually populated for read-back).
    let zeros = vec![0u8; cb as usize];
    mmu.write_initializer(scratch, &zeros)
        .map_err(|t| Win32Error::InvalidArgument {
            stub: "ICGetInfo",
            reason: format!("{t}"),
        })?;
    let written = call_guest(
        cpu,
        mmu,
        registry,
        state,
        entry.driver_proc_va,
        &[entry.driver_id, hic, ICM_GETINFO, scratch, cb],
    )?;
    let n = written.min(cb) as usize;
    let mut out = vec![0u8; n];
    for (i, b) in out.iter_mut().enumerate() {
        *b = mmu
            .load8(scratch + i as u32)
            .map_err(|t| Win32Error::InvalidArgument {
                stub: "ICGetInfo",
                reason: format!("{t}"),
            })?;
    }
    // szName starts at offset 24 (dwSize..dwVersionICM = 6 dwords)
    // and is 16 WCHARs (32 bytes). If the codec left it all-NUL,
    // fall back to the fcc handler — `vfw32!ICGetInfo` would
    // normally fill this from a registry "DESCRIPTION" entry.
    if out.len() >= 24 + 32 && out[24..24 + 32].iter().all(|b| *b == 0) {
        let fcc = entry.fcc_handler.to_le_bytes();
        for (i, &c) in fcc.iter().enumerate() {
            if 24 + i * 2 + 1 < out.len() {
                out[24 + i * 2] = c;
            }
        }
    }
    Ok(out)
}

/// Ask the codec whether it can decompress the given input format
/// to the given output format. Returns the `LRESULT` value
/// `DriverProc` chose — 0 means yes, negative means no.
///
/// Either `output` may be `None` to defer the format choice to
/// the codec.
pub fn ic_decompress_query(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    registry: &Registry,
    state: &mut HostState,
    hic: u32,
    input: &Bih,
    output: Option<&Bih>,
) -> Result<u32, crate::Error> {
    let entry = state
        .hics
        .get(&hic)
        .cloned()
        .ok_or_else(|| Win32Error::InvalidArgument {
            stub: "ICDecompressQuery",
            reason: format!("unknown HIC {hic}"),
        })?;
    let in_addr = state.arena_alloc(BIH_SIZE)?;
    host_bih_to_guest(mmu, input, in_addr)?;
    let out_addr = if let Some(out_bih) = output {
        let a = state.arena_alloc(BIH_SIZE)?;
        host_bih_to_guest(mmu, out_bih, a)?;
        a
    } else {
        0
    };
    call_guest(
        cpu,
        mmu,
        registry,
        state,
        entry.driver_proc_va,
        &[
            entry.driver_id,
            hic,
            ICM_DECOMPRESS_QUERY,
            in_addr,
            out_addr,
        ],
    )
}

/// Negotiate the decoder pipeline. Returns the codec's `LRESULT`.
pub fn ic_decompress_begin(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    registry: &Registry,
    state: &mut HostState,
    hic: u32,
    input: &Bih,
    output: &Bih,
) -> Result<u32, crate::Error> {
    let entry = state
        .hics
        .get(&hic)
        .cloned()
        .ok_or_else(|| Win32Error::InvalidArgument {
            stub: "ICDecompressBegin",
            reason: format!("unknown HIC {hic}"),
        })?;
    let in_addr = state.arena_alloc(BIH_SIZE)?;
    host_bih_to_guest(mmu, input, in_addr)?;
    let out_addr = state.arena_alloc(BIH_SIZE)?;
    host_bih_to_guest(mmu, output, out_addr)?;
    call_guest(
        cpu,
        mmu,
        registry,
        state,
        entry.driver_proc_va,
        &[
            entry.driver_id,
            hic,
            ICM_DECOMPRESS_BEGIN,
            in_addr,
            out_addr,
        ],
    )
}

/// End-of-stream cleanup. Returns the codec's `LRESULT`.
pub fn ic_decompress_end(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    registry: &Registry,
    state: &mut HostState,
    hic: u32,
) -> Result<u32, crate::Error> {
    let entry = state
        .hics
        .get(&hic)
        .cloned()
        .ok_or_else(|| Win32Error::InvalidArgument {
            stub: "ICDecompressEnd",
            reason: format!("unknown HIC {hic}"),
        })?;
    call_guest(
        cpu,
        mmu,
        registry,
        state,
        entry.driver_proc_va,
        &[entry.driver_id, hic, ICM_DECOMPRESS_END, 0, 0],
    )
}

/// Decompress one frame.
///
/// Lays out an `ICDECOMPRESS` structure in guest memory,
/// populates input/output `BITMAPINFOHEADER`s and the encoded /
/// raw byte buffers, calls `DriverProc(_, _, ICM_DECOMPRESS,
/// &icd, sizeof)`, then reads the decoded bytes back out.
///
/// `flags` is the `dwFlags` field of `ICDECOMPRESS`; a typical
/// value for the first frame is `0` (sender's choice; common
/// codec implementations key on `ICDECOMPRESS_NOTKEYFRAME`).
#[allow(clippy::too_many_arguments)]
pub fn ic_decompress(
    cpu: &mut Cpu,
    mmu: &mut Mmu,
    registry: &Registry,
    state: &mut HostState,
    hic: u32,
    flags: u32,
    input_bih: &Bih,
    input_bytes: &[u8],
    output_bih: &Bih,
    output_capacity: u32,
) -> Result<(u32, Vec<u8>), crate::Error> {
    let entry = state
        .hics
        .get(&hic)
        .cloned()
        .ok_or_else(|| Win32Error::InvalidArgument {
            stub: "ICDecompress",
            reason: format!("unknown HIC {hic}"),
        })?;

    // Lay out the four pieces of guest scratch:
    //   bi-input, bi-output, in-bytes, out-bytes
    let bi_in = state.arena_alloc(BIH_SIZE)?;
    host_bih_to_guest(mmu, input_bih, bi_in)?;
    let bi_out = state.arena_alloc(BIH_SIZE)?;
    host_bih_to_guest(mmu, output_bih, bi_out)?;

    let in_buf = state.arena_alloc(input_bytes.len() as u32)?;
    mmu.write_initializer(in_buf, input_bytes)
        .map_err(|t| Win32Error::InvalidArgument {
            stub: "ICDecompress",
            reason: format!("{t}"),
        })?;
    let out_buf = state.arena_alloc(output_capacity)?;
    let zeros = vec![0u8; output_capacity as usize];
    mmu.write_initializer(out_buf, &zeros)
        .map_err(|t| Win32Error::InvalidArgument {
            stub: "ICDecompress",
            reason: format!("{t}"),
        })?;

    // Lay out the ICDECOMPRESS struct.
    let icd = state.arena_alloc(ICDECOMPRESS_SIZE)?;
    let trap = |t: crate::emulator::Trap| Win32Error::InvalidArgument {
        stub: "ICDecompress",
        reason: format!("{t}"),
    };
    mmu.store32(icd, flags).map_err(trap)?;
    mmu.store32(icd + 4, bi_in).map_err(trap)?;
    mmu.store32(icd + 8, in_buf).map_err(trap)?;
    mmu.store32(icd + 12, bi_out).map_err(trap)?;
    mmu.store32(icd + 16, out_buf).map_err(trap)?;
    mmu.store32(icd + 20, 0).map_err(trap)?; // ckid

    let lresult = call_guest(
        cpu,
        mmu,
        registry,
        state,
        entry.driver_proc_va,
        &[entry.driver_id, hic, ICM_DECOMPRESS, icd, ICDECOMPRESS_SIZE],
    )?;

    // Read back the decoded bytes.
    let mut out = vec![0u8; output_capacity as usize];
    for (i, b) in out.iter_mut().enumerate() {
        *b = mmu
            .load8(out_buf + i as u32)
            .map_err(|t| Win32Error::InvalidArgument {
                stub: "ICDecompress",
                reason: format!("{t}"),
            })?;
    }
    Ok((lresult, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::{
        isa_int::RET_SENTINEL,
        mmu::{Mmu, Perm},
        regs::Reg32,
        Cpu,
    };
    use crate::win32::{HostState, Registry};

    /// Build a synthetic "DriverProc" — three bytes of code at
    /// `va` that load the second argument (`hdrvr` for OPEN /
    /// CLOSE; or just an `imm32` we choose) into eax and return:
    ///
    /// ```asm
    ///     mov eax, imm32
    ///     ret 20  ; pop 5 stdcall dwords
    /// ```
    fn install_canned_driver_proc(mmu: &mut Mmu, va: u32, ret_value: u32) {
        // mov eax, imm32  (B8 imm32)
        // ret 20          (C2 14 00)
        mmu.map(va & !0xFFF, 0x1000, Perm::R | Perm::W | Perm::X);
        let mut code = [0u8; 8];
        code[0] = 0xB8;
        code[1..5].copy_from_slice(&ret_value.to_le_bytes());
        code[5] = 0xC2;
        code[6..8].copy_from_slice(&20u16.to_le_bytes());
        mmu.write_initializer(va, &code).unwrap();
    }

    fn make_env() -> (Cpu, Mmu, Registry, HostState) {
        let mut mmu = Mmu::new();
        // Heap arena
        mmu.map(0x6000_0000, 0x10_0000, Perm::R | Perm::W);
        // Stack
        mmu.map(0x9000_0000, 0x10_0000, Perm::R | Perm::W);
        let mut cpu = Cpu::new();
        cpu.regs.set_esp(0x9000_0000 + 0x0F_0000);
        let mut registry = Registry::new();
        registry.register_kernel32();
        let state = HostState::new(0x6000_0000, 0x6000_0000 + 0x10_0000);
        (cpu, mmu, registry, state)
    }

    #[test]
    fn host_bih_marshal_roundtrip() {
        let (mut _cpu, mut mmu, _registry, _state) = make_env();
        let bih = Bih {
            bi_size: 40,
            width: 320,
            height: 240,
            planes: 1,
            bit_count: 24,
            compression: *b"cvid",
            size_image: 320 * 240 * 3 / 2,
            x_pels_per_meter: 0,
            y_pels_per_meter: 0,
            clr_used: 0,
            clr_important: 0,
        };
        host_bih_to_guest(&mut mmu, &bih, 0x6000_0000).unwrap();
        let back = guest_bih_to_host(&mmu, 0x6000_0000).unwrap();
        assert_eq!(bih, back);
    }

    #[test]
    fn ic_open_with_canned_driver_returns_synthetic_hic() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        // Plant DriverProc at a fixed VA returning 0xC0FFEE
        // (driver-id) for DRV_OPEN.
        let dpv = 0x0040_0000;
        install_canned_driver_proc(&mut mmu, dpv, 0xC0FFEE);
        state.default_driver_proc = dpv;
        let fcc_video = u32::from_le_bytes(*b"VIDC");
        let fcc_cvid = u32::from_le_bytes(*b"cvid");
        let hic = ic_open(
            &mut cpu, &mut mmu, &registry, &mut state, fcc_video, fcc_cvid, 1,
        )
        .unwrap();
        assert_ne!(hic, 0);
        let entry = state.hics.get(&hic).unwrap();
        assert_eq!(entry.driver_id, 0xC0FFEE);
        assert_eq!(entry.driver_proc_va, dpv);
        assert_eq!(entry.fcc_type, fcc_video);
        assert_eq!(entry.fcc_handler, fcc_cvid);
        // Sentinel was popped — eip should be RET_SENTINEL or
        // wherever the runner left it. The contract is just that
        // the hic was installed.
        assert_eq!(cpu.regs.eip, RET_SENTINEL);
    }

    #[test]
    fn ic_open_returning_zero_does_not_install_hic() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        let dpv = 0x0040_0000;
        install_canned_driver_proc(&mut mmu, dpv, 0);
        state.default_driver_proc = dpv;
        let hic = ic_open(&mut cpu, &mut mmu, &registry, &mut state, 0, 0, 1).unwrap();
        assert_eq!(hic, 0);
        assert!(state.hics.is_empty());
    }

    #[test]
    fn ic_close_invokes_driver_proc_and_drops_hic() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        let dpv = 0x0040_0000;
        install_canned_driver_proc(&mut mmu, dpv, 0xABCD);
        state.default_driver_proc = dpv;
        let hic = ic_open(&mut cpu, &mut mmu, &registry, &mut state, 0, 0, 1).unwrap();
        // ic_close: codec returns whatever, we ensure the HIC is
        // gone.
        let _ = ic_close(&mut cpu, &mut mmu, &registry, &mut state, hic).unwrap();
        assert!(state.hics.is_empty());
    }

    #[test]
    fn ic_close_unknown_hic_errors() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        let r = ic_close(&mut cpu, &mut mmu, &registry, &mut state, 99);
        assert!(r.is_err());
    }

    #[test]
    fn ic_get_info_reads_back_codec_buffer() {
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        // DriverProc that, instead of writing icinfo, just
        // returns the cb argument so we can verify the message
        // round-trip + the cb passed into the callback.
        let dpv = 0x0040_0000;
        install_canned_driver_proc(&mut mmu, dpv, 16);
        state.default_driver_proc = dpv;
        let hic = ic_open(&mut cpu, &mut mmu, &registry, &mut state, 0, 0, 1).unwrap();
        let bytes = ic_get_info(&mut cpu, &mut mmu, &registry, &mut state, hic, 32).unwrap();
        // We allocated 32; the canned proc returns 16 → we read
        // the first 16 bytes (all zero from arena_alloc).
        assert_eq!(bytes.len(), 16);
        assert!(bytes.iter().all(|b| *b == 0));
    }

    #[test]
    fn ic_decompress_round_trip_passes_buffers_through_emulator() {
        // Use a slightly-richer DriverProc that copies the input
        // bytes into the output buffer when ICM_DECOMPRESS fires.
        // For the canned proc to do that we'd need a real
        // assembly routine — too much for an isa_int unit test.
        // Instead we just check the call dispatches and the
        // output buffer survives.
        let (mut cpu, mut mmu, registry, mut state) = make_env();
        let dpv = 0x0040_0000;
        // Plant a non-zero return value so DRV_OPEN succeeds and
        // mints a HIC. The same canned proc is then re-used for
        // ICM_DECOMPRESS, which expects 0 (ICERR_OK) — but our
        // contract here is just "the buffers round-trip cleanly".
        install_canned_driver_proc(&mut mmu, dpv, 0xDEAD_BEEF);
        state.default_driver_proc = dpv;
        let hic = ic_open(&mut cpu, &mut mmu, &registry, &mut state, 0, 0, 1).unwrap();
        assert_ne!(hic, 0);
        let bih_in = Bih {
            width: 16,
            height: 16,
            bit_count: 24,
            compression: *b"cvid",
            ..Default::default()
        };
        let bih_out = Bih {
            width: 16,
            height: 16,
            bit_count: 24,
            ..Default::default()
        };
        let input = vec![0xAAu8; 64];
        let (lr, out) = ic_decompress(
            &mut cpu,
            &mut mmu,
            &registry,
            &mut state,
            hic,
            0,
            &bih_in,
            &input,
            &bih_out,
            16 * 16 * 3,
        )
        .unwrap();
        assert_eq!(lr, 0xDEAD_BEEF);
        assert_eq!(out.len(), 16 * 16 * 3);
        // Eax was set to the canned LRESULT.
        assert_eq!(cpu.regs.get32(Reg32::Eax), 0xDEAD_BEEF);
    }
}
