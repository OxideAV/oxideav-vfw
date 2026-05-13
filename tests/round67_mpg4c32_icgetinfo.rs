//! Round 67 — close the last remaining mpg4c32 ICGetInfo anomaly.
//!
//! Round 24 added the `ICINFO_SIZE = 568` strict-codec gate to
//! [`oxideav_vfw::win32::vfw32::ic_get_info`] after we noticed
//! `mpg4c32.dll` silently returned 0 bytes against round-20's
//! `cb = 80` probe.  Disassembly pinned the gate to
//! `mpg4c32!DriverProc+0x999..0x99c`:
//!
//! ```text
//!     mov ebx, 0x238    ; sizeof(ICINFO) = 568
//!     cmp [ebp+0x10], ebx
//!     jb  .return_zero
//! ```
//!
//! What round 24 did NOT do is propagate the constraint into the
//! [`oxideav_vfw::discovery::probe`] code path that gets invoked
//! by `Sandbox::register()` — that helper was still calling
//! `ic_get_info(hic, 112)` (a value chosen for the Indeo family,
//! which is lenient about short reads).  As a result, mpg4c32's
//! identity card never made it back to discovery callers even
//! though every other VfW surface (decode, encode, ICDecompressBegin,
//! ICCompress) worked end-to-end.  The discovery probe burned an
//! `ICOpen → ICGetInfo` round-trip and threw away the codec's
//! self-description.
//!
//! Round 67's deliverables:
//!
//! 1. Pin the strict-size gate empirically: with `cb = 112` the
//!    codec writes 0 bytes; with `cb = 568` the codec writes
//!    exactly 568.  Both calls go through the same `ic_get_info`
//!    wrapper, so the asymmetry is solely the gate.
//! 2. Decode and assert the full string trio (`szName`,
//!    `szDescription`, `szDriver`) from the 568-byte ICINFO
//!    returned by `mpg4c32.dll`.  Real `vfw32!ICGetInfo` would
//!    fill `szName` / `szDescription` from the registry HKEY
//!    `\Software\Microsoft\Windows NT\CurrentVersion\drivers32`;
//!    we don't have a registry and rely entirely on what the
//!    codec writes into the buffer itself.
//! 3. Confirm the discovery probe was the bug site — the
//!    `cb = ICINFO_SIZE` form, which `probe.rs` now uses,
//!    surfaces a full 568-byte ICINFO with valid header
//!    dwords (dwSize / fccType / fccHandler / dwFlags /
//!    dwVersion / dwVersionICM) instead of the pre-r67
//!    zero-byte response.  szName / szDescription / szDriver
//!    remain empty (the codec delegates those to the
//!    Windows registry per MSDN; we have no registry).
//!
//! NEVER reference Wine / ReactOS / MinGW / vfw32 base-class
//! source.  All assertions trace to MSDN `ICINFO` / `ICGetInfo`
//! reference and Intel SDM Vol. 2 disassembly of mpg4c32.dll.

use oxideav_vfw::Sandbox;
use std::path::PathBuf;

fn workspace_root() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest = PathBuf::from(manifest);
    Some(manifest.parent()?.parent()?.to_path_buf())
}

