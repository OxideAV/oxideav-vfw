//! Round-204 — steady-state `discover()` skips the no-op cache rewrite.
//!
//! Through round 197 every successful `discover()` call ended with
//! an unconditional `cache.save_atomic(...)`, even on the
//! steady-state hot path where the in-memory cache and the on-disk
//! file already agreed byte-for-byte. That's one pretty-printed
//! `vfw-discovery.json` rewrite per `register()` call on a stable
//! codec directory — wasted bytes, wasted fsyncs, and on a
//! cross-mount cache-dir (NFS, tmpfs-backed `XDG_CACHE_HOME`) a
//! visible spike of inode churn at startup.
//!
//! Round 204 adds an interior `dirty` flag to
//! [`oxideav_vfw::discovery::Cache`]:
//!
//! - [`Cache::upsert`] sets it (every cache-miss re-probe).
//! - [`Cache::load`] sets it ONLY for the legacy bare-array shape
//!   (whose on-disk representation differs from what we'd write
//!   now) — a versioned envelope load starts clean.
//! - [`Cache::save_atomic`] clears it on success.
//!
//! `discover()` now skips `save_atomic` when `cache.is_dirty()` is
//! false. The two contracts this test pins:
//!
//! 1. **Steady-state no-op-skip.** Two `discover()` calls in a row
//!    against the same codec directory: the first must heal the
//!    cache (legacy → envelope or first-probe → envelope); the
//!    second must NOT rewrite the file. We measure file mtime
//!    before/after the second call to prove the on-disk byte
//!    stream is untouched.
//! 2. **Legacy-promotion still fires.** A pre-r197 bare-array cache
//!    file is correctly promoted to the versioned envelope on the
//!    first `discover()` call even when no candidate DLL needs
//!    re-probing. Without the load-time dirty-flag, the
//!    no-op-skip would leave the legacy file on disk forever.
//!
//! ## Wall — clean-room sourcing
//!
//! No external library source consulted. All behaviour anchored on
//! `oxideav-vfw`'s own `discovery::cache` module + the round-197
//! integration-test patterns (`tests/round197_cache_schema_versioning.rs`)
//! whose env-guard / tempdir / `expected_cache_file` helpers are
//! mirrored here verbatim so the two tests stay parallel.

#![cfg(feature = "auto-discovery")]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oxideav_vfw::discovery;

/// Zero-dep tempdir helper — same shape as round 197.
struct Tmp(PathBuf);

