//! Test-only fixture-discovery helper.
//!
//! Round 3 introduces real-codec smoke tests against Intel's
//! `IR32_32.DLL` (Indeo 3) redistributable. This module owns
//! locating the DLL bytes — looking through any user-staged
//! directory, the host's Wine prefix, the host's system32 (on
//! Windows), the local cache, and finally the canonical HTTPS
//! mirror — before the test runs.
//!
//! Each tier is checked in order. The first hit wins. When none
//! hit and HTTPS succeeds, the bytes are written to the local
//! cache. Subsequent runs short-circuit at the cache step.
//!
//! In CI (`CI=true` env var set), the cache is bypassed in both
//! directions so the tests always exercise the network path —
//! this avoids depending on a stale cache between CI runs.
//!
//! The helper intentionally fails loudly on any miss: round 3
//! demands real codec coverage, so silently skipping is the wrong
//! shape. If `ureq` cannot reach `samples.oxideav.org`, the test
//! reports the network failure verbatim.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
pub mod avi_extractor;
#[allow(dead_code)]
pub mod mov_extractor;

/// Canonical HTTPS prefix for the Intel IV5 driver bundle.
/// Each filename listed in `tests/README.md` is appended verbatim.
const BASE_URL: &str = "https://samples.oxideav.org/codecs/windows/IV5PLAY";

/// Canonical HTTPS prefix for the FFmpeg samples corpus,
/// indexed by FourCC. Used by round 7+ to fetch real-codec
/// `.mov` / `.avi` payloads. The full URL is built as
/// `<FFMPEG_BASE_URL>/<FOURCC>/<NAME>`.
const FFMPEG_BASE_URL: &str = "https://samples.oxideav.org/ffmpeg/V-codecs";

/// Resolve `name` (e.g. `"IR32_32.DLL"`) to a byte buffer.
///
/// Resolution order:
///
/// 1. `OXIDEAV_VFW_FIXTURE_DIR` env var, if set.
/// 2. Wine prefix (Linux + macOS): `~/.wine/drive_c/windows/system32/`,
///    then `~/.wine/drive_c/windows/syswow64/`.
/// 3. System paths (Windows host): `%SystemRoot%\\SysWOW64\\` then
///    `%SystemRoot%\\System32\\`.
/// 4. Local cache: `<CARGO_TARGET_DIR or target>/test-fixture-cache/<NAME>`.
///    Skipped (read + write) when `CI=true`.
/// 5. HTTPS fetch from `BASE_URL/<NAME>`.
///
/// On HTTPS success, step 4's cache is populated (unless `CI=true`).
pub fn fetch_or_load(name: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // 1. Explicit user override.
    if let Some(dir) = env::var_os("OXIDEAV_VFW_FIXTURE_DIR") {
        let dir = PathBuf::from(dir);
        if let Some(bytes) = read_case_insensitive(&dir, name)? {
            return Ok(bytes);
        }
        return Err(format!(
            "OXIDEAV_VFW_FIXTURE_DIR={} set but {name} not found there",
            dir.display()
        )
        .into());
    }

    // 2. Wine prefix (Linux + macOS).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        for sub in ["drive_c/windows/syswow64", "drive_c/windows/system32"] {
            let dir = home.join(".wine").join(sub);
            if let Some(bytes) = read_case_insensitive(&dir, name)? {
                return Ok(bytes);
            }
        }
    }

    // 3. Windows system paths.
    #[cfg(windows)]
    if let Some(sysroot) = env::var_os("SystemRoot") {
        let sysroot = PathBuf::from(sysroot);
        for sub in ["SysWOW64", "System32"] {
            let dir = sysroot.join(sub);
            if let Some(bytes) = read_case_insensitive(&dir, name)? {
                return Ok(bytes);
            }
        }
    }

    let ci = env::var("CI")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    // 4. Local cache (skipped in CI).
    let cache_path = cache_dir().join(name.to_ascii_uppercase());
    if !ci && cache_path.exists() {
        return Ok(fs::read(&cache_path)?);
    }

    // 5. HTTPS fetch.
    let url = format!("{BASE_URL}/{name}");
    let bytes = http_fetch(&url).map_err(|e| format!("HTTPS fetch of {url} failed: {e}"))?;

    if !ci {
        // Best-effort cache write; never fail the test on cache I/O.
        let _ = fs::create_dir_all(cache_dir());
        let _ = fs::write(&cache_path, &bytes);
    }

    Ok(bytes)
}

