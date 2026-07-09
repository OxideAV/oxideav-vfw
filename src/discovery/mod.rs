//! Auto-discovery — round 28.
//!
//! At [`crate::register`] time we walk a discovery path, probe
//! every loadable `*.dll` / `*.ax` for a VfW or DirectShow entry
//! point, and register one [`oxideav_core::CodecInfo`] per
//! recognised FourCC into [`oxideav_core::RuntimeContext::codecs`].
//!
//! ### Discovery path resolution
//!
//! - `OXIDEAV_VFW_CODEC_PATH=/p1:/p2` (colon-separated on UNIX,
//!   `;`-separated on Windows) **replaces** the default — empty
//!   strings between separators and unreadable directories are
//!   skipped silently.
//! - Default (env var unset):
//!     - UNIX: `$XDG_DATA_HOME/oxideav/codecs/` or
//!       `$HOME/.local/share/oxideav/codecs/`.
//!     - Windows: `%LOCALAPPDATA%\oxideav\codecs\`.
//!
//! Walks each path **non-recursively** for `*.dll` and `*.ax`.
//! Files that aren't valid PE32 (or that simply lack the codec
//! entry points we know how to drive) are skipped silently with a
//! `log::debug!` — discovery never panics at register time.
//!
//! ### Probe priority
//!
//! 1. **VfW** — call `Sandbox::load` then drive
//!    `DRV_LOAD → ICOpen('VIDC', candidate_fcc) → ICGetInfo`. Try
//!    a small fixed FourCC sweep (`MP43 / MP42 / MPG4 / DIV3 /
//!    IV31 / IV41 / IV50 / CVID / MJPG`). Each FourCC the codec
//!    accepts becomes one [`DiscoveryEntry`] tagged
//!    [`Kind::Vfw`].
//! 2. **DirectShow** — try `DllGetClassObject` with a short
//!    static CLSID candidate list (the wmpcdcs8-2001 known
//!    binaries — `MPG4DS32.AX`'s factory CLSID `{82CCD3E0-…}`).
//!    Candidates that succeed are recorded as
//!    [`Kind::DirectShow`]; their FourCCs are extracted from
//!    `IPin::EnumMediaTypes` (deferred — round 29 wires the full
//!    AMT walk; this round records the CLSID + leaves
//!    `fourccs` empty if the AMT walk fails).
//! 3. Anything that exports neither `DriverProc` nor a
//!    recognisable `DllGetClassObject` CLSID is recorded as
//!    [`Kind::Unsupported`] so we don't re-probe on the next
//!    `register()` call.
//!
//! ### Cache
//!
//! Discovery results are cached at
//! `$XDG_CACHE_HOME/oxideav/vfw-discovery.json`
//! (or `$HOME/.cache/oxideav/vfw-discovery.json`,
//! or `%LOCALAPPDATA%\oxideav\Cache\vfw-discovery.json`),
//! keyed by `(absolute_path, mtime, size_bytes)`. A stale entry
//! (mtime or size mismatch) is treated as a cache miss; on miss
//! we re-probe and overwrite the entry. The cache is written
//! atomically (tempfile + rename).
//!
//! See `docs/winmf/winmf-emulator.md` for the broader sandbox
//! design contract.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

mod cache;
mod codec;
mod paths;
mod probe;

pub use cache::{Cache, CacheEntry, CURRENT_SCHEMA_VERSION};
pub use codec::{
    codec_id_for, last_codec_allocator_negotiation, lookup_record, make_decoder, make_encoder,
    output_pixel_format, register_factory_for_id, resolve_encoder_knobs,
    unrecognized_encoder_knobs, CodecAllocatorNegotiation, DiscoveryRecord, EncoderKnobs,
    ENCODER_KNOB_DATA_RATE, ENCODER_KNOB_KEYINT, ENCODER_KNOB_KEYS, ENCODER_KNOB_QUALITY,
    ENCODER_QUALITY_MAX,
};
pub use paths::{cache_file_path, discovery_paths};
pub use probe::{probe_bytes, probe_dll, Kind, ProbeResult};

/// Whether the `register()` cascade should silently skip a missing
/// discovery directory (the default — true) or surface it as a
/// `log::warn!` (false). Tests flip this off to assert "directory
/// missing" still produces zero codecs cleanly.
const ALLOW_MISSING_DIR: bool = true;

