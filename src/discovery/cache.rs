//! On-disk JSON cache of discovery results.
//!
//! ## Schemas
//!
//! Two on-disk shapes are accepted by [`Cache::load`]:
//!
//! 1. **Versioned envelope (current, written by every round-197+
//!    save):** `{"version": N, "entries": [CacheEntry, ...]}`.
//!    Version mismatches (older or newer than
//!    [`CURRENT_SCHEMA_VERSION`]) are treated as a parse failure —
//!    `load` returns `None` and the next `save_atomic` overwrites
//!    the file with the current schema. This mirrors the
//!    round-189 "corruption is recoverable" contract: a downgrade
//!    or a forward-incompatible upgrade must never poison
//!    `register()`.
//! 2. **Legacy bare-array (pre-round-197, still readable for
//!    seamless upgrade):** a top-level JSON array of
//!    [`CacheEntry`]. Loading one of these is accepted exactly
//!    once; the next `save_atomic` rewrites the file with the
//!    versioned envelope, so the bare-array shape is never
//!    re-written by this crate.
//!
//! ## Lookup
//!
//! Lookups are keyed by `(absolute_path, mtime_unix,
//! size_bytes)`. A mismatch on any of those three is treated as a
//! cache miss; on miss we re-probe and overwrite the entry.
//! Atomic writes via tempfile + rename.

use std::cell::Cell;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::probe::Kind;
use super::DiscoveryEntry;

/// On-disk schema version stamped into every save. Bumped when the
/// shape of [`CacheEntry`] changes in a way that would silently
/// mis-interpret older or newer rows — readers that don't know the
/// version refuse to trust the file (round-189 corruption-recovery
/// path: discard + re-probe + heal on next write).
///
/// **History:**
/// - `1` (round 197): first versioned envelope. The
///   [`CacheEntry`] shape that ships with v1 is identical to the
///   pre-versioning round-28..189 shape, so a pre-r197 cache file
///   is still loadable through the legacy bare-array path below.
///   Re-saving promotes it to v1.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// One cache row. Mirrors [`DiscoveryEntry`] verbatim — the
/// separate type exists so we can freely evolve the in-memory
/// representation without rewriting the on-disk schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheEntry {
    pub path: PathBuf,
    pub mtime_unix: i64,
    pub size_bytes: u64,
    pub kind: String,
    pub fourccs: Vec<String>,
    pub clsid: Option<String>,
    /// Reserved for round-29 — round-trip a captured handshake
    /// transcript so we can replay reverse-engineered protocols
    /// without re-driving the codec on every register call.
    pub handshake: Option<serde_json::Value>,
}

impl CacheEntry {
    fn from_entry(e: &DiscoveryEntry) -> Self {
        CacheEntry {
            path: e.path.clone(),
            mtime_unix: e.mtime_unix,
            size_bytes: e.size_bytes,
            kind: kind_to_str(e.kind).to_string(),
            fourccs: e.fourccs.clone(),
            clsid: e.clsid.clone(),
            handshake: None,
        }
    }

    fn to_entry(&self) -> DiscoveryEntry {
        DiscoveryEntry {
            path: self.path.clone(),
            mtime_unix: self.mtime_unix,
            size_bytes: self.size_bytes,
            kind: str_to_kind(&self.kind),
            fourccs: self.fourccs.clone(),
            clsid: self.clsid.clone(),
        }
    }

    /// True if this row's `(path, mtime_unix, size_bytes)` triple
    /// matches the supplied `(path, mtime, size)`. Mirrors
    /// [`super::DiscoveryEntry::matches`] on the on-disk row type
    /// so the cache's staleness check has a single source of truth.
    ///
    /// Round 217 introduced this method to dedupe the triple
    /// equality check previously inlined into [`Cache::lookup`].
    /// The hand-written `e.path == path && e.mtime_unix == mtime &&
    /// e.size_bytes == size` chain was structurally identical to
    /// the [`super::DiscoveryEntry::matches`] body but lived in a
    /// separate file, so a future change to the triple's
    /// definition risked diverging silently between the in-memory
    /// and on-disk types. Routing both through a sibling pair of
    /// `matches` methods keeps the contract co-located with each
    /// type's definition.
    pub fn matches(&self, path: &Path, mtime: i64, size: u64) -> bool {
        self.path == path && self.mtime_unix == mtime && self.size_bytes == size
    }
}

fn kind_to_str(k: Kind) -> &'static str {
    match k {
        Kind::Vfw => "vfw",
        Kind::DirectShow => "directshow",
        Kind::Unsupported => "unsupported",
    }
}

fn str_to_kind(s: &str) -> Kind {
    match s {
        "vfw" => Kind::Vfw,
        "directshow" => Kind::DirectShow,
        _ => Kind::Unsupported,
    }
}

/// Versioned on-disk envelope. Future schema bumps re-serialise into
/// the same envelope shape (`version` advances; field additions to
/// [`CacheEntry`] live alongside).
///
/// Serialised key order is `version` then `entries` — the unit test
/// `load_versioned_envelope_round_trips` locks in the shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VersionedCache {
    version: u32,
    entries: Vec<CacheEntry>,
}

