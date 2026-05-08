//! Round-28 — auto-discovery integration tests.
//!
//! Exercises the public surface added by round 28:
//!
//! 1. `register()` against a default discovery path that doesn't
//!    exist (or that points at `/dev/null:/tmp/nonexistent`)
//!    succeeds without panicking and registers zero codecs.
//! 2. The cache layer round-trips correctly: a synthetic
//!    `DiscoveryEntry` written to disk reads back identically.
//! 3. Cache invalidation: changing a file's mtime forces a
//!    re-probe instead of a stale hit.
//! 4. Probe of `pe::test_image::build_minimal_dll()` lands on
//!    `Kind::Unsupported` (the synthetic DLL has neither
//!    `DriverProc` nor `DllGetClassObject`).
//!
//! Per round-28 spec, the tests stay self-contained — no
//! reliance on the wmpcdcs8-2001 fixture corpus.

#![cfg(feature = "auto-discovery")]

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use oxideav_core::RuntimeContext;
use oxideav_vfw::discovery;
use oxideav_vfw::pe::test_image::build_minimal_dll;

/// Tiny zero-dep tempdir helper. Sibling tests bring in `ureq`
/// for HTTPS fetches; we deliberately avoid a `tempfile` dev-dep
/// just for these four tests.
struct Tmp(PathBuf);

impl Tmp {
    fn new(label: &str) -> Self {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = env::temp_dir().join(format!("vfw-r28-{label}-{pid}-{nanos}"));
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

#[test]
fn register_against_nonexistent_default_path_clean() {
    // Force OXIDEAV_VFW_CODEC_PATH to a known-bogus value so we
    // don't accidentally hit a real codec dir on the dev box.
    let tmp = Tmp::new("default-empty");
    let saved = env::var_os("OXIDEAV_VFW_CODEC_PATH");
    let bogus = tmp.path().join("does-not-exist");
    env::set_var("OXIDEAV_VFW_CODEC_PATH", &bogus);

    let mut ctx = RuntimeContext::new();
    oxideav_vfw::register(&mut ctx);
    // Did not panic. We don't assert any specific state on
    // ctx.codecs — register is allowed to register zero codecs
    // (the directory doesn't exist), and codecs registered by
    // some sibling reachable from prior test setup should not
    // bleed into our assertions.

    match saved {
        Some(v) => env::set_var("OXIDEAV_VFW_CODEC_PATH", v),
        None => env::remove_var("OXIDEAV_VFW_CODEC_PATH"),
    }
}

#[test]
fn register_with_dev_null_and_nonexistent_path() {
    let saved = env::var_os("OXIDEAV_VFW_CODEC_PATH");
    let sep = if cfg!(windows) { ";" } else { ":" };
    env::set_var(
        "OXIDEAV_VFW_CODEC_PATH",
        format!("/dev/null{sep}/tmp/vfw-discovery-nonexistent-xyz"),
    );

    let mut ctx = RuntimeContext::new();
    oxideav_vfw::register(&mut ctx);
    // Just verify no panic — `/dev/null` isn't a directory and
    // the second path doesn't exist, so discovery should yield
    // zero codecs.

    match saved {
        Some(v) => env::set_var("OXIDEAV_VFW_CODEC_PATH", v),
        None => env::remove_var("OXIDEAV_VFW_CODEC_PATH"),
    }
}

#[test]
fn cache_round_trip_via_discover() {
    let saved = env::var_os("OXIDEAV_VFW_CODEC_PATH");
    let tmp = Tmp::new("cache-rt");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();

    // Write a single garbage `.dll` so it gets recorded as
    // Unsupported. Probing then re-probing should hit the cache
    // the second time around.
    let dll = codec_dir.join("synth1.dll");
    fs::write(&dll, b"this is not a real PE32 file").unwrap();

    env::set_var("OXIDEAV_VFW_CODEC_PATH", &codec_dir);

    let v1 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v1.len(), 1, "first call should record the synthetic DLL");
    assert_eq!(v1[0].kind, discovery::Kind::Unsupported);

    // Re-call: cache hit yields the same entry.
    let v2 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v2.len(), 1);
    assert_eq!(v2, v1, "cache hit should produce identical entries");

    match saved {
        Some(v) => env::set_var("OXIDEAV_VFW_CODEC_PATH", v),
        None => env::remove_var("OXIDEAV_VFW_CODEC_PATH"),
    }
}

#[test]
fn cache_invalidates_on_mtime_change() {
    use std::thread::sleep;
    use std::time::Duration;

    let tmp = Tmp::new("cache-inv");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    let dll = codec_dir.join("synth.dll");
    fs::write(&dll, b"original-bytes-not-PE").unwrap();

    let v1 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v1.len(), 1);
    let mtime1 = v1[0].mtime_unix;

    // Sleep just over a second so filesystem mtime resolution
    // (1s on many ext4 / HFS+ configs) actually rolls forward.
    sleep(Duration::from_millis(1100));

    // Overwrite — different size, fresh mtime → cache miss.
    fs::write(&dll, b"different bytes still not PE -- longer this time").unwrap();

    let v2 = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(v2.len(), 1);
    assert_ne!(
        v2[0].mtime_unix, mtime1,
        "mtime should advance after rewrite"
    );
    assert_ne!(
        v2[0].size_bytes, v1[0].size_bytes,
        "size should differ between rewrites"
    );
}

#[test]
fn probe_minimal_synthetic_dll_unsupported() {
    let tmp = Tmp::new("synth-pe");
    let codec_dir = tmp.path().join("codecs");
    fs::create_dir_all(&codec_dir).unwrap();
    let dll = codec_dir.join("min.dll");
    let bytes = build_minimal_dll();
    {
        let mut f = fs::File::create(&dll).unwrap();
        f.write_all(&bytes).unwrap();
    }
    let entries = discovery::discover(std::slice::from_ref(&codec_dir));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, discovery::Kind::Unsupported);
    assert!(entries[0].fourccs.is_empty());
    assert!(entries[0].clsid.is_none());
}