/// One probed DLL → its (possibly empty) list of recognised
/// FourCCs. The cache JSON stores a `Vec` of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryEntry {
    pub path: PathBuf,
    pub mtime_unix: i64,
    pub size_bytes: u64,
    pub kind: Kind,
    pub fourccs: Vec<String>,
    pub clsid: Option<String>,
}

impl DiscoveryEntry {
    /// True if this entry's `(absolute_path, mtime_unix,
    /// size_bytes)` triple matches the supplied `(path, mtime,
    /// size)`. Used by the cache layer to decide whether to honour
    /// or invalidate a stored entry.
    ///
    /// Round 217 paired this with [`super::CacheEntry::matches`] —
    /// the two methods share the exact same triple-equality
    /// contract so an in-memory [`DiscoveryEntry`] and its on-disk
    /// [`super::CacheEntry`] mirror image can be queried with the
    /// same shape of staleness check. Drift between the two
    /// previously had to be caught by hand-mirroring `==` chains in
    /// `Cache::lookup` — the round-217 dedupe routes `Cache::lookup`
    /// through `CacheEntry::matches`, so any future change to the
    /// triple's definition only has to land in one place per type.
    pub fn matches(&self, path: &Path, mtime: i64, size: u64) -> bool {
        self.path == path && self.mtime_unix == mtime && self.size_bytes == size
    }
}

/// Top-level discovery entry point. Walks `paths`, probes every
/// `*.dll` / `*.ax` in each, consults / updates the on-disk
/// cache, and returns the merged list.
///
/// The cache lives at [`cache_file_path()`] — the platform
/// default cache location. Callers that need an explicit,
/// hermetic cache file (CI sandboxes, parallel test binaries,
/// embedders with their own state directory) should use
/// [`discover_with_cache`] instead; this function is exactly
/// `discover_with_cache(paths, &cache_file_path())`.
///
/// Hard contract: never panics, never returns an error type. A
/// single bad DLL is silently skipped with `log::debug!`. The
/// outer caller (`crate::register`) is therefore safe to chain
/// multiple `register` calls without a panic-aborting path.
pub fn discover(paths: &[PathBuf]) -> Vec<DiscoveryEntry> {
    discover_with_cache(paths, &cache_file_path())
}

