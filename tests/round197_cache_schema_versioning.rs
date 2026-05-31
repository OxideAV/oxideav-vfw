//! Round-197 — on-disk cache schema versioning.
//!
//! Round 189 locked in the corruption-recovery contract on
//! [`oxideav_vfw::discovery::Cache::load`]: a malformed cache file
//! never poisons `register()`. Round 197 extends that contract to
//! cover **schema mismatch** alongside structural corruption — a
//! cache file written by a different crate version (newer or
//! older) is treated as corruption from this reader's perspective
//! and the next save heals it.
//!
//! Three end-to-end scenarios:
//!
//! 1. **Legacy bare-array seamless upgrade.** A pre-r197 cache file
//!    (top-level JSON array, no version envelope) is loadable on
//!    first call; the same call's atomic-write tail promotes the
//!    on-disk shape to the round-197 versioned envelope
//!    `{"version": N, "entries": [...]}`.
//! 2. **Future-version refusal.** A cache file stamped with a
//!    version higher than [`oxideav_vfw::discovery::Cache`] knows
//!    (simulating a downgrade or a forward-incompatible crate
//!    upgrade) is treated as corruption: re-probe runs, the file
//!    is overwritten with the current version.
//! 3. **Round-trip stability.** Calling `discover()` twice in a row
//!    against the same dir must hit the healed cache the second
//!    time (no re-probe), regardless of the original on-disk
//!    shape.
//!
//! The cache file location is redirected via `XDG_CACHE_HOME`
//! (UNIX) / `LOCALAPPDATA` (Windows) so the test never reaches the
//! dev box's real cache.
//!
//! ## Wall — clean-room sourcing
//!
//! No external library source consulted. All behaviour anchored on
//! `oxideav-vfw`'s own `discovery::cache` module + the round-189
//! integration-test patterns (`tests/round189_corrupted_cache_recovery.rs`).

#![cfg(feature = "auto-discovery")]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use oxideav_vfw::discovery;

/// Zero-dep tempdir helper — mirrors the round-189 shape verbatim
/// so the two tests stay parallel.
struct Tmp(PathBuf);

impl Tmp {
    fn new(label: &str) -> Self {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = env::temp_dir().join(format!("vfw-r197-{label}-{pid}-{nanos}"));
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

/// Process-global serialiser for any test in this binary that
/// mutates a shared env var (`XDG_CACHE_HOME` / `LOCALAPPDATA`).
/// Parallel test execution is the cargo default and the env var is
/// process-global, so without this lock two tests in the same
/// binary can race each other into seeing one another's cache
/// directory. Pre-existing same-shape race in the round-189
/// integration tests stayed latent because that binary only had
/// two tests that happened not to collide on timing.
fn cache_env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
    // Poisoned-mutex tolerance: an earlier test panicked while
    // holding the lock; subsequent tests still need the env-var
    // serialisation guarantee.
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

#[test]
fn legacy_bare_array_cache_is_loaded_then_promoted_to_envelope() {
    let tmp = Tmp::new("legacy-upgrade");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();

    // One synthetic non-PE candidate — same shape round-189 uses.
    let dll = codec_dir.join("synth.dll");
    fs::write(&dll, b"this is not a real PE32 file").unwrap();

    let cache_root = tmp.path().join("cache-root");
    fs::create_dir_all(&cache_root).unwrap();
    let _serial = cache_env_lock();
    let _cache_env = EnvGuard::set(cache_dir_env_var(), &cache_root);
    let cache_file = expected_cache_file(&cache_root);
    fs::create_dir_all(cache_file.parent().unwrap()).unwrap();

    // Pre-seed with a legacy bare-array cache that ALREADY claims
    // to know about synth.dll. Use the synthetic file's real mtime
    // / size so `lookup` accepts the cached row as fresh — this
    // proves discovery honoured the legacy shape (no re-probe).
    let meta = fs::metadata(&dll).unwrap();
    let mtime = meta
        .modified()
        .unwrap()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
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

    let v1 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v1.len(), 1, "discovery sees the synthetic candidate");
    // Cache hit on a legacy row means we got the cached `Kind::Vfw`
    // back — NOT a fresh probe that would have classified synth.dll
    // as `Kind::Unsupported`.
    assert_eq!(
        v1[0].kind,
        discovery::Kind::Vfw,
        "legacy bare-array cache row was honoured (no spurious re-probe)",
    );
    assert_eq!(v1[0].fourccs, vec!["MP43".to_string()]);

    // The atomic save tail promoted the file to the versioned
    // envelope — verify on-disk shape.
    let post = fs::read(&cache_file).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&post).unwrap();
    assert!(
        parsed.is_object(),
        "post-save: cache file is the versioned envelope, not the legacy bare array",
    );
    let version = parsed
        .get("version")
        .and_then(|v| v.as_u64())
        .expect("envelope carries a `version` field");
    assert_eq!(version, discovery::CURRENT_SCHEMA_VERSION as u64);
    assert!(
        parsed.get("entries").map(|e| e.is_array()).unwrap_or(false),
        "envelope carries an `entries` array",
    );
}

