//! Round 235 — exercise `discovery::probe_dll` from the public
//! re-export surface a downstream consumer would actually see.
//!
//! The round adds `probe_dll(&Path) -> Option<ProbeResult>` as
//! the single-shot companion to `discover_and_register(ctx)`:
//! a CLI tool, an integration test, or any consumer that already
//! holds an absolute DLL path can now classify it through the
//! same VfW-then-DirectShow probe pipeline without walking the
//! discovery directory or touching the on-disk cache.
//!
//! Coverage:
//!
//! * The helper is reachable from the crate-root re-export path
//!   `oxideav_vfw::discovery::{probe_dll, ProbeResult, Kind}`
//!   so its public surface contract is wired correctly (this
//!   round's surface-area test exists precisely to catch a
//!   future `pub use probe::...` typo that compiles but breaks
//!   downstream consumers).
//! * A synthesised minimal PE32 (via the dev-dep
//!   `ud_emulator::pe::test_image::build_minimal_dll`) reads
//!   cleanly but exports neither `DriverProc` nor
//!   `DllGetClassObject`, so the classification lands on
//!   `Kind::Unsupported` with empty fourccs and no CLSID.
//! * Missing paths surface as `None`, distinguishing
//!   "couldn't read the file" from "read fine but unclassifiable"
//!   for downstream consumers that want to log the two cases
//!   differently.

use oxideav_vfw::discovery::{probe_dll, Kind, ProbeResult};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Tiny zero-dep tempdir wrapper. Mirrors `discovery::test_tmpdir::Tmp`
/// but stays in this integration-test file so it doesn't depend on
/// `pub(crate)` reachability.
struct Tmp(PathBuf);

impl Tmp {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("oxideav-vfw-r235-{label}-{pid}-{nanos}-{n}"));
        fs::create_dir_all(&path).unwrap();
        Tmp(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn probe_dll_garbage_classified_unsupported_via_public_surface() {
    let tmp = Tmp::new("garbage");
    let p = tmp.path().join("garbage.dll");
    let mut f = fs::File::create(&p).unwrap();
    f.write_all(b"definitely not a PE32 file payload").unwrap();
    drop(f);

    let r: ProbeResult = probe_dll(&p).expect("file reads cleanly → Some(_)");
    assert_eq!(r.kind, Kind::Unsupported);
    assert!(r.fourccs.is_empty());
    assert!(r.clsid.is_none());
}

#[test]
fn probe_dll_missing_path_returns_none_via_public_surface() {
    let p = PathBuf::from("/this/does/not/exist/r235.dll");
    assert!(probe_dll(&p).is_none());
}

#[test]
fn probe_dll_minimal_synthetic_dll_unsupported_via_public_surface() {
    // Same DLL builder the unit-level probe tests use — confirms
    // the public re-export observes the identical classification.
    let dll = ud_emulator::pe::test_image::build_minimal_dll();
    let tmp = Tmp::new("minimal");
    let p = tmp.path().join("minimal.dll");
    fs::write(&p, &dll).unwrap();
    let r = probe_dll(&p).expect("file reads cleanly → Some(_)");
    assert_eq!(r.kind, Kind::Unsupported);
    assert!(r.fourccs.is_empty());
    assert!(r.clsid.is_none());
}