/// [`discover`] with an explicit on-disk cache file.
///
/// Identical walk / probe / cache semantics to [`discover`], but
/// the JSON cache is read from and written to `cache_path`
/// instead of the platform-default [`cache_file_path()`]. This is
/// the hermetic entry point: a consumer that owns its state
/// directory (or a test that must not race the developer's real
/// `~/.cache/oxideav/vfw-discovery.json` across parallel test
/// binaries) passes its own path and gets a fully self-contained
/// discovery cycle — no environment-variable redirection
/// required.
///
/// `cache_path` names the cache **file**, not its directory.
/// Parent directories are created on the first save; a missing or
/// unreadable cache file starts the cycle from an empty cache
/// (identical to the corrupted-cache recovery contract on
/// [`Cache::load`]).
///
/// Hard contract: never panics, never returns an error type —
/// same as [`discover`].
pub fn discover_with_cache(paths: &[PathBuf], cache_path: &Path) -> Vec<DiscoveryEntry> {
    let mut cache = Cache::load(cache_path).unwrap_or_default();

    let mut out: Vec<DiscoveryEntry> = Vec::new();
    // Duplicate-root dedupe (round 401): a directory listed twice
    // (an `OXIDEAV_VFW_CODEC_PATH` with a repeated component — easy
    // to produce when a deploy script appends the same default dir
    // a config file already names) used to be walked twice. The
    // first pass probed and cached each DLL, the second pass
    // cache-hit the same rows, and every codec came back — and got
    // REGISTERED — twice. Walk each distinct root once, keeping
    // first-occurrence order. Comparison is by `Path` equality
    // (component-wise, so a trailing separator doesn't defeat it);
    // two different spellings of one directory via symlinks are
    // still walked twice — we deliberately don't canonicalise,
    // because that would also rewrite the paths users see in logs
    // and cache keys.
    let mut walked: Vec<&Path> = Vec::with_capacity(paths.len());
    // Roots this cycle successfully iterated + every candidate the
    // walk saw in them — the inputs to the vanished-row prune
    // below (round 401).
    let mut walked_ok: Vec<&Path> = Vec::with_capacity(paths.len());
    let mut seen: Vec<PathBuf> = Vec::new();
    for dir in paths {
        if walked.contains(&dir.as_path()) {
            log::debug!("vfw discovery: skipping duplicate root {:?}", dir);
            continue;
        }
        walked.push(dir.as_path());
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => {
                if !ALLOW_MISSING_DIR {
                    log::warn!("vfw discovery: cannot read {:?}", dir);
                }
                continue;
            }
        };
        walked_ok.push(dir.as_path());
        // Deterministic walk order (round 401): `fs::read_dir`
        // yields entries in filesystem-internal order (inode /
        // B-tree / hash order depending on the FS), which varies
        // between machines and even between runs after a file is
        // rewritten. Registration order — and therefore which DLL
        // wins when two codecs in the same directory claim the
        // same FourCC at equal priority — used to inherit that
        // nondeterminism. Sort candidates by path within each
        // directory so a given codec-dir layout always produces
        // the same DiscoveryEntry order, the same registration
        // order, and the same cache-row order. Directories
        // themselves are walked in caller order (the configured
        // path-list order is meaningful: earlier roots register
        // first).
        let mut candidates: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_codec_candidate(p))
            .collect();
        candidates.sort();
        // Every candidate the walk encountered counts as "seen"
        // for prune purposes — including ones whose stat fails
        // below (the file exists; a transient metadata error must
        // not evict its row).
        seen.extend(candidates.iter().cloned());
        for path in candidates {
            let (mtime, size) = match file_meta(&path) {
                Some(m) => m,
                None => continue,
            };
            // Cache hit?
            if let Some(cached) = cache.lookup(&path, mtime, size) {
                out.push(cached.clone());
                continue;
            }
            // Cache miss — probe.
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    log::debug!("vfw discovery: read {:?} failed: {e}", path);
                    continue;
                }
            };
            let probed = probe::probe_bytes(&bytes);
            let entry = DiscoveryEntry {
                path: path.clone(),
                mtime_unix: mtime,
                size_bytes: size,
                kind: probed.kind,
                fourccs: probed.fourccs,
                clsid: probed.clsid,
            };
            cache.upsert(entry.clone());
            out.push(entry);
        }
    }
    // Vanished-row prune (round 401): rows whose DLL was deleted
    // from a root we successfully walked this cycle are dead — the
    // file is gone, the row can never hit again, and before this
    // prune it lingered in the JSON forever (the cache only ever
    // grew). Rows under roots NOT walked this cycle are kept; see
    // `Cache::prune_vanished` for the exact scoping contract.
    let pruned = cache.prune_vanished(&walked_ok, &seen);
    if pruned > 0 {
        log::debug!("vfw discovery: pruned {pruned} vanished cache row(s)");
    }
    // Best-effort write — never let a cache I/O failure poison
    // discovery itself.  Round 204: only fire when something
    // changed (a cache miss re-probed a candidate, a vanished row
    // was pruned, or `load` consumed a legacy bare-array shape that
    // wants envelope-promotion).  Steady-state `register()` calls
    // against a fully-cached, stable codec directory still skip the
    // atomic rewrite entirely.
    if cache.is_dirty() {
        let _ = cache.save_atomic(cache_path);
    }
    out
}

/// Read the file's `(mtime_unix, size_bytes)` pair. Returns
/// `None` on stat failure — discovery treats those files as
/// unreadable and skips them silently.
fn file_meta(path: &Path) -> Option<(i64, u64)> {
    let md = fs::metadata(path).ok()?;
    let size = md.len();
    let mtime = md.modified().ok().and_then(systime_to_unix).unwrap_or(0);
    Some((mtime, size))
}

fn systime_to_unix(t: SystemTime) -> Option<i64> {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => Some(d.as_secs() as i64),
        Err(e) => Some(-(e.duration().as_secs() as i64)),
    }
}

/// True for `*.dll` / `*.ax`, case-insensitive.
fn is_codec_candidate(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => {
            let lower = ext.to_ascii_lowercase();
            lower == "dll" || lower == "ax"
        }
        None => false,
    }
}