impl Tmp {
    fn new(label: &str) -> Self {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = env::temp_dir().join(format!("vfw-r204-{label}-{pid}-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// RAII guard — saves an env var on construction, restores on drop.
struct EnvGuard {
    key: &'static str,
    saved: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let saved = env::var_os(key);
        env::set_var(key, value);
        EnvGuard { key, saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.saved.take() {
            Some(v) => env::set_var(self.key, v),
            None => env::remove_var(self.key),
        }
    }
}

/// Process-global serialiser — same shape as round 197.  Without
/// this lock two tests in this binary that both override
/// `XDG_CACHE_HOME` / `LOCALAPPDATA` would race each other into
/// seeing one another's cache directory.
fn cache_env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn cache_dir_env_var() -> &'static str {
    if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "XDG_CACHE_HOME"
    }
}

fn expected_cache_file(root: &Path) -> PathBuf {
    if cfg!(windows) {
        root.join("oxideav")
            .join("Cache")
            .join("vfw-discovery.json")
    } else {
        root.join("oxideav").join("vfw-discovery.json")
    }
}

/// Read the file's last-modification time as nanoseconds since
/// UNIX_EPOCH. Returns `0` on stat failure — never panics. We use
/// nanos rather than seconds so the test catches single-call
/// rewrites even on the same wall-second.
fn mtime_nanos(p: &Path) -> u128 {
    fs::metadata(p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[test]
fn steady_state_discover_does_not_rewrite_cache_file() {
    // First call: codec dir has one synthetic non-PE candidate, no
    // pre-existing cache file. `discover()` probes, classifies as
    // Unsupported, writes the envelope.
    //
    // Second call: same codec dir, same DLL, cache file in place.
    // `discover()` must hit the cached row and MUST NOT rewrite the
    // file — we assert by sampling mtime before and after.
    let tmp = Tmp::new("noop-skip");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    let dll = codec_dir.join("synth.dll");
    fs::write(&dll, b"not a PE32 file").unwrap();

    let cache_root = tmp.path().join("cache-root");
    fs::create_dir_all(&cache_root).unwrap();
    let _serial = cache_env_lock();
    let _cache_env = EnvGuard::set(cache_dir_env_var(), &cache_root);
    let cache_file = expected_cache_file(&cache_root);

    // First call: should produce the cache file.
    let v1 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v1.len(), 1);
    assert_eq!(v1[0].kind, discovery::Kind::Unsupported);
    assert!(
        cache_file.exists(),
        "first discover writes the envelope file",
    );

    let mtime_before = mtime_nanos(&cache_file);
    let raw_before = fs::read(&cache_file).expect("envelope readable");

    // Tiny sleep so a hypothetical second write WOULD bump mtime —
    // catches OSes that bucket file-mtime updates at coarse
    // granularity (some macOS HFS+ vintage rounds to 1s; APFS is
    // fine-grained but we belt-and-braces anyway).
    std::thread::sleep(Duration::from_millis(20));

    // Second call: nothing changed on disk, the cache row is fresh,
    // so `discover()` MUST skip the rewrite.
    let v2 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v2, v1, "second call returns same entries (cache hit)");

    let mtime_after = mtime_nanos(&cache_file);
    let raw_after = fs::read(&cache_file).expect("envelope still readable");

    assert_eq!(
        mtime_before, mtime_after,
        "round-204 no-op-skip: steady-state discover() did not rewrite the cache file",
    );
    assert_eq!(
        raw_before, raw_after,
        "round-204 no-op-skip: file bytes unchanged across two discover() calls",
    );
}

#[test]
fn legacy_bare_array_still_promoted_under_noop_skip() {
    // The dirty flag on legacy-load is load-bearing here. Without
    // it, the no-op-skip would see "no upserts happened (cache hit
    // on every candidate)" and leave the file in the legacy shape.
    // We pre-seed a bare-array cache covering the codec dir, then
    // verify the file is rewritten as the versioned envelope on the
    // first `discover()` call — i.e. the legacy-load DID mark the
    // cache dirty even though no row was upserted.
    let tmp = Tmp::new("legacy-promote-under-skip");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    let dll = codec_dir.join("synth.dll");
    fs::write(&dll, b"not a PE32 file").unwrap();

    let cache_root = tmp.path().join("cache-root");
    fs::create_dir_all(&cache_root).unwrap();
    let _serial = cache_env_lock();
    let _cache_env = EnvGuard::set(cache_dir_env_var(), &cache_root);
    let cache_file = expected_cache_file(&cache_root);
    fs::create_dir_all(cache_file.parent().unwrap()).unwrap();

    // Pre-seed the legacy bare-array shape covering the synthetic
    // dll. Use the file's real mtime + size so lookup hits.
    let meta = fs::metadata(&dll).unwrap();
    let mtime = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let size = meta.len();
    let legacy = serde_json::json!([{
        "path": dll,
        "mtime_unix": mtime,
        "size_bytes": size,
        "kind": "vfw",
        "fourccs": ["MP43"],
        "clsid": null,
        "handshake": null,
    }]);
    fs::write(&cache_file, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let v = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v.len(), 1);
    // Cache hit on the legacy row → MP43 classification preserved.
    // (If the legacy row was discarded the synthetic non-PE would
    // come back as Unsupported.)
    assert_eq!(v[0].kind, discovery::Kind::Vfw);
    assert_eq!(v[0].fourccs, vec!["MP43".to_string()]);

    // File now in envelope shape — proves the load-time dirty flag
    // fires the save through the no-op-skip gate.
    let post = fs::read(&cache_file).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&post).unwrap();
    assert!(
        parsed.is_object(),
        "post-call: file is envelope (legacy bare-array was promoted)",
    );
    assert_eq!(
        parsed.get("version").and_then(|v| v.as_u64()),
        Some(discovery::CURRENT_SCHEMA_VERSION as u64),
        "promoted file stamps the current schema version",
    );
}

#[test]
fn cache_miss_still_triggers_save_under_noop_skip() {
    // First call: probe + save (cache miss → upsert → dirty → save).
    // Second call: introduce a new dll → cache miss again → upsert
    // → dirty → save. The mtime MUST change on the second call
    // because a new row landed.  This is the symmetric guard
    // against an overly aggressive no-op-skip that would also skip
    // legitimate writes.
    let tmp = Tmp::new("miss-fires-save");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    let dll_a = codec_dir.join("a.dll");
    fs::write(&dll_a, b"not a PE32 - first").unwrap();

    let cache_root = tmp.path().join("cache-root");
    fs::create_dir_all(&cache_root).unwrap();
    let _serial = cache_env_lock();
    let _cache_env = EnvGuard::set(cache_dir_env_var(), &cache_root);
    let cache_file = expected_cache_file(&cache_root);

    let v1 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v1.len(), 1);
    assert!(cache_file.exists());
    let mtime_after_first = mtime_nanos(&cache_file);

    std::thread::sleep(Duration::from_millis(20));

    // Drop a second dll into the directory — second `discover()`
    // will see it as a cache miss and must persist the new row.
    let dll_b = codec_dir.join("b.dll");
    fs::write(&dll_b, b"not a PE32 - second").unwrap();

    let v2 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v2.len(), 2, "second pass sees both candidates");

    let mtime_after_second = mtime_nanos(&cache_file);
    assert!(
        mtime_after_second > mtime_after_first,
        "cache miss → upsert → dirty → save: mtime must advance \
         ({mtime_after_first} → {mtime_after_second})",
    );
}