/// In-memory cache. Loads either the versioned envelope (current
/// schema) or the legacy bare-array shape (pre-r197); saves the
/// versioned envelope unconditionally.
///
/// Carries an interior **dirty flag** ([`Cache::is_dirty`]) set by
/// any mutation that would alter the on-disk shape — `upsert` for
/// in-memory churn, and the [`Cache::load`] legacy bare-array fallback
/// which needs a one-time promotion-write. A successful
/// `save_atomic` clears it. `discover()` uses this to skip a no-op
/// rewrite on the steady-state case (all DLLs already cached, no
/// envelope shape change), eliminating one full pretty-printed
/// `vfw-discovery.json` rewrite per `register()` call against a
/// stable codec directory. The flag uses [`Cell`] so `save_atomic`
/// keeps its pre-r204 `&self` signature.
#[derive(Debug, Default, Clone)]
pub struct Cache {
    entries: Vec<CacheEntry>,
    /// Set by every mutation that would change the on-disk
    /// representation. `save_atomic` clears it on success.
    dirty: Cell<bool>,
}

impl Cache {
    /// Read `path`. Returns an empty cache on any I/O / parse
    /// failure (corrupted on-disk cache must never poison
    /// `register()`).
    ///
    /// Accepts:
    /// 1. The versioned envelope `{"version": N, "entries": [...]}`
    ///    when `N == CURRENT_SCHEMA_VERSION`. Any other version
    ///    (older or newer) yields `None` — the file is treated as
    ///    corrupted from this reader's perspective and the next
    ///    `save_atomic` overwrites it cleanly.
    /// 2. The legacy bare-array `[CacheEntry, ...]` shape, for
    ///    seamless upgrade from pre-round-197 caches. The next
    ///    `save_atomic` promotes the file to the versioned shape.
    pub fn load(path: &Path) -> Option<Self> {
        let data = fs::read(path).ok()?;
        // Versioned envelope path first — covers every cache file
        // written by round-197+.
        if let Ok(env) = serde_json::from_slice::<VersionedCache>(&data) {
            if env.version != CURRENT_SCHEMA_VERSION {
                // Forward-incompatible (newer writer) or
                // backward-incompatible (older + reshaped) — refuse
                // to trust the file and let the corruption-recovery
                // path on the next `save_atomic` heal it.
                return None;
            }
            let (entries, removed) = dedupe_rows_keep_last(env.entries);
            // The on-disk shape already matches what we'd write —
            // clean by construction (round 204) — UNLESS the file
            // carried duplicate-path rows (round 401): then the
            // deduped in-memory state diverges and the next save
            // must fire to rewrite the file without the zombies.
            return Some(Cache {
                entries,
                dirty: Cell::new(removed > 0),
            });
        }
        // Legacy bare-array fallback — covers caches written by
        // rounds 28..189 (no envelope, no version field).
        let entries: Vec<CacheEntry> = serde_json::from_slice(&data).ok()?;
        let (entries, _) = dedupe_rows_keep_last(entries);
        // Round 204: legacy bare-array shape is loadable but its
        // on-disk representation differs from what we'd write now.
        // Mark dirty so the next `save_atomic` actually fires and
        // promotes the file to the versioned envelope; without this
        // flag the round-204 no-op-skip would leave a pre-r197 cache
        // file un-promoted on a stable codec directory.
        Some(Cache {
            entries,
            dirty: Cell::new(true),
        })
    }

    /// [`Cache::load`] with an explicit heal marker for corrupt
    /// files (round 401).
    ///
    /// - File parses (envelope at the current version, or the
    ///   legacy bare array): behaves exactly like `load`.
    /// - File **exists but doesn't parse** (corrupted JSON,
    ///   zero-byte truncation, unknown envelope version,
    ///   unreadable): returns an empty cache marked **dirty**, so
    ///   the next [`save_atomic`](Cache::save_atomic) overwrites
    ///   the bad file even when the discovery cycle itself makes
    ///   no mutations. Pre-r401, `load(..).unwrap_or_default()`
    ///   lost the existed-but-corrupt signal: against an empty (or
    ///   all-`Unsupported`-free) codec directory nothing flipped
    ///   the dirty flag, the round-204 no-op-skip held, and the
    ///   corrupt file survived every subsequent `register()` call
    ///   instead of being healed as the module docs promise.
    /// - File is simply **missing**: clean empty cache (no file is
    ///   created by a cycle that discovers nothing — the no-op
    ///   contract stands).
    pub fn load_or_heal(path: &Path) -> Self {
        match Self::load(path) {
            Some(c) => c,
            None => Cache {
                entries: Vec::new(),
                dirty: Cell::new(path.exists()),
            },
        }
    }