/// Public entry — walks the configured discovery path, probes,
/// and registers each result into the codec registry.
///
/// Hard contract: never panics, never errors out. Always returns
/// the count of registered codecs (zero is fine — that's what
/// happens on a stock CI box that has no codecs to discover).
pub fn discover_and_register(ctx: &mut oxideav_core::RuntimeContext) -> usize {
    let paths = discovery_paths();
    let entries = discover(&paths);
    let mut registered = 0usize;
    let mut fourccs_seen: Vec<String> = Vec::new();
    for entry in entries {
        if matches!(entry.kind, Kind::Unsupported) {
            log::debug!("vfw discovery: {:?} is unsupported", entry.path);
            continue;
        }
        for fcc in &entry.fourccs {
            let codec_id_str = codec::codec_id_for(&entry.path, fcc);
            codec::register_factory_for_id(
                &codec_id_str,
                DiscoveryRecord {
                    dll_path: entry.path.clone(),
                    fourcc: fcc.clone(),
                    kind: entry.kind,
                    clsid: entry.clsid.clone(),
                },
            );
            codec::register_codec_info(ctx, &codec_id_str, fcc, entry.kind);
            fourccs_seen.push(fcc.clone());
            registered += 1;
        }
    }
    log::debug!(
        "vfw: discovered {} codecs from {:?}: {:?}",
        registered,
        paths,
        fourccs_seen
    );
    registered
}

#[cfg(test)]
pub(crate) mod test_tmpdir {
    //! Tiny zero-dep tempdir helper. We deliberately avoid
    //! taking a `tempfile` dev-dep just for these tests.
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub struct Tmp(pub PathBuf);

    impl Tmp {
        pub fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path =
                std::env::temp_dir().join(format!("oxideav-vfw-disc-{label}-{pid}-{nanos}-{n}"));
            std::fs::create_dir_all(&path).unwrap();
            Tmp(path)
        }
        pub fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_tmpdir::Tmp;
    use super::*;
    use std::io::Write;

    #[test]
    fn discovery_on_nonexistent_path_returns_empty_cleanly() {
        let entries = discover(&[PathBuf::from("/this/does/not/exist/anywhere")]);
        assert!(entries.is_empty());
    }

    #[test]
    fn discovery_on_empty_dir_returns_empty() {
        let tmp = Tmp::new("empty");
        let entries = discover(&[tmp.path().to_path_buf()]);
        assert!(entries.is_empty());
    }

    #[test]
    fn discovery_skips_non_pe_files() {
        let tmp = Tmp::new("nonpe");
        let bogus = tmp.path().join("garbage.dll");
        let mut f = fs::File::create(&bogus).unwrap();
        f.write_all(b"this is not a PE32 file").unwrap();
        let entries = discover(&[tmp.path().to_path_buf()]);
        // Probe runs but yields Unsupported; entry IS recorded
        // (so we don't re-probe next time) but with empty FourCCs.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, Kind::Unsupported);
        assert!(entries[0].fourccs.is_empty());
    }

    #[test]
    fn is_codec_candidate_matches_dll_and_ax() {
        let tmp = Tmp::new("ext");
        let dll = tmp.path().join("foo.DLL");
        let ax = tmp.path().join("bar.Ax");
        let txt = tmp.path().join("baz.txt");
        for p in [&dll, &ax, &txt] {
            fs::File::create(p).unwrap();
        }
        assert!(is_codec_candidate(&dll));
        assert!(is_codec_candidate(&ax));
        assert!(!is_codec_candidate(&txt));
    }

    // ── Round 401: discover_with_cache — hermetic cache-file entry ──

    #[test]
    fn discover_with_cache_writes_cache_at_given_path() {
        // The explicit-cache entry point must confine ALL cache
        // I/O to the path the caller supplied — the whole point of
        // the hermetic variant. One probe miss → dirty cache →
        // save lands exactly at `cache_path` (parent dirs created
        // on the way).
        let tmp = Tmp::new("withcache-write");
        let codec_dir = tmp.path().join("codecs");
        fs::create_dir_all(&codec_dir).unwrap();
        fs::write(codec_dir.join("synth.dll"), b"not a PE32 file").unwrap();
        let cache_path = tmp.path().join("state").join("disc.json");
        assert!(!cache_path.exists());

        let v1 = discover_with_cache(std::slice::from_ref(&codec_dir), &cache_path);
        assert_eq!(v1.len(), 1);
        assert_eq!(v1[0].kind, Kind::Unsupported);
        assert!(
            cache_path.is_file(),
            "cache written at the caller-supplied path (parent dir auto-created)",
        );

        // Second cycle against the same explicit cache: pure hit,
        // identical entries.
        let v2 = discover_with_cache(std::slice::from_ref(&codec_dir), &cache_path);
        assert_eq!(v2, v1, "second call hits the hermetic cache");
    }