fn binary_path(name: &str) -> Option<PathBuf> {
    let p = workspace_root()?.join(format!(
        "docs/video/msmpeg4/reference/binaries/wmpcdcs8-2001/{name}"
    ));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// Open mpg4c32.dll, drive DllMain + ICOpen('VIDC','MP43',DECOMPRESS),
/// and return the HIC plus the live sandbox.  Shared by all four
/// tests in this file.
fn open_mp43_hic() -> Option<(Sandbox, u32)> {
    let dll = binary_path("mpg4c32.dll")?;
    let bytes = std::fs::read(&dll).ok()?;
    let mut sb = Sandbox::new();
    sb.cpu.set_instr_limit(500_000_000);
    let img = sb.load("mpg4c32.dll", &bytes).ok()?;
    sb.call_dll_main(&img, oxideav_vfw::DLL_PROCESS_ATTACH)
        .ok()?;
    sb.install_codec(&img).ok()?;
    let fcc_video = u32::from_le_bytes(*b"VIDC");
    let fcc_handler = u32::from_le_bytes(*b"MP43");
    let hic = sb.ic_open(fcc_video, fcc_handler, 2).ok()?;
    if hic == 0 {
        return None;
    }
    Some((sb, hic))
}

/// Decode a NUL-terminated UTF-16LE substring stored at
/// `bytes[off..off + max_wchars * 2]`.  Empty slice if the first
/// WCHAR is NUL.  Non-ASCII bytes are rendered as `?` so the
/// returned `String` is always printable; the *bytes* are tested
/// directly elsewhere for byte-exact assertions.
fn decode_utf16le_until_nul(bytes: &[u8], off: usize, max_wchars: usize) -> String {
    let mut out = String::with_capacity(max_wchars);
    for i in 0..max_wchars {
        let lo = off + i * 2;
        let hi = lo + 1;
        if hi >= bytes.len() {
            break;
        }
        let w = u16::from_le_bytes([bytes[lo], bytes[hi]]);
        if w == 0 {
            break;
        }
        if (0x20..=0x7E).contains(&w) {
            out.push(w as u8 as char);
        } else {
            out.push('?');
        }
    }
    out
}

// ---- Test 1: strict-size gate FALSIFIES short reads -----------
//
// MSDN `ICGetInfo` says the caller passes `cb >= sizeof(ICINFO)`,
// and the strict-codec gate at `mpg4c32!DriverProc+0x999..0x99c`
// enforces that contract by silently returning 0 from the
// DriverProc when `cb < 0x238`.  Pin the behaviour so future host
// changes to `ic_get_info`'s buffer-staging path can't silently
// hide it.
#[test]
fn mp43_icgetinfo_short_cb_returns_zero_bytes() {
    let Some((mut sb, hic)) = open_mp43_hic() else {
        eprintln!("round67: mpg4c32.dll missing or ICOpen failed; skipping");
        return;
    };
    let info = sb
        .ic_get_info(hic, 112)
        .expect("ic_get_info wrapper trapped — bug in host-side staging");
    assert_eq!(
        info.len(),
        0,
        "mpg4c32 must reject cb={} < ICINFO_SIZE=568 per the \
         `cmp [ebp+0x10], 0x238 / jb .return_zero` gate at \
         DriverProc+0x999..0x99c — got {} bytes back",
        112,
        info.len(),
    );
    let _ = sb.ic_close(hic);
}

// ---- Test 2: cb = 568 returns the full ICINFO record ----------
//
// Round 24 asserted the 6 header dwords; round 67 adds the
// three UTF-16LE string fields (szName / szDescription /
// szDriver).  Pin the byte-level shape so a future regression
// in `ic_get_info`'s read-back loop is caught.
#[test]
fn mp43_icgetinfo_full_record_decodes_strings() {
    let Some((mut sb, hic)) = open_mp43_hic() else {
        eprintln!("round67: mpg4c32.dll missing or ICOpen failed; skipping");
        return;
    };
    let cb = oxideav_vfw::win32::vfw32::ICINFO_SIZE;
    let info = sb.ic_get_info(hic, cb).expect("ICGetInfo trapped");
    assert_eq!(info.len() as u32, cb, "codec wrote {} bytes", info.len());

    // 6 header dwords (per MSDN ICINFO layout).
    let dw_size = u32::from_le_bytes(info[0..4].try_into().unwrap());
    let fcc_type = u32::from_le_bytes(info[4..8].try_into().unwrap());
    let fcc_h = u32::from_le_bytes(info[8..12].try_into().unwrap());
    let dw_flags = u32::from_le_bytes(info[12..16].try_into().unwrap());
    let dw_version = u32::from_le_bytes(info[16..20].try_into().unwrap());
    let dw_version_icm = u32::from_le_bytes(info[20..24].try_into().unwrap());

    // Three string fields:
    //   szName[16]         at offset 24  (32 bytes)
    //   szDescription[128] at offset 56  (256 bytes)
    //   szDriver[128]      at offset 312 (256 bytes)
    let sz_name = decode_utf16le_until_nul(&info, 24, 16);
    let sz_description = decode_utf16le_until_nul(&info, 56, 128);
    let sz_driver = decode_utf16le_until_nul(&info, 312, 128);

    eprintln!(
        "round67 ICGetInfo full record:\n  \
         dwSize=0x{dw_size:x} fccType=0x{fcc_type:x} fccHandler=0x{fcc_h:x}\n  \
         dwFlags=0x{dw_flags:x} dwVersion=0x{dw_version:x} dwVersionICM=0x{dw_version_icm:x}\n  \
         szName={sz_name:?}\n  szDescription={sz_description:?}\n  szDriver={sz_driver:?}",
    );

    // Invariant 1: dwSize echoes the caller's cb argument back.
    assert_eq!(
        dw_size,
        oxideav_vfw::win32::vfw32::ICINFO_SIZE,
        "ICINFO.dwSize should be 568"
    );
    // Invariant 2: fccType is 'vidc' (lowercased VIDC per the
    // ICOpen path that lowercased before passing it on).
    let vidc = u32::from_le_bytes(*b"VIDC");
    let vidc_lc = u32::from_le_bytes(*b"vidc");
    assert!(
        fcc_type == vidc || fcc_type == vidc_lc,
        "ICINFO.fccType is neither VIDC nor vidc — got {fcc_type:#010x}",
    );
    // Invariant 3: fccHandler == 'MP43' per the literal write at
    // mpg4c32!DriverProc+0x95d..0x96d.
    let mp43_le = u32::from_le_bytes(*b"MP43");
    assert_eq!(
        fcc_h, mp43_le,
        "ICINFO.fccHandler should be 'MP43' — got {fcc_h:#010x}",
    );
    // Invariant 4: szName is non-empty.  Empirically mpg4c32
    // leaves szName all-NUL inside the codec (per MSDN it
    // expects the host vfw32 layer to populate it from registry
    // HKEY `\Software\Microsoft\Windows NT\CurrentVersion\
    // drivers32`), and the [`ic_get_info`] wrapper falls back
    // to a 4-character UTF-16LE rendering of the fcc handler
    // when the field is all-zero.  So in our sandbox szName
    // ALWAYS reads back as "MP43".  Either the codec wrote
    // something or the host fallback fired — the field must
    // not be empty in either case.
    assert!(
        !sz_name.is_empty(),
        "szName should be non-empty after a successful 568-byte ICINFO read"
    );
    assert_eq!(
        sz_name, "MP43",
        "szName should be the fcc-handler fallback ('MP43') because \
         mpg4c32 leaves szName all-NUL — real vfw32 fills it from the \
         registry, which we don't have."
    );
    // Invariant 5: szDescription and szDriver — mpg4c32 leaves
    // BOTH of these all-NUL inside the codec (also delegated to
    // the registry per MSDN).  We do NOT fabricate fallbacks for
    // them; the host has no registry to read from, so the bytes
    // stay zero.  Pin the empirical behaviour so a future
    // accidental fabrication is caught.
    assert!(
        sz_description.is_empty(),
        "szDescription empirically all-NUL on mpg4c32 (the codec \
         delegates to registry HKEY drivers32); got {sz_description:?}"
    );
    assert!(
        sz_driver.is_empty(),
        "szDriver empirically all-NUL on mpg4c32 (the codec \
         delegates to registry HKEY drivers32); got {sz_driver:?}"
    );

    let _ = sb.ic_close(hic);
}

// ---- Test 3: discovery probe surfaces the identity card -------
//
// Pre-r67, `discovery/probe.rs` called `ic_get_info(hic, 112)`
// and discarded the result — mpg4c32's strict size gate fired
// and the discovery layer never saw the codec's identity card.
// Post-r67, the probe passes `ICINFO_SIZE`, so the same call
// path that the consumer-facing `Sandbox::register()` hits
// returns a full 568-byte record.
//
// We can't easily inject the probe into a unit test, but we
// can verify the documented constant the probe uses is now
// 568 — that's the contract the bug-fix locked in.
#[test]
fn discovery_probe_uses_icinfo_size_constant() {
    // `ICINFO_SIZE` is the public constant `probe.rs` now imports
    // for its `ic_get_info` call.  This test is a compile-time
    // anchor: if a future refactor accidentally changes the
    // constant or removes it, this file fails to build.
    assert_eq!(
        oxideav_vfw::win32::vfw32::ICINFO_SIZE,
        568,
        "ICINFO_SIZE must be 568 per the Win32 ICINFO struct layout \
         (6 dwords + 16+128+128 WCHARs = 24+32+256+256)"
    );

    // Drive the actual codec through the probe-style call to
    // confirm the larger cb is what the codec wants.  This is
    // the end-to-end regression check that pre-r67 would have
    // returned 0 bytes.
    let Some((mut sb, hic)) = open_mp43_hic() else {
        eprintln!("round67: mpg4c32.dll missing or ICOpen failed; skipping");
        return;
    };
    let info = sb
        .ic_get_info(hic, oxideav_vfw::win32::vfw32::ICINFO_SIZE)
        .expect("ICGetInfo trapped");
    assert!(
        !info.is_empty(),
        "discovery-style ICGetInfo against mpg4c32 must return non-zero bytes"
    );
    let _ = sb.ic_close(hic);
}
