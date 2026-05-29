//! Round-189 — corrupted on-disk cache recovery.
//!
//! Locks in the hard contract documented on
//! [`oxideav_vfw::discovery::Cache::load`] (and reiterated on
//! [`oxideav_vfw::discovery`] module docs):
//!
//! > *A corrupted cache is treated as empty rather than poisoning
//! > `register()`.*
//!
//! The existing round-28 unit test
//! `cache::tests::load_corrupted_file_returns_none` only exercises
//! `Cache::load` in isolation. This integration test wires the
//! end-to-end behaviour the contract actually promises:
//!
//! 1. Scribble garbage onto the on-disk cache JSON.
//! 2. Call `discovery::discover()` against an active codec dir
//!    that holds one synthetic `*.dll`.
//! 3. Verify discovery does **not** panic, re-probes the candidate
//!    cleanly (recording it as [`Kind::Unsupported`] since it's
//!    not a real PE32), and **overwrites** the corrupted cache
//!    with a parseable JSON document on the way out.
//! 4. A subsequent `discover()` call must hit the now-healed cache
//!    rather than re-probing again.
//!
//! Cache file location is redirected via `XDG_CACHE_HOME` (UNIX)
//! / `LOCALAPPDATA` (Windows) so the test stays sandboxed inside a
//! per-test tempdir and never touches the dev box's real cache at
//! `~/.cache/oxideav/vfw-discovery.json`.
//!
//! ## Wall — clean-room sourcing
//!
//! No external library source consulted. All behaviour anchored on
//! `oxideav-vfw`'s own module-level documentation (the comments in
//! `src/discovery/{mod,cache,paths}.rs`) and on the existing
//! round-28 test patterns.

#![cfg(feature = "auto-discovery")]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use oxideav_vfw::discovery;

/// Tiny zero-dep tempdir helper. Sibling tests use the same shape
/// to avoid a `tempfile` dev-dep.
struct Tmp(PathBuf);

impl Tmp {
    fn new(label: &str) -> Self {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = env::temp_dir().join(format!("vfw-r189-{label}-{pid}-{nanos}"));
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
/// restore it on drop. Keeps the test's env mutations from leaking
/// into sibling tests in the same binary.
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

/// Resolve the platform cache-dir env-var name that the discovery
/// layer honours when picking
/// `<cache>/oxideav/vfw-discovery.json`. UNIX uses
/// `XDG_CACHE_HOME`; Windows uses `LOCALAPPDATA`.
fn cache_dir_env_var() -> &'static str {
    if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "XDG_CACHE_HOME"
    }
}

/// Compute the exact cache file path the discovery layer will use
/// when `cache_dir_env_var()` is set to `root`. Mirrors
/// `paths::platform_cache_dir` + `paths::cache_file_path`.
fn expected_cache_file(root: &Path) -> PathBuf {
    if cfg!(windows) {
        // LOCALAPPDATA → <root>/oxideav/Cache/vfw-discovery.json
        root.join("oxideav")
            .join("Cache")
            .join("vfw-discovery.json")
    } else {
        // XDG_CACHE_HOME → <root>/oxideav/vfw-discovery.json
        root.join("oxideav").join("vfw-discovery.json")
    }
}

#[test]
fn corrupted_cache_is_treated_as_empty_and_overwritten() {
    let tmp = Tmp::new("corrupt-cache");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();

    // One synthetic non-PE candidate so probe lands on
    // Unsupported. We deliberately don't bring in a real PE here
    // (round-28 already covers the real-PE path); the goal is
    // strictly to verify the cache recovery shape.
    let dll = codec_dir.join("synth.dll");
    fs::write(&dll, b"this is not a real PE32 file").unwrap();

    // Redirect the cache directory into the per-test tempdir
    // BEFORE pre-populating the corrupted JSON, so we know the
    // path our scribble lands on is the same one discovery will
    // read.
    let cache_root = tmp.path().join("cache-root");
    fs::create_dir_all(&cache_root).unwrap();
    let _cache_env = EnvGuard::set(cache_dir_env_var(), &cache_root);
    let cache_file = expected_cache_file(&cache_root);
    fs::create_dir_all(cache_file.parent().unwrap()).unwrap();

    // Drop a known-malformed JSON document at the exact path
    // discovery will read.
    let garbage = b"{this is definitely not valid: json,,,";
    fs::write(&cache_file, garbage).unwrap();
    let pre = fs::read(&cache_file).unwrap();
    assert_eq!(
        pre.as_slice(),
        garbage,
        "pre-condition: corrupted cache landed on disk verbatim",
    );

    // First discover() call:
    //   - Cache::load returns None (corrupted JSON → parse fails).
    //   - Discovery re-probes the candidate cleanly.
    //   - The atomic-rename write path overwrites the corrupted
    //     file with a parseable JSON array.
    let v1 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(
        v1.len(),
        1,
        "discovery recovered from corrupted cache and probed the synthetic DLL",
    );
    assert_eq!(v1[0].kind, discovery::Kind::Unsupported);
    assert!(v1[0].fourccs.is_empty());

    // Cache file is now non-empty and parseable as JSON.
    let post = fs::read(&cache_file).unwrap_or_default();
    assert!(
        !post.is_empty(),
        "discovery wrote a fresh cache after recovering from corruption",
    );
    assert_ne!(
        post, garbage,
        "fresh cache content differs from the original garbage",
    );
    let parsed: serde_json::Value = serde_json::from_slice(&post)
        .expect("overwritten cache must parse as JSON (atomic write produced valid JSON)");
    assert!(
        parsed.is_array(),
        "schema: top-level cache document is a JSON array of CacheEntry",
    );
    assert_eq!(
        parsed.as_array().map(|a| a.len()),
        Some(1),
        "exactly one entry written for the one synthetic DLL",
    );

    // Second discover() call:
    //   - The healed cache hits cleanly; result matches v1
    //     verbatim.
    let v2 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(
        v2, v1,
        "second call sees the healed cache as a hit, not a re-probe",
    );
}

#[test]
fn empty_cache_file_is_treated_as_empty_not_a_panic() {
    // Closely-related edge case: a zero-byte cache file (e.g.
    // a previous atomic write was interrupted between create()
    // and write_all). The hard contract says any I/O / parse
    // failure on Cache::load yields an empty cache; an empty
    // file deserialises into a serde_json parse error.
    let tmp = Tmp::new("empty-cache");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    let dll = codec_dir.join("synth.dll");
    fs::write(&dll, b"still not a PE").unwrap();

    let cache_root = tmp.path().join("cache-root");
    fs::create_dir_all(&cache_root).unwrap();
    let _cache_env = EnvGuard::set(cache_dir_env_var(), &cache_root);
    let cache_file = expected_cache_file(&cache_root);
    fs::create_dir_all(cache_file.parent().unwrap()).unwrap();

    // Zero-byte cache file.
    fs::File::create(&cache_file).unwrap();
    assert_eq!(fs::metadata(&cache_file).unwrap().len(), 0);

    // Must not panic, must still probe.
    let entries = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, discovery::Kind::Unsupported);

    // Cache rewritten in valid form.
    let after = fs::read(&cache_file).unwrap_or_default();
    assert!(
        !after.is_empty(),
        "empty cache was overwritten with content"
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&after).expect("post-recovery cache parses cleanly");
    assert!(parsed.is_array());
}
