//! Round-401 — discovery-cache lifecycle, end-to-end through the
//! public surface.
//!
//! The round-401 unit tests pin each behaviour at the
//! `discover_with_cache` / `Cache` layer; this binary drives the
//! same contracts through the crate's outermost entry points
//! (`oxideav_vfw::register` and `discovery::discover`), with the
//! cache redirected via the new `OXIDEAV_VFW_CACHE_PATH` override:
//!
//! 1. `register()` honours `OXIDEAV_VFW_CACHE_PATH` — the discovery
//!    cache lands at exactly the file named by the env var, nowhere
//!    else in the redirected cache root.
//! 2. A corrupted override-cache file is healed by a `register()`
//!    call even when the codec directory registers nothing
//!    (zero-probe heal, end-to-end).
//! 3. Deleting a DLL prunes its row from the override cache on the
//!    next `discover()` cycle.
//! 4. A duplicated codec-path component (same dir listed twice)
//!    yields each candidate once.
//!
//! Env-var mutations are process-global; every test takes the
//! shared lock and restores via RAII guards, following the
//! round-189 / round-197 sibling binaries.
//!
//! ## Wall — clean-room sourcing
//!
//! No external library source consulted. All behaviour anchored on
//! `oxideav-vfw`'s own module documentation and the round-28..401
//! test patterns in this repository.

#![cfg(feature = "auto-discovery")]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use oxideav_vfw::discovery;

/// Tiny zero-dep tempdir helper, same shape as the sibling
/// binaries.
struct Tmp(PathBuf);

impl Tmp {
    fn new(label: &str) -> Self {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = env::temp_dir().join(format!("vfw-r401-{label}-{pid}-{nanos}"));
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

/// RAII guard: snapshot a process env var on construction and
/// restore it on drop.
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

/// Process-global serialiser for tests that mutate env vars —
/// `cargo test` runs tests in threads and env vars are
/// process-global.
fn env_serial_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[test]
fn register_honours_cache_path_override() {
    let tmp = Tmp::new("reg-override");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    fs::write(codec_dir.join("synth.dll"), b"not a PE32 file").unwrap();
    let cache_root = tmp.path().join("state");
    fs::create_dir_all(&cache_root).unwrap();
    let cache_file = cache_root.join("override-cache.json");

    let _serial = env_serial_lock();
    let _codec_env = EnvGuard::set("OXIDEAV_VFW_CODEC_PATH", &codec_dir);
    let _cache_env = EnvGuard::set("OXIDEAV_VFW_CACHE_PATH", &cache_file);

    let mut ctx = oxideav_core::RuntimeContext::new();
    oxideav_vfw::register(&mut ctx);

    // The probe (an Unsupported garbage DLL) dirties the cache, so
    // register()'s discovery cycle must have written EXACTLY the
    // override file.
    assert!(
        cache_file.is_file(),
        "register() wrote the cache at the OXIDEAV_VFW_CACHE_PATH file",
    );
    let names: Vec<_> = fs::read_dir(&cache_root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["override-cache.json".to_string()],
        "no other files (tempfiles, default-named caches) in the state dir",
    );

    // The written file is the versioned envelope holding the one
    // probed row.
    let parsed: serde_json::Value =
        serde_json::from_slice(&fs::read(&cache_file).unwrap()).unwrap();
    assert_eq!(
        parsed.get("version").and_then(|v| v.as_u64()),
        Some(discovery::CURRENT_SCHEMA_VERSION as u64),
    );
    assert_eq!(
        parsed
            .get("entries")
            .and_then(|e| e.as_array())
            .map(Vec::len),
        Some(1),
    );
}

#[test]
fn register_heals_corrupt_override_cache_with_empty_codec_dir() {
    // Zero-probe heal, end-to-end: an EMPTY codec directory plus a
    // corrupt override cache. Pre-r401 nothing dirtied the cache,
    // so the corrupt file survived; now register() rewrites it as
    // a valid empty envelope.
    let tmp = Tmp::new("reg-heal");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    let cache_file = tmp.path().join("corrupt-cache.json");
    fs::write(&cache_file, b"]]]{{{ definitely not json").unwrap();

    let _serial = env_serial_lock();
    let _codec_env = EnvGuard::set("OXIDEAV_VFW_CODEC_PATH", &codec_dir);
    let _cache_env = EnvGuard::set("OXIDEAV_VFW_CACHE_PATH", &cache_file);

    let mut ctx = oxideav_core::RuntimeContext::new();
    oxideav_vfw::register(&mut ctx);

    let parsed: serde_json::Value = serde_json::from_slice(&fs::read(&cache_file).unwrap())
        .expect("register() healed the corrupt cache to valid JSON");
    assert_eq!(
        parsed.get("version").and_then(|v| v.as_u64()),
        Some(discovery::CURRENT_SCHEMA_VERSION as u64),
    );
    assert_eq!(
        parsed
            .get("entries")
            .and_then(|e| e.as_array())
            .map(Vec::len),
        Some(0),
        "nothing was discovered; the healed envelope is empty",
    );
}

#[test]
fn deleted_dll_pruned_through_public_discover() {
    // discover() (the default-cache entry point) with the cache
    // redirected through the override var: probe → delete → next
    // cycle prunes the dead row from the on-disk JSON.
    let tmp = Tmp::new("prune-public");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    let dll = codec_dir.join("synth.dll");
    fs::write(&dll, b"not a PE32 file").unwrap();
    let cache_file = tmp.path().join("cache.json");

    let _serial = env_serial_lock();
    let _cache_env = EnvGuard::set("OXIDEAV_VFW_CACHE_PATH", &cache_file);

    let v1 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v1.len(), 1);

    fs::remove_file(&dll).unwrap();
    let v2 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert!(v2.is_empty());

    let parsed: serde_json::Value =
        serde_json::from_slice(&fs::read(&cache_file).unwrap()).unwrap();
    assert_eq!(
        parsed
            .get("entries")
            .and_then(|e| e.as_array())
            .map(Vec::len),
        Some(0),
        "the deleted DLL's row was pruned from the on-disk cache",
    );
}

#[test]
fn duplicated_codec_path_component_yields_each_candidate_once() {
    // OXIDEAV_VFW_CODEC_PATH with the same directory twice: the
    // path-list dedupe must collapse it end-to-end through
    // discovery_paths() + discover().
    let tmp = Tmp::new("dup-component");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    fs::write(codec_dir.join("synth.dll"), b"not a PE32 file").unwrap();
    let cache_file = tmp.path().join("cache.json");

    let sep = if cfg!(windows) { ";" } else { ":" };
    let doubled = format!("{0}{sep}{0}", codec_dir.display());

    let _serial = env_serial_lock();
    let _codec_env = EnvGuard::set("OXIDEAV_VFW_CODEC_PATH", &doubled);
    let _cache_env = EnvGuard::set("OXIDEAV_VFW_CACHE_PATH", &cache_file);

    let paths = discovery::discovery_paths();
    assert_eq!(
        paths.len(),
        2,
        "path parsing itself keeps both components (dedupe happens at walk time)",
    );
    let entries = discovery::discover(&paths);
    assert_eq!(entries.len(), 1, "duplicate root walked once");
}