    /// Look up by `(path, mtime, size)`. Stale entries (mtime or
    /// size mismatch) return `None`.
    ///
    /// Round 217 routes the triple equality check through
    /// [`CacheEntry::matches`] (which mirrors
    /// [`super::DiscoveryEntry::matches`]) so the cache's freshness
    /// rule is defined in exactly one place per type. The previous
    /// hand-inlined `e.path == path && e.mtime_unix == mtime &&
    /// e.size_bytes == size` chain was structurally correct but
    /// duplicated the contract three times across the crate
    /// (`DiscoveryEntry::matches`, this loop, the in-memory dedupe
    /// in `Cache::upsert`).
    pub fn lookup(&self, path: &Path, mtime: i64, size: u64) -> Option<DiscoveryEntry> {
        self.entries
            .iter()
            .find(|e| e.matches(path, mtime, size))
            .map(|e| e.to_entry())
    }

    /// Insert or overwrite. If an entry for `entry.path` already
    /// exists (regardless of mtime/size), it is replaced.
    ///
    /// Sets the [`is_dirty`](Cache::is_dirty) flag unconditionally —
    /// even an upsert that re-writes a row to its own current value
    /// counts as a mutation from the caller's perspective; the
    /// no-op-skip in [`super::discover`] relies on `is_dirty` flipping
    /// from `false` to `true` whenever a cache miss re-probes a DLL.
    pub fn upsert(&mut self, entry: DiscoveryEntry) {
        let row = CacheEntry::from_entry(&entry);
        if let Some(pos) = self.entries.iter().position(|e| e.path == entry.path) {
            self.entries[pos] = row;
        } else {
            self.entries.push(row);
        }
        self.dirty.set(true);
    }

    /// Atomic write: serialise to a sibling tempfile, then
    /// rename. On any failure, leaves the original cache intact.
    ///
    /// Always writes the versioned envelope at
    /// [`CURRENT_SCHEMA_VERSION`] — never the legacy bare-array
    /// shape, even if `load` consumed a legacy file on the way in.
    /// This is the seamless-upgrade story: a single
    /// `discover() → save_atomic()` cycle promotes any legacy cache
    /// file to the current schema.
    ///
    /// Clears [`is_dirty`](Cache::is_dirty) on success. Round 204:
    /// `super::discover` calls this only when `is_dirty()` is true,
    /// so a steady-state `register()` against a fully-cached codec
    /// directory no longer rewrites the cache file at all.
    pub fn save_atomic(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = tempfile_sibling(path);
        // Pretty-printed JSON — discovery output is human-grep-friendly.
        let envelope = VersionedCache {
            version: CURRENT_SCHEMA_VERSION,
            entries: self.entries.clone(),
        };
        let json = serde_json::to_vec_pretty(&envelope).map_err(io_err)?;
        // Round 401: on ANY failure between tempfile creation and
        // the rename, remove the tempfile before surfacing the
        // error. Pre-r401 a failed rename (e.g. the cache path
        // occupied by a directory, or a cross-device link error
        // when a symlinked cache dir points at another filesystem)
        // stranded one `vfw-discovery.json.tmp.<pid>.<nanos>`
        // sibling per attempt — and since the tempfile name embeds
        // a nanosecond stamp, every retry leaked a fresh one.
        let write_then_rename = (|| -> std::io::Result<()> {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()?;
            fs::rename(&tmp, path)
        })();
        if let Err(e) = write_then_rename {
            // Best-effort cleanup; the original cache file (if any)
            // is untouched either way. The dirty flag deliberately
            // stays set — the in-memory state still diverges from
            // disk, so a later save attempt must fire.
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.dirty.set(false);
        Ok(())
    }

    /// Drop rows for DLLs that vanished from a walked discovery
    /// root (round 401).
    ///
    /// `roots` is the set of directories the current discovery
    /// cycle **successfully iterated**; `seen` is every candidate
    /// path that walk encountered (cache hits and misses alike). A
    /// row is removed iff its file's parent directory is one of
    /// `roots` *and* the walk didn't see the file — i.e. the DLL
    /// was deleted (or renamed away from `*.dll` / `*.ax`) since
    /// the row was written. Rows under directories that were NOT
    /// walked this cycle are always kept: the cache file is shared
    /// across `OXIDEAV_VFW_CODEC_PATH` configurations, and a row
    /// belonging to an unmounted removable drive or a root that's
    /// simply absent from the current path list must survive so
    /// the next cycle that walks it again gets its hits back.
    /// Unreadable roots never reach `roots` (the walk skips them
    /// before iterating), so a transiently unmountable directory
    /// can't trigger a prune storm.
    ///
    /// Root matching is exact parent equality — the walk is
    /// non-recursive, so a cached row's file can only have been
    /// produced by the root that is its direct parent.
    ///
    /// Returns the number of rows removed; sets the
    /// [`is_dirty`](Cache::is_dirty) flag iff that count is
    /// non-zero (a no-op prune must not defeat the round-204
    /// no-op-save skip).
    pub fn prune_vanished(&mut self, roots: &[&Path], seen: &[PathBuf]) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| {
            let under_walked_root = e
                .path
                .parent()
                .is_some_and(|parent| roots.contains(&parent));
            !under_walked_root || seen.contains(&e.path)
        });
        let removed = before - self.entries.len();
        if removed > 0 {
            self.dirty.set(true);
        }
        removed
    }

    /// True when the in-memory state diverges from what the
    /// last-loaded on-disk file held.  Set by [`Cache::upsert`] and
    /// by [`Cache::load`] when it consumed a pre-r197 legacy
    /// bare-array shape (so the next `save_atomic` actually fires
    /// and promotes the file); cleared on successful
    /// [`Cache::save_atomic`].
    ///
    /// Round 204 added the flag so [`super::discover`] can skip the
    /// atomic-rewrite call when nothing changed — common-case
    /// `register()` against a stable codec directory now costs zero
    /// filesystem writes instead of one full pretty-printed
    /// `vfw-discovery.json` rewrite per call.
    pub fn is_dirty(&self) -> bool {
        self.dirty.get()
    }

    /// Total entry count — diagnostic only.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no entries are stored.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn io_err(e: serde_json::Error) -> std::io::Error {
    std::io::Error::other(e)
}