/// Round-7 sibling of [`fetch_or_load`] for the FFmpeg samples
/// corpus.
///
/// The corpus URL pattern is
/// `https://samples.oxideav.org/ffmpeg/V-codecs/<FOURCC>/<NAME>`.
/// `fourcc` is the codec FourCC (e.g. `"IV32"`, `"IV50"`),
/// `name` is the leaf file name (e.g. `"cubes.mov"`).
///
/// Resolution order — same tiers as [`fetch_or_load`]:
///
/// 1. `OXIDEAV_VFW_FIXTURE_DIR/<NAME>` (case-insensitive).
/// 2. Local cache: `<CARGO_TARGET_DIR>/test-fixture-cache/<FOURCC>-<NAME>`.
///    Skipped (read + write) when `CI=true`.
/// 3. HTTPS fetch from
///    `<FFMPEG_BASE_URL>/<FOURCC>/<NAME>`.
///
/// Wine + Windows-system-path tiers are skipped here — the
/// FFmpeg corpus carries no Windows-side analogue.
#[allow(dead_code)]
pub fn fetch_or_load_ffmpeg_sample(
    fourcc: &str,
    name: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // 1. Explicit user override (same env var as fetch_or_load).
    if let Some(dir) = env::var_os("OXIDEAV_VFW_FIXTURE_DIR") {
        let dir = PathBuf::from(dir);
        if let Some(bytes) = read_case_insensitive(&dir, name)? {
            return Ok(bytes);
        }
        // Fall through if the env-var is set but this file isn't in
        // the override dir — the FFmpeg corpus is large + the user
        // only stages the fixtures they care about. Drop to cache /
        // network like the un-overridden case.
    }

    let ci = env::var("CI")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    // 2. Local cache. Cache key includes fourcc to avoid clashes
    //    when two corpora ship a `cubes.mov`.
    let cache_key = format!("{}-{}", fourcc.to_ascii_uppercase(), name);
    let cache_path = cache_dir().join(&cache_key);
    if !ci && cache_path.exists() {
        return Ok(fs::read(&cache_path)?);
    }

    // 3. HTTPS fetch.
    let url = format!("{FFMPEG_BASE_URL}/{fourcc}/{name}");
    let bytes = http_fetch(&url).map_err(|e| format!("HTTPS fetch of {url} failed: {e}"))?;

    if !ci {
        let _ = fs::create_dir_all(cache_dir());
        let _ = fs::write(&cache_path, &bytes);
    }

    Ok(bytes)
}

/// Open `<dir>/<name>` with case-insensitive matching on the
/// filename component. Returns the bytes on success, `None` if
/// the file isn't present, and an error only on I/O failure.
fn read_case_insensitive(dir: &Path, name: &str) -> std::io::Result<Option<Vec<u8>>> {
    if !dir.is_dir() {
        return Ok(None);
    }
    // Fast path: exact filename.
    let exact = dir.join(name);
    if exact.is_file() {
        return Ok(Some(fs::read(&exact)?));
    }
    // Case-insensitive scan of dir.
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
        {
            return Ok(Some(fs::read(entry.path())?));
        }
    }
    Ok(None)
}

/// Resolve the cache root: `$CARGO_TARGET_DIR/test-fixture-cache`,
/// or `target/test-fixture-cache` relative to the crate manifest.
fn cache_dir() -> PathBuf {
    if let Some(target) = env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(target).join("test-fixture-cache");
    }
    // CARGO_MANIFEST_DIR is set by Cargo when building tests.
    let manifest = env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    manifest.join("target").join("test-fixture-cache")
}