    #[test]
    fn discover_with_cache_missing_cache_file_and_empty_dir_writes_nothing() {
        // Zero candidates + no pre-existing cache → the cache
        // stays clean and the no-op-save skip (round 204) must
        // hold for the explicit-path variant too: no file is
        // created at `cache_path`.
        let tmp = Tmp::new("withcache-noop");
        let codec_dir = tmp.path().join("codecs");
        fs::create_dir_all(&codec_dir).unwrap();
        let cache_path = tmp.path().join("disc.json");

        let v = discover_with_cache(std::slice::from_ref(&codec_dir), &cache_path);
        assert!(v.is_empty());
        assert!(
            !cache_path.exists(),
            "steady-state no-op cycle creates no cache file",
        );
    }

    #[test]
    fn discover_delegates_to_with_cache_shape() {
        // `discover` is documented as exactly
        // `discover_with_cache(paths, &cache_file_path())`. We
        // can't hermetically assert on the default cache path from
        // a unit test (it's the user's real cache), but we CAN pin
        // that both functions classify the same directory
        // identically — the walk/probe halves share one body.
        let tmp = Tmp::new("withcache-delegate");
        let codec_dir = tmp.path().join("codecs");
        fs::create_dir_all(&codec_dir).unwrap();
        fs::write(codec_dir.join("synth.dll"), b"not a PE32 file").unwrap();
        let cache_path = tmp.path().join("disc.json");

        let via_explicit = discover_with_cache(std::slice::from_ref(&codec_dir), &cache_path);
        let via_default = discover(&[codec_dir]);
        assert_eq!(via_explicit.len(), via_default.len());
        assert_eq!(via_explicit[0].kind, via_default[0].kind);
        assert_eq!(via_explicit[0].path, via_default[0].path);
    }

    // ── Round 401: deterministic walk order ─────────────────────

    #[test]
    fn discover_orders_candidates_by_path_within_directory() {
        // Creation order is deliberately shuffled relative to the
        // lexical order; the walk must return lexical (path) order
        // regardless of what `fs::read_dir` yields. This is the
        // determinism guarantee: same directory layout → same
        // DiscoveryEntry order → same registration order.
        let tmp = Tmp::new("det-order");
        let codec_dir = tmp.path().join("codecs");
        fs::create_dir_all(&codec_dir).unwrap();
        for name in ["cc.dll", "aa.dll", "bb.ax", "zz.txt"] {
            fs::write(codec_dir.join(name), b"not a PE32 file").unwrap();
        }
        let cache_path = tmp.path().join("disc.json");

        let v = discover_with_cache(std::slice::from_ref(&codec_dir), &cache_path);
        let names: Vec<_> = v
            .iter()
            .map(|e| e.path.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["aa.dll", "bb.ax", "cc.dll"],
            "candidates sorted by path; non-candidates (zz.txt) excluded",
        );

        // Second cycle (all cache hits) preserves the same order —
        // ordering must not depend on hit-vs-miss.
        let v2 = discover_with_cache(std::slice::from_ref(&codec_dir), &cache_path);
        assert_eq!(v2, v, "cache-hit cycle returns the identical ordered list");
    }