#[test]
fn future_version_cache_is_treated_as_corruption_and_healed() {
    let tmp = Tmp::new("future-version");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();

    let dll = codec_dir.join("synth.dll");
    fs::write(&dll, b"still not a PE32 file").unwrap();

    let cache_root = tmp.path().join("cache-root");
    fs::create_dir_all(&cache_root).unwrap();
    let _serial = cache_env_lock();
    let _cache_env = EnvGuard::set(cache_dir_env_var(), &cache_root);
    let cache_file = expected_cache_file(&cache_root);
    fs::create_dir_all(cache_file.parent().unwrap()).unwrap();

    // Scribble a future-versioned envelope onto disk. Use a
    // version far above the current so we don't have to bump this
    // literal every time the schema moves forward.
    let future = serde_json::json!({
        "version": discovery::CURRENT_SCHEMA_VERSION + 99,
        "entries": [{
            "path": dll,
            "mtime_unix": 1,
            "size_bytes": 1,
            "kind": "vfw",
            "fourccs": ["MP43"],
            "clsid": null,
            "handshake": null,
        }],
    });
    fs::write(&cache_file, serde_json::to_vec_pretty(&future).unwrap()).unwrap();

    // Discovery must NOT trust the future-version row (which
    // claims MP43 against a non-PE) and must NOT panic.
    let v1 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v1.len(), 1);
    assert_eq!(
        v1[0].kind,
        discovery::Kind::Unsupported,
        "future-version cache was discarded; re-probe correctly classified \
         the synthetic non-PE as Unsupported",
    );

    // File is now overwritten at OUR version.
    let post = fs::read(&cache_file).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&post).unwrap();
    let healed_version = parsed
        .get("version")
        .and_then(|v| v.as_u64())
        .expect("healed envelope carries a `version` field");
    assert_eq!(healed_version, discovery::CURRENT_SCHEMA_VERSION as u64);

    // Second call hits the healed cache cleanly.
    let v2 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v2, v1, "second call hits the healed envelope as cache hit");
}

#[test]
fn older_envelope_version_is_also_treated_as_corruption() {
    // The downgrade-and-re-upgrade path: a hypothetical v0
    // envelope from an old beta should also be discarded + healed.
    // We don't have a real v0 file in the wild (the very first
    // versioned write was v1), so this is a forward-looking guard
    // against ever shipping a v < CURRENT writer.
    let tmp = Tmp::new("older-version");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    let dll = codec_dir.join("synth.dll");
    fs::write(&dll, b"non-PE").unwrap();

    let cache_root = tmp.path().join("cache-root");
    fs::create_dir_all(&cache_root).unwrap();
    let _serial = cache_env_lock();
    let _cache_env = EnvGuard::set(cache_dir_env_var(), &cache_root);
    let cache_file = expected_cache_file(&cache_root);
    fs::create_dir_all(cache_file.parent().unwrap()).unwrap();

    let older = serde_json::json!({ "version": 0, "entries": [] });
    fs::write(&cache_file, serde_json::to_vec(&older).unwrap()).unwrap();

    let v = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].kind, discovery::Kind::Unsupported);

    let post = fs::read(&cache_file).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&post).unwrap();
    assert_eq!(
        parsed.get("version").and_then(|v| v.as_u64()),
        Some(discovery::CURRENT_SCHEMA_VERSION as u64),
        "older-version envelope was discarded; healed file uses current version",
    );
}