/// Enumerate every `(dll, function)` import the PE32 file at
/// `bytes` declares, by parsing the file's headers + import
/// directory directly (no MMU, no Loader). Used by round-3
/// tests to list which `kernel32` / `user32` / `gdi32` / etc.
/// stubs are needed before the loader's fail-fast import
/// resolution short-circuits at the first miss.
///
/// Returns the imports in `BTreeSet` order so the diagnostic
/// output is deterministic + easy to read.
#[allow(dead_code, clippy::while_let_loop)]
pub fn list_pe_imports(bytes: &[u8]) -> Result<BTreeSet<(String, String)>, String> {
    use oxideav_vfw::pe::header;

    let parsed = header::parse(bytes).map_err(|e| format!("PE parse failed: {e}"))?;
    let dir = parsed.optional.data_directories[header::IMAGE_DIRECTORY_ENTRY_IMPORT];
    let mut out = BTreeSet::new();
    if dir.virtual_address == 0 || dir.size == 0 {
        return Ok(out);
    }

    // RVA → file offset translator.
    let rva_to_file = |rva: u32| -> Option<usize> {
        for s in &parsed.sections {
            let start = s.virtual_address;
            let end = s
                .virtual_address
                .saturating_add(s.virtual_size.max(s.size_of_raw_data));
            if rva >= start && rva < end {
                let file_off = s.pointer_to_raw_data.saturating_add(rva - start) as usize;
                if file_off < bytes.len() {
                    return Some(file_off);
                }
            }
        }
        None
    };

    let read_u32 = |off: usize| -> Option<u32> {
        bytes
            .get(off..off + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    let read_cstr = |off: usize| -> String {
        let mut s = Vec::new();
        let mut p = off;
        while let Some(&b) = bytes.get(p) {
            if b == 0 {
                break;
            }
            s.push(b);
            p += 1;
            if s.len() >= 1024 {
                break;
            }
        }
        String::from_utf8_lossy(&s).into_owned()
    };

    let mut desc_rva = dir.virtual_address;
    loop {
        let desc_off = match rva_to_file(desc_rva) {
            Some(o) => o,
            None => break,
        };
        let original_first_thunk = read_u32(desc_off).unwrap_or(0);
        let name_rva = read_u32(desc_off + 12).unwrap_or(0);
        let first_thunk = read_u32(desc_off + 16).unwrap_or(0);
        if original_first_thunk == 0 && first_thunk == 0 && name_rva == 0 {
            break;
        }
        let dll = match rva_to_file(name_rva) {
            Some(o) => read_cstr(o),
            None => break,
        };
        let dll_lower = dll.to_ascii_lowercase();

        let table_rva = if original_first_thunk != 0 {
            original_first_thunk
        } else {
            first_thunk
        };
        let mut i: u32 = 0;
        loop {
            let entry_rva = table_rva.wrapping_add(4 * i);
            let entry_off = match rva_to_file(entry_rva) {
                Some(o) => o,
                None => break,
            };
            let entry = match read_u32(entry_off) {
                Some(v) => v,
                None => break,
            };
            if entry == 0 {
                break;
            }
            let name = if (entry & 0x8000_0000) != 0 {
                format!("@{}", entry & 0xFFFF)
            } else {
                let by_name_rva = entry & 0x7FFF_FFFF;
                match rva_to_file(by_name_rva) {
                    // IMAGE_IMPORT_BY_NAME: 2 bytes hint, then ASCIIZ.
                    Some(o) => read_cstr(o + 2),
                    None => break,
                }
            };
            out.insert((dll_lower.clone(), name));
            i = i.wrapping_add(1);
        }

        desc_rva = desc_rva.wrapping_add(20);
    }
    Ok(out)
}

/// HTTPS GET via `ureq`. Returns the raw body bytes.
fn http_fetch(url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Cap at 32 MiB — covers Intel IV5 DLLs (tens of KB) and the
    // FFmpeg corpus's small-shape `.mov` / `.avi` fixtures (low
    // single-digit MB). Anything beyond that is a server-side
    // surprise we want to refuse.
    const MAX_BYTES: u64 = 32 * 1024 * 1024;
    let resp = ureq::get(url).call()?;
    let mut buf = Vec::new();
    resp.into_reader().take(MAX_BYTES).read_to_end(&mut buf)?;
    Ok(buf)
}