    #[test]
    fn discover_walks_directories_in_caller_order() {
        // Root order is meaningful (earlier roots register first, so
        // they win FourCC ties at equal priority) — only the files
        // WITHIN a root are sorted. Give root2 a lexically-smaller
        // file than root1's and confirm root1's entries still come
        // first.
        let tmp = Tmp::new("det-roots");
        let root1 = tmp.path().join("r1");
        let root2 = tmp.path().join("r2");
        fs::create_dir_all(&root1).unwrap();
        fs::create_dir_all(&root2).unwrap();
        fs::write(root1.join("zz.dll"), b"not a PE32 file").unwrap();
        fs::write(root2.join("aa.dll"), b"not a PE32 file").unwrap();
        let cache_path = tmp.path().join("disc.json");

        let v = discover_with_cache(&[root1.clone(), root2.clone()], &cache_path);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].path, root1.join("zz.dll"), "first root first");
        assert_eq!(v[1].path, root2.join("aa.dll"), "second root second");
    }

    // ── Round 401: duplicate-root dedupe ────────────────────────

    #[test]
    fn discover_walks_duplicate_root_once() {
        // The same directory listed twice must yield each codec
        // once, not twice — pre-r401 the second pass cache-hit the
        // same rows and double-registered every FourCC.
        let tmp = Tmp::new("dup-root");
        let codec_dir = tmp.path().join("codecs");
        fs::create_dir_all(&codec_dir).unwrap();
        fs::write(codec_dir.join("synth.dll"), b"not a PE32 file").unwrap();
        let cache_path = tmp.path().join("disc.json");

        let v = discover_with_cache(&[codec_dir.clone(), codec_dir.clone()], &cache_path);
        assert_eq!(v.len(), 1, "duplicate root walked once");
        assert_eq!(v[0].path, codec_dir.join("synth.dll"));
    }

    #[test]
    fn discover_dedupes_root_with_trailing_separator_spelling() {
        // `Path` equality is component-wise, so `/dir` and `/dir/`
        // are the same root. A path list that mixes the two
        // spellings must still walk once.
        let tmp = Tmp::new("dup-root-sep");
        let codec_dir = tmp.path().join("codecs");
        fs::create_dir_all(&codec_dir).unwrap();
        fs::write(codec_dir.join("synth.dll"), b"not a PE32 file").unwrap();
        let cache_path = tmp.path().join("disc.json");

        let mut with_sep = codec_dir.as_os_str().to_owned();
        with_sep.push(std::path::MAIN_SEPARATOR.to_string());
        let v = discover_with_cache(&[codec_dir.clone(), PathBuf::from(with_sep)], &cache_path);
        assert_eq!(v.len(), 1, "trailing-separator spelling deduped");
    }

    #[test]
    fn discover_distinct_roots_both_walked() {
        // Dedupe must not over-trigger: two genuinely different
        // directories both contribute their candidates.
        let tmp = Tmp::new("dup-root-distinct");
        let r1 = tmp.path().join("r1");
        let r2 = tmp.path().join("r2");
        fs::create_dir_all(&r1).unwrap();
        fs::create_dir_all(&r2).unwrap();
        fs::write(r1.join("a.dll"), b"not a PE32 file").unwrap();
        fs::write(r2.join("b.dll"), b"not a PE32 file").unwrap();
        let cache_path = tmp.path().join("disc.json");

        let v = discover_with_cache(&[r1, r2], &cache_path);
        assert_eq!(v.len(), 2, "distinct roots each walked");
    }

    // ── Round 401: vanished-row prune, end-to-end ───────────────

    #[test]
    fn deleted_dll_row_is_pruned_from_cache_on_next_discover() {
        // Probe a DLL, delete it, discover again: the second cycle
        // must return nothing AND rewrite the cache without the
        // dead row (pre-r401 the row lingered in the JSON forever).
        let tmp = Tmp::new("prune-e2e");
        let codec_dir = tmp.path().join("codecs");
        fs::create_dir_all(&codec_dir).unwrap();
        let dll = codec_dir.join("synth.dll");
        fs::write(&dll, b"not a PE32 file").unwrap();
        let cache_path = tmp.path().join("disc.json");

        let v1 = discover_with_cache(std::slice::from_ref(&codec_dir), &cache_path);
        assert_eq!(v1.len(), 1);
        let loaded = Cache::load(&cache_path).expect("cache written");
        assert_eq!(loaded.len(), 1);

        fs::remove_file(&dll).unwrap();
        let v2 = discover_with_cache(std::slice::from_ref(&codec_dir), &cache_path);
        assert!(v2.is_empty(), "deleted DLL yields no entries");
        let reloaded = Cache::load(&cache_path).expect("cache still parseable");
        assert!(
            reloaded.is_empty(),
            "dead row pruned from the on-disk cache",
        );
    }

    #[test]
    fn rows_under_roots_not_in_this_cycle_survive() {
        // Probe root A, then discover root B only — A's row must
        // survive in the cache (shared across path configurations)
        // and hit again when A comes back into the path list.
        let tmp = Tmp::new("prune-scope");
        let root_a = tmp.path().join("a");
        let root_b = tmp.path().join("b");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        fs::write(root_a.join("keep.dll"), b"not a PE32 file").unwrap();
        let cache_path = tmp.path().join("disc.json");

        let va = discover_with_cache(std::slice::from_ref(&root_a), &cache_path);
        assert_eq!(va.len(), 1);

        // Walk B only; A's row must not be pruned.
        let vb = discover_with_cache(std::slice::from_ref(&root_b), &cache_path);
        assert!(vb.is_empty());
        let loaded = Cache::load(&cache_path).expect("cache parseable");
        assert_eq!(loaded.len(), 1, "row under unwalked root A survives");

        // A returns to the path list: pure cache hit, same entry.
        let va2 = discover_with_cache(std::slice::from_ref(&root_a), &cache_path);
        assert_eq!(va2, va, "root A's row hits again after the B-only cycle");
    }

    #[test]
    fn unreadable_root_does_not_trigger_prune() {
        // A root that fails read_dir this cycle (deleted,
        // unmounted) must not evict its rows — only successfully
        // walked roots participate in the prune.
        let tmp = Tmp::new("prune-unreadable");
        let codec_dir = tmp.path().join("codecs");
        fs::create_dir_all(&codec_dir).unwrap();
        fs::write(codec_dir.join("synth.dll"), b"not a PE32 file").unwrap();
        let cache_path = tmp.path().join("disc.json");

        let v1 = discover_with_cache(std::slice::from_ref(&codec_dir), &cache_path);
        assert_eq!(v1.len(), 1);

        // Whole root vanishes (unmount-shaped, not file-delete-shaped).
        fs::remove_dir_all(&codec_dir).unwrap();
        let v2 = discover_with_cache(std::slice::from_ref(&codec_dir), &cache_path);
        assert!(v2.is_empty());
        let loaded = Cache::load(&cache_path).expect("cache parseable");
        assert_eq!(
            loaded.len(),
            1,
            "rows under an unreadable root survive the cycle",
        );
    }

    // ── Round 217: triple-equality contract for the staleness check ─

    fn sample_disc_entry() -> DiscoveryEntry {
        DiscoveryEntry {
            path: PathBuf::from("/abs/codec.dll"),
            mtime_unix: 1_700_000_000,
            size_bytes: 524_288,
            kind: probe::Kind::Vfw,
            fourccs: vec!["MP43".into()],
            clsid: None,
        }
    }

    #[test]
    fn discovery_entry_matches_returns_true_on_identical_triple() {
        // The base case the cache layer hits on every steady-state
        // `register()`: the exact `(path, mtime, size)` we stored
        // last time, no DLL was touched between calls.
        let e = sample_disc_entry();
        assert!(e.matches(Path::new("/abs/codec.dll"), 1_700_000_000, 524_288));
    }

    #[test]
    fn discovery_entry_matches_false_on_path_change() {
        // A different absolute path under the same DLL basename
        // (different codec dir, multiple discovery roots) is NOT
        // the same entry — the cache table is keyed by absolute path.
        let e = sample_disc_entry();
        assert!(!e.matches(Path::new("/elsewhere/codec.dll"), 1_700_000_000, 524_288));
    }

    #[test]
    fn discovery_entry_matches_false_on_mtime_change() {
        // mtime tick → cache miss. The DLL was rewritten between
        // calls; re-probe rather than honouring a stale entry.
        let e = sample_disc_entry();
        assert!(!e.matches(Path::new("/abs/codec.dll"), 1_700_000_001, 524_288));
    }

    #[test]
    fn discovery_entry_matches_false_on_size_change() {
        // Same path + mtime but the size advanced (rare: a tar
        // extract that races the clock can hit this) → still a miss.
        // The triple is "all three or nothing"; tolerating two of
        // three would let stale rows survive after a partial rewrite.
        let e = sample_disc_entry();
        assert!(!e.matches(Path::new("/abs/codec.dll"), 1_700_000_000, 524_289));
    }
}