/// Collapse duplicate-path rows, keeping the **last** occurrence
/// of each path (round 401). Returns the deduped rows plus the
/// number removed.
///
/// The cache table is keyed by absolute path — [`Cache::upsert`]
/// replaces in place, so this crate never *writes* duplicates. But
/// a hand-edited file, a bad merge of two caches, or an external
/// tool appending rows can produce them, and they were previously
/// loaded verbatim: `upsert` then replaced only the FIRST matching
/// row, leaving the later duplicate as a zombie that the
/// vanished-row prune could never remove (its file still exists,
/// so it always counts as seen). Keep-last matches the "later
/// entry is the most recently written" convention of every
/// append-shaped producer; the survivor keeps the last
/// occurrence's position so relative order among distinct paths
/// is preserved.
fn dedupe_rows_keep_last(rows: Vec<CacheEntry>) -> (Vec<CacheEntry>, usize) {
    let before = rows.len();
    let mut out: Vec<CacheEntry> = Vec::with_capacity(before);
    for row in rows {
        if let Some(pos) = out.iter().position(|e| e.path == row.path) {
            out.remove(pos);
        }
        out.push(row);
    }
    let removed = before - out.len();
    (out, removed)
}

/// Build a tempfile path next to `target` for an atomic write.
fn tempfile_sibling(target: &Path) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut s = target.as_os_str().to_owned();
    s.push(format!(".tmp.{pid}.{nanos}"));
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::test_tmpdir::Tmp;

    fn sample_entry(path: &Path, kind: Kind) -> DiscoveryEntry {
        DiscoveryEntry {
            path: path.to_path_buf(),
            mtime_unix: 1_700_000_000,
            size_bytes: 524_288,
            kind,
            fourccs: vec!["MP43".into(), "MP42".into()],
            clsid: None,
        }
    }

    #[test]
    fn round_trip_preserves_entries() {
        let tmp = Tmp::new("cache");
        let cache_path = tmp.path().join("disc.json");
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        c.upsert(sample_entry(Path::new("/abs/b.ax"), Kind::DirectShow));
        c.save_atomic(&cache_path).unwrap();

        let loaded = Cache::load(&cache_path).unwrap();
        assert_eq!(loaded.len(), 2);
        let look = loaded
            .lookup(Path::new("/abs/a.dll"), 1_700_000_000, 524_288)
            .unwrap();
        assert_eq!(look.kind, Kind::Vfw);
        assert_eq!(look.fourccs, vec!["MP43".to_string(), "MP42".to_string()]);
    }

    #[test]
    fn lookup_misses_on_mtime_change() {
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        // size matches but mtime is different → miss.
        assert!(c
            .lookup(Path::new("/abs/a.dll"), 1_700_000_001, 524_288)
            .is_none());
    }

    #[test]
    fn lookup_misses_on_size_change() {
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        assert!(c
            .lookup(Path::new("/abs/a.dll"), 1_700_000_000, 999_999)
            .is_none());
    }

    #[test]
    fn upsert_overwrites_same_path() {
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        let mut second = sample_entry(Path::new("/abs/a.dll"), Kind::DirectShow);
        second.fourccs = vec!["WMV3".into()];
        c.upsert(second);
        assert_eq!(c.len(), 1);
        let look = c
            .lookup(Path::new("/abs/a.dll"), 1_700_000_000, 524_288)
            .unwrap();
        assert_eq!(look.kind, Kind::DirectShow);
        assert_eq!(look.fourccs, vec!["WMV3".to_string()]);
    }

    #[test]
    fn load_missing_file_returns_none() {
        let tmp = Tmp::new("missing");
        let path = tmp.path().join("absent.json");
        assert!(Cache::load(&path).is_none());
    }

    #[test]
    fn load_corrupted_file_returns_none() {
        let tmp = Tmp::new("corrupt");
        let path = tmp.path().join("bad.json");
        fs::write(&path, b"{not valid json").unwrap();
        assert!(Cache::load(&path).is_none());
    }

    // ── Round 197: schema versioning ─────────────────────────────

    #[test]
    fn save_atomic_writes_versioned_envelope_at_current_version() {
        // Saving any cache (even empty) produces a file shaped as
        // `{"version": CURRENT_SCHEMA_VERSION, "entries": [...]}`.
        // The unit-test contract that locks the envelope shape
        // sibling-tests rely on for round-trip compatibility.
        let tmp = Tmp::new("save-envelope");
        let cache_path = tmp.path().join("v.json");
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        c.save_atomic(&cache_path).unwrap();

        let raw = fs::read(&cache_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert!(parsed.is_object(), "envelope, not bare array");
        assert_eq!(
            parsed.get("version").and_then(|v| v.as_u64()),
            Some(CURRENT_SCHEMA_VERSION as u64),
            "envelope stamped at the current schema version",
        );
        assert!(
            parsed.get("entries").map(|e| e.is_array()).unwrap_or(false),
            "entries key holds a JSON array",
        );
    }

    #[test]
    fn load_versioned_envelope_round_trips() {
        let tmp = Tmp::new("rt-envelope");
        let cache_path = tmp.path().join("v.json");
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        c.upsert(sample_entry(Path::new("/abs/b.ax"), Kind::DirectShow));
        c.save_atomic(&cache_path).unwrap();

        let loaded = Cache::load(&cache_path).expect("envelope round-trips");
        assert_eq!(loaded.len(), 2);
        let look = loaded
            .lookup(Path::new("/abs/a.dll"), 1_700_000_000, 524_288)
            .unwrap();
        assert_eq!(look.kind, Kind::Vfw);
    }

    #[test]
    fn load_legacy_bare_array_is_accepted_for_seamless_upgrade() {
        // Pre-r197 caches were a bare JSON array. We MUST keep
        // loading them so users with an existing
        // `~/.cache/oxideav/vfw-discovery.json` from a prior crate
        // version don't get a re-probe storm at first register
        // after upgrading.
        let tmp = Tmp::new("legacy");
        let path = tmp.path().join("legacy.json");
        let legacy_entries = vec![CacheEntry::from_entry(&sample_entry(
            Path::new("/abs/legacy.dll"),
            Kind::Vfw,
        ))];
        let raw = serde_json::to_vec_pretty(&legacy_entries).unwrap();
        fs::write(&path, &raw).unwrap();

        let loaded = Cache::load(&path).expect("legacy bare-array still loads");
        assert_eq!(loaded.len(), 1);
        let look = loaded
            .lookup(Path::new("/abs/legacy.dll"), 1_700_000_000, 524_288)
            .unwrap();
        assert_eq!(look.kind, Kind::Vfw);
    }

    #[test]
    fn save_after_loading_legacy_promotes_to_versioned_envelope() {
        // The seamless-upgrade story: round-trip a legacy file
        // through load + save and verify the on-disk shape is now
        // the versioned envelope. The next register call after a
        // crate upgrade picks up the new shape automatically; no
        // user intervention required.
        let tmp = Tmp::new("promote");
        let path = tmp.path().join("legacy.json");
        let legacy_entries = vec![CacheEntry::from_entry(&sample_entry(
            Path::new("/abs/p.dll"),
            Kind::Vfw,
        ))];
        let raw = serde_json::to_vec_pretty(&legacy_entries).unwrap();
        fs::write(&path, &raw).unwrap();

        let loaded = Cache::load(&path).unwrap();
        loaded.save_atomic(&path).unwrap();

        let post = fs::read(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&post).unwrap();
        assert!(parsed.is_object(), "promoted to envelope");
        assert_eq!(
            parsed.get("version").and_then(|v| v.as_u64()),
            Some(CURRENT_SCHEMA_VERSION as u64),
        );
    }

    #[test]
    fn load_envelope_with_unknown_version_returns_none() {
        // A cache file written by a newer crate (envelope version >
        // CURRENT_SCHEMA_VERSION) MUST be refused. The round-189
        // corruption-recovery path kicks in next: discovery
        // re-probes and `save_atomic` heals the file to our
        // version. Mirrors the legacy-corruption shape exactly so
        // there's no extra handling burden.
        let tmp = Tmp::new("future");
        let path = tmp.path().join("future.json");
        let future = serde_json::json!({
            "version": CURRENT_SCHEMA_VERSION + 99,
            "entries": [],
        });
        fs::write(&path, serde_json::to_vec(&future).unwrap()).unwrap();

        assert!(
            Cache::load(&path).is_none(),
            "unknown envelope version is treated as corruption",
        );
    }

    #[test]
    fn load_envelope_with_older_version_returns_none() {
        // Symmetric — a hypothetical older v0 envelope (which we
        // never wrote, but a downgrade-and-re-upgrade workflow
        // could produce) is also refused. Same recovery path.
        let tmp = Tmp::new("older");
        let path = tmp.path().join("older.json");
        let older = serde_json::json!({ "version": 0, "entries": [] });
        fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();

        assert!(Cache::load(&path).is_none());
    }

    #[test]
    fn envelope_with_malformed_entries_returns_none() {
        // Envelope shape parses, but `entries` is the wrong type
        // (string instead of array). serde_json refuses; we
        // surface as None just like any other parse failure.
        let tmp = Tmp::new("bad-entries");
        let path = tmp.path().join("bad.json");
        let bad = serde_json::json!({
            "version": CURRENT_SCHEMA_VERSION,
            "entries": "not an array",
        });
        fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();

        assert!(Cache::load(&path).is_none());
    }

    // ── Round 204: dirty-flag + no-op-save-skip ─────────────────

    #[test]
    fn default_cache_is_not_dirty() {
        // A freshly constructed cache has no divergence from the
        // empty on-disk file the steady-state `discover()` would
        // produce, so `is_dirty()` MUST be false out of the gate.
        // If this flipped, the round-204 no-op-skip would still
        // fire `save_atomic` on the very first `register()` call
        // for an empty / missing cache file — defeating half the
        // optimisation.
        let c = Cache::default();
        assert!(!c.is_dirty());
    }

    #[test]
    fn upsert_marks_cache_dirty() {
        // Every mutation flips the flag — the no-op-skip relies on
        // a single cache-miss `discover()` flow producing a dirty
        // cache that DOES get persisted.
        let mut c = Cache::default();
        assert!(!c.is_dirty());
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        assert!(c.is_dirty(), "upsert sets dirty");
    }

    #[test]
    fn save_atomic_clears_dirty_flag() {
        // Once persisted, the cache is back in sync with disk; a
        // subsequent `discover()` against an unchanged codec
        // directory MUST then see `is_dirty() == false` and skip
        // its rewrite.
        let tmp = Tmp::new("clear-dirty");
        let cache_path = tmp.path().join("v.json");
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        assert!(c.is_dirty());
        c.save_atomic(&cache_path).unwrap();
        assert!(!c.is_dirty(), "successful save clears dirty");
    }

    #[test]
    fn load_versioned_envelope_starts_clean() {
        // The on-disk envelope already matches what we'd write —
        // loading it produces a clean cache; a subsequent
        // `discover()` that adds nothing skips its `save_atomic`.
        let tmp = Tmp::new("load-clean");
        let cache_path = tmp.path().join("v.json");
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        c.save_atomic(&cache_path).unwrap();

        let loaded = Cache::load(&cache_path).expect("envelope loads");
        assert!(
            !loaded.is_dirty(),
            "envelope shape already matches what save would write",
        );
    }

    // ── Round 217: CacheEntry::matches triple-equality contract ─

    fn sample_cache_row() -> CacheEntry {
        CacheEntry::from_entry(&sample_entry(Path::new("/abs/a.dll"), Kind::Vfw))
    }

    #[test]
    fn cache_entry_matches_returns_true_on_identical_triple() {
        // Round 217 — the on-disk row mirrors the in-memory
        // `DiscoveryEntry::matches` contract so `Cache::lookup` can
        // delegate to the row's own staleness check rather than
        // hand-inlining a `&&` chain. The base hit case.
        let row = sample_cache_row();
        assert!(row.matches(Path::new("/abs/a.dll"), 1_700_000_000, 524_288));
    }

    #[test]
    fn cache_entry_matches_false_on_any_field_mismatch() {
        // All three components must agree — partial matches MUST
        // miss, otherwise a stale row would survive after a DLL
        // rewrite (the same trap `discover()` is defended against
        // by routing the triple check through this method).
        let row = sample_cache_row();
        assert!(!row.matches(Path::new("/abs/b.dll"), 1_700_000_000, 524_288));
        assert!(!row.matches(Path::new("/abs/a.dll"), 1_700_000_001, 524_288));
        assert!(!row.matches(Path::new("/abs/a.dll"), 1_700_000_000, 524_289));
    }

    #[test]
    fn cache_lookup_routes_through_cache_entry_matches() {
        // Round 217 — end-to-end pin: `Cache::lookup` now delegates
        // to `CacheEntry::matches`, so a row whose `matches` would
        // return false MUST yield `None` here. Mirrors the existing
        // `lookup_misses_on_mtime_change` / `lookup_misses_on_size_change`
        // tests above but covers both axes in one assertion-rich
        // shape after the refactor.
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/x.dll"), Kind::Vfw));
        // Triple matches → hit, returns the converted DiscoveryEntry.
        let hit = c.lookup(Path::new("/abs/x.dll"), 1_700_000_000, 524_288);
        assert!(hit.is_some(), "exact triple hits");
        assert_eq!(hit.unwrap().kind, Kind::Vfw);
        // Each individual field mismatch → miss.
        assert!(c
            .lookup(Path::new("/abs/y.dll"), 1_700_000_000, 524_288)
            .is_none());
        assert!(c
            .lookup(Path::new("/abs/x.dll"), 1_700_000_001, 524_288)
            .is_none());
        assert!(c
            .lookup(Path::new("/abs/x.dll"), 1_700_000_000, 999_999)
            .is_none());
    }

    // ── Round 401: load_or_heal — corrupt-file heal marker ──────

    #[test]
    fn load_or_heal_missing_file_is_clean_empty() {
        // No file → nothing to heal; the cycle must stay zero-write
        // if it discovers nothing.
        let tmp = Tmp::new("heal-missing");
        let c = Cache::load_or_heal(&tmp.path().join("absent.json"));
        assert!(c.is_empty());
        assert!(!c.is_dirty(), "missing file must not force a write");
    }

    #[test]
    fn load_or_heal_corrupt_file_is_empty_and_dirty() {
        // Existing-but-unparseable → empty cache that WANTS a
        // rewrite, so the heal fires even on a zero-probe cycle.
        let tmp = Tmp::new("heal-corrupt");
        let path = tmp.path().join("bad.json");
        fs::write(&path, b"{definitely not json").unwrap();
        let c = Cache::load_or_heal(&path);
        assert!(c.is_empty());
        assert!(c.is_dirty(), "corrupt file marks the cache for healing");
    }

    #[test]
    fn load_or_heal_unknown_version_is_empty_and_dirty() {
        // Unknown envelope version is refused by `load`; through
        // `load_or_heal` it takes the same heal path as corruption.
        let tmp = Tmp::new("heal-version");
        let path = tmp.path().join("future.json");
        let future = serde_json::json!({
            "version": CURRENT_SCHEMA_VERSION + 7,
            "entries": [],
        });
        fs::write(&path, serde_json::to_vec(&future).unwrap()).unwrap();
        let c = Cache::load_or_heal(&path);
        assert!(c.is_empty());
        assert!(c.is_dirty());
    }

    #[test]
    fn load_or_heal_valid_envelope_matches_load() {
        // The happy path is byte-for-byte `load`: entries come
        // back, cache starts clean.
        let tmp = Tmp::new("heal-valid");
        let path = tmp.path().join("ok.json");
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        c.save_atomic(&path).unwrap();

        let healed = Cache::load_or_heal(&path);
        assert_eq!(healed.len(), 1);
        assert!(!healed.is_dirty());
    }

    // ── Round 401: duplicate-path row dedupe on load ────────────

    /// Serialise a versioned envelope holding `rows` verbatim —
    /// bypassing `save_atomic` (which can't produce duplicates) to
    /// simulate a hand-edited / badly-merged cache file.
    fn write_envelope_raw(path: &Path, rows: &[CacheEntry]) {
        let env = serde_json::json!({
            "version": CURRENT_SCHEMA_VERSION,
            "entries": rows,
        });
        fs::write(path, serde_json::to_vec_pretty(&env).unwrap()).unwrap();
    }

    #[test]
    fn load_dedupes_duplicate_path_rows_keeping_last() {
        // Two rows for one path (older mtime first, newer second):
        // the newer row must win, the cache must load dirty so the
        // next save rewrites the file without the zombie.
        let tmp = Tmp::new("dedupe-load");
        let path = tmp.path().join("dup.json");
        let mut older = CacheEntry::from_entry(&sample_entry(Path::new("/abs/dup.dll"), Kind::Vfw));
        older.mtime_unix = 1_600_000_000;
        let newer = CacheEntry::from_entry(&sample_entry(Path::new("/abs/dup.dll"), Kind::Vfw));
        write_envelope_raw(&path, &[older, newer]);

        let loaded = Cache::load(&path).expect("envelope loads");
        assert_eq!(loaded.len(), 1, "duplicate collapsed");
        assert!(
            loaded
                .lookup(Path::new("/abs/dup.dll"), 1_700_000_000, 524_288)
                .is_some(),
            "the LAST (newer) row survives",
        );
        assert!(
            loaded
                .lookup(Path::new("/abs/dup.dll"), 1_600_000_000, 524_288)
                .is_none(),
            "the earlier duplicate is gone",
        );
        assert!(
            loaded.is_dirty(),
            "deduped state diverges from disk → next save must fire",
        );
    }

    #[test]
    fn load_dedupe_preserves_distinct_paths_and_stays_clean() {
        // No duplicates → no behaviour change: all rows load, cache
        // starts clean (round-204 no-op-skip intact).
        let tmp = Tmp::new("dedupe-clean");
        let path = tmp.path().join("ok.json");
        let a = CacheEntry::from_entry(&sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        let b = CacheEntry::from_entry(&sample_entry(Path::new("/abs/b.ax"), Kind::DirectShow));
        write_envelope_raw(&path, &[a, b]);

        let loaded = Cache::load(&path).expect("envelope loads");
        assert_eq!(loaded.len(), 2);
        assert!(!loaded.is_dirty(), "distinct rows load clean");
    }

    #[test]
    fn save_after_dedupe_writes_single_row() {
        // End-to-end: duplicate file → load → save → the on-disk
        // envelope now holds exactly one row for the path.
        let tmp = Tmp::new("dedupe-heal");
        let path = tmp.path().join("dup.json");
        let row = CacheEntry::from_entry(&sample_entry(Path::new("/abs/dup.dll"), Kind::Vfw));
        write_envelope_raw(&path, &[row.clone(), row]);

        let loaded = Cache::load(&path).unwrap();
        loaded.save_atomic(&path).unwrap();

        let post: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            post.get("entries").and_then(|e| e.as_array()).map(Vec::len),
            Some(1),
            "rewritten file carries the deduped single row",
        );
    }

    #[test]
    fn legacy_bare_array_with_duplicates_also_dedupes() {
        // The pre-r197 shape takes the same dedupe path (it's
        // already dirty by definition, so only the row count needs
        // pinning).
        let tmp = Tmp::new("dedupe-legacy");
        let path = tmp.path().join("legacy.json");
        let row = CacheEntry::from_entry(&sample_entry(Path::new("/abs/dup.dll"), Kind::Vfw));
        let rows = vec![row.clone(), row];
        fs::write(&path, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();

        let loaded = Cache::load(&path).expect("legacy loads");
        assert_eq!(loaded.len(), 1);
        assert!(loaded.is_dirty());
    }

    // ── Round 401: failed-save tempfile cleanup ─────────────────

    #[test]
    fn failed_save_removes_tempfile_and_keeps_dirty() {
        // Occupy the cache path with a DIRECTORY: the tempfile
        // write succeeds but the rename-over-a-directory fails on
        // every platform we target. The failure must surface as
        // Err, leave no `.tmp.*` sibling behind, and keep the
        // dirty flag set so a later save attempt still fires.
        let tmp = Tmp::new("save-fail-cleanup");
        let cache_path = tmp.path().join("occupied");
        fs::create_dir_all(&cache_path).unwrap();

        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        assert!(c.is_dirty());

        let res = c.save_atomic(&cache_path);
        assert!(res.is_err(), "rename onto a directory fails");
        assert!(c.is_dirty(), "failed save must not clear the dirty flag");

        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no stranded tempfiles after a failed save: {leftovers:?}",
        );
    }

    #[test]
    fn successful_save_leaves_no_tempfile_sibling() {
        // The happy path never leaked (rename consumes the
        // tempfile) — pinned here so the cleanup refactor can't
        // regress it.
        let tmp = Tmp::new("save-ok-clean");
        let cache_path = tmp.path().join("ok.json");
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/abs/a.dll"), Kind::Vfw));
        c.save_atomic(&cache_path).unwrap();

        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "no tempfiles after success");
        assert!(cache_path.is_file());
    }

    // ── Round 401: vanished-row prune ───────────────────────────

    #[test]
    fn prune_vanished_removes_unseen_row_under_walked_root() {
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/root/gone.dll"), Kind::Vfw));
        c.upsert(sample_entry(Path::new("/root/still.dll"), Kind::Vfw));
        let seen = vec![PathBuf::from("/root/still.dll")];
        let removed = c.prune_vanished(&[Path::new("/root")], &seen);
        assert_eq!(removed, 1, "the unseen row under the walked root goes");
        assert_eq!(c.len(), 1);
        assert!(c
            .lookup(Path::new("/root/still.dll"), 1_700_000_000, 524_288)
            .is_some());
    }

    #[test]
    fn prune_vanished_keeps_rows_under_unwalked_roots() {
        // The cache is shared across OXIDEAV_VFW_CODEC_PATH
        // configurations — a row for a root absent from THIS
        // cycle's path list (unmounted drive, different config)
        // must survive untouched.
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/other/keep.dll"), Kind::Vfw));
        c.save_atomic(&Tmp::new("prune-unwalked").path().join("c.json"))
            .unwrap();
        assert!(!c.is_dirty());
        let removed = c.prune_vanished(&[Path::new("/root")], &[]);
        assert_eq!(removed, 0);
        assert_eq!(c.len(), 1, "row under unwalked root kept");
        assert!(!c.is_dirty(), "no-op prune must not dirty the cache");
    }

    #[test]
    fn prune_vanished_root_match_is_exact_parent_not_prefix() {
        // The walk is non-recursive: walking `/root` says nothing
        // about `/root/sub`, so a row under the subdirectory is
        // out of scope even though `/root` is its path prefix.
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/root/sub/nested.dll"), Kind::Vfw));
        let removed = c.prune_vanished(&[Path::new("/root")], &[]);
        assert_eq!(removed, 0, "prefix match must not evict nested rows");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn prune_vanished_sets_dirty_only_when_rows_removed() {
        let tmp = Tmp::new("prune-dirty");
        let cache_path = tmp.path().join("c.json");
        let mut c = Cache::default();
        c.upsert(sample_entry(Path::new("/root/gone.dll"), Kind::Vfw));
        c.save_atomic(&cache_path).unwrap();
        assert!(!c.is_dirty());
        let removed = c.prune_vanished(&[Path::new("/root")], &[]);
        assert_eq!(removed, 1);
        assert!(c.is_dirty(), "a real prune marks the cache for rewrite");
    }

    #[test]
    fn load_legacy_bare_array_starts_dirty() {
        // The bare-array shape parses fine but diverges from what
        // `save_atomic` would write (it'd emit an envelope). Mark
        // dirty on load so the next `save_atomic` actually fires
        // and promotes the file — without this flag the round-204
        // no-op-skip would leave a pre-r197 cache un-promoted on a
        // stable codec directory.
        let tmp = Tmp::new("load-legacy-dirty");
        let path = tmp.path().join("legacy.json");
        let legacy_entries = vec![CacheEntry::from_entry(&sample_entry(
            Path::new("/abs/legacy.dll"),
            Kind::Vfw,
        ))];
        let raw = serde_json::to_vec_pretty(&legacy_entries).unwrap();
        fs::write(&path, &raw).unwrap();

        let loaded = Cache::load(&path).expect("legacy still loads");
        assert!(
            loaded.is_dirty(),
            "legacy shape diverges from envelope → dirty so promotion fires",
        );
    }
}
