//! Path resolution for the discovery directory + on-disk cache.
//!
//! Resolution order is documented on [`super`].
//!
//! No new dependencies — we read the relevant env vars directly
//! (`OXIDEAV_VFW_CODEC_PATH`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`,
//! `HOME`, `LOCALAPPDATA`) using `std::env::var_os` so non-UTF8
//! values still resolve.

use std::env;
use std::path::PathBuf;

/// On Windows, the platform separator for path lists is `;` — on
/// every other target we follow the conventional `:`.
#[cfg(windows)]
const PATH_SEP: char = ';';
#[cfg(not(windows))]
const PATH_SEP: char = ':';

/// Resolve the list of directories `discover()` should walk.
///
/// `OXIDEAV_VFW_CODEC_PATH=<list>` overrides the default. The
/// list is split on the platform separator (`:` on UNIX, `;` on
/// Windows); each component then has leading and trailing ASCII
/// whitespace stripped, and components that are empty (or
/// whitespace-only) after the strip are skipped silently. This
/// makes the env var forgiving of `.env` files, systemd unit
/// definitions, and Docker / Kubernetes container manifests where
/// shell expansion doesn't run and YAML quoting frequently leaves
/// a stray space or newline around each value. The default is a
/// single-entry list pointing at the platform-conventional codec
/// directory.
///
/// Hard contract: never panics. Returns an empty `Vec` only when
/// no env var is set and the platform default cannot be resolved
/// (e.g. `HOME` and `LOCALAPPDATA` both unset — extremely
/// unusual), or when the env var is set but every entry was
/// empty / whitespace-only.
pub fn discovery_paths() -> Vec<PathBuf> {
    if let Some(over) = env::var_os("OXIDEAV_VFW_CODEC_PATH") {
        return parse_path_list(&over);
    }
    default_discovery_paths()
}

/// Default discovery directory list (env var unset).
fn default_discovery_paths() -> Vec<PathBuf> {
    if let Some(d) = platform_default_codec_dir() {
        vec![d]
    } else {
        Vec::new()
    }
}

#[cfg(windows)]
fn platform_default_codec_dir() -> Option<PathBuf> {
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        let p = PathBuf::from(local).join("oxideav").join("codecs");
        return Some(p);
    }
    None
}

#[cfg(not(windows))]
fn platform_default_codec_dir() -> Option<PathBuf> {
    if let Some(xdg) = env::var_os("XDG_DATA_HOME") {
        let p = PathBuf::from(xdg).join("oxideav").join("codecs");
        return Some(p);
    }
    if let Some(home) = env::var_os("HOME") {
        let p = PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("oxideav")
            .join("codecs");
        return Some(p);
    }
    None
}

/// Split a `PATH`-style list (`:`/`;`-separated, depending on
/// platform) into individual directory entries.
///
/// Each component is stripped of leading and trailing ASCII
/// whitespace before use; components that are empty or
/// whitespace-only after the strip are filtered out. The strip
/// is conservative — only the seven ASCII whitespace characters
/// (`\t \n \v \f \r space`) and only at the edges; embedded or
/// trailing slashes / backslashes / interior whitespace stay
/// intact. Round 211 added the whitespace strip: shell expansion
/// already handles surrounding spaces when a user writes
/// `OXIDEAV_VFW_CODEC_PATH=/p1:/p2` on a command line, but
/// `.env` files, systemd `Environment=` lines, Docker / k8s YAML
/// manifests, and Windows registry strings frequently round-trip
/// through code that doesn't strip — so a `KEY="  /p1 : /p2  "`
/// stanza used to silently produce two unreadable paths. The
/// strip closes that hole without changing behaviour for any
/// existing well-formed input.
fn parse_path_list(value: &std::ffi::OsStr) -> Vec<PathBuf> {
    // Convert via lossy string for splitting — paths that round-trip
    // through this lose sub-codepoint detail on weird inputs, but the
    // discovery path list is a user-facing config so UTF-8 is fine.
    let s = value.to_string_lossy();
    s.split(PATH_SEP)
        .map(|s| s.trim_matches(|c: char| c.is_ascii_whitespace()))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Resolve the on-disk JSON cache file location.
pub fn cache_file_path() -> PathBuf {
    if let Some(p) = platform_cache_dir() {
        p.join("vfw-discovery.json")
    } else {
        // Pathological fallback — current working directory.
        PathBuf::from("vfw-discovery.json")
    }
}

#[cfg(windows)]
fn platform_cache_dir() -> Option<PathBuf> {
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        let p = PathBuf::from(local).join("oxideav").join("Cache");
        return Some(p);
    }
    None
}

#[cfg(not(windows))]
fn platform_cache_dir() -> Option<PathBuf> {
    if let Some(xdg) = env::var_os("XDG_CACHE_HOME") {
        let p = PathBuf::from(xdg).join("oxideav");
        return Some(p);
    }
    if let Some(home) = env::var_os("HOME") {
        let p = PathBuf::from(home).join(".cache").join("oxideav");
        return Some(p);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Process-local serialisation for tests that mutate the
    /// `OXIDEAV_VFW_CODEC_PATH` env var. Within a single test
    /// binary, `cargo test` defaults to multi-threaded execution
    /// and a process-global env var is not thread-safe to mutate
    /// — the round-189 / round-197 cache-dir tests hit the same
    /// failure mode and solved it with the same shape of lock.
    fn env_lock() -> MutexGuard<'static, ()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        L.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn parse_empty_components_skipped() {
        let s = OsString::from(format!("{0}/a{0}{0}/b{0}", PATH_SEP));
        let v = parse_path_list(&s);
        assert_eq!(v, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn parse_single_component() {
        let s = OsString::from("/only/one");
        let v = parse_path_list(&s);
        assert_eq!(v, vec![PathBuf::from("/only/one")]);
    }

    // ── Round 211: whitespace strip on path-list components ─────

    #[test]
    fn parse_strips_surrounding_whitespace() {
        // The shape `.env` files and systemd `Environment=` lines
        // most often produce: a stray space or tab around each
        // component because the writer was lining up an `=` or a
        // human pasted a quoted value. Trim those.
        let s = OsString::from(format!("  /a  {0}\t/b\t{0} /c ", PATH_SEP));
        let v = parse_path_list(&s);
        assert_eq!(
            v,
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c"),
            ]
        );
    }

    #[test]
    fn parse_whitespace_only_components_skipped() {
        // A component made of nothing but whitespace is treated
        // exactly like the existing empty-component case (the user
        // clearly didn't mean to add a path entry there). Without
        // the trim+filter, a single space between separators would
        // hand `PathBuf::from(" ")` to `fs::read_dir`, which fails
        // silently and gives the user no signal at all.
        let s = OsString::from(format!("/a{0}   {0}\t{0}/b", PATH_SEP));
        let v = parse_path_list(&s);
        assert_eq!(v, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn parse_preserves_interior_whitespace_in_path() {
        // A real directory whose name contains a space (common on
        // macOS / Windows — `~/Library/Application Support/...`,
        // `C:\Program Files\...`) MUST round-trip untouched. The
        // r211 trim is strictly `trim_matches`, not a global
        // `replace`.
        let s = OsString::from("/Applications/My Codecs/dir");
        let v = parse_path_list(&s);
        assert_eq!(v, vec![PathBuf::from("/Applications/My Codecs/dir")]);
    }

    #[test]
    fn parse_trims_trailing_newline_on_single_component() {
        // YAML / Docker env loaders that read a file frequently
        // leave the trailing `\n` on the last value. A single-line
        // env var with a stray newline at the end must still
        // resolve to the bare path.
        let s = OsString::from("/codec/dir\n");
        let v = parse_path_list(&s);
        assert_eq!(v, vec![PathBuf::from("/codec/dir")]);
    }

    #[test]
    fn discovery_paths_strips_whitespace_via_env_var() {
        // End-to-end: the public `discovery_paths()` honours the
        // strip when reading `OXIDEAV_VFW_CODEC_PATH`. Locked here
        // because the trim was added inside `parse_path_list`, but
        // the user-visible contract is on `discovery_paths`.
        let _serial = env_lock();
        let saved = env::var_os("OXIDEAV_VFW_CODEC_PATH");
        env::set_var(
            "OXIDEAV_VFW_CODEC_PATH",
            format!(" /tmp/vfw-r211-a {0}\t/tmp/vfw-r211-b\n", PATH_SEP),
        );
        let paths = discovery_paths();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/tmp/vfw-r211-a"),
                PathBuf::from("/tmp/vfw-r211-b"),
            ]
        );
        match saved {
            Some(v) => env::set_var("OXIDEAV_VFW_CODEC_PATH", v),
            None => env::remove_var("OXIDEAV_VFW_CODEC_PATH"),
        }
    }

    #[test]
    fn discovery_paths_honours_override() {
        // Saved/restored manually so we don't poison sibling tests.
        let _serial = env_lock();
        let saved = env::var_os("OXIDEAV_VFW_CODEC_PATH");
        env::set_var(
            "OXIDEAV_VFW_CODEC_PATH",
            format!("/dev/null{0}/tmp/nonexistent", PATH_SEP),
        );
        let paths = discovery_paths();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/dev/null"),
                PathBuf::from("/tmp/nonexistent"),
            ]
        );
        match saved {
            Some(v) => env::set_var("OXIDEAV_VFW_CODEC_PATH", v),
            None => env::remove_var("OXIDEAV_VFW_CODEC_PATH"),
        }
    }

    #[test]
    fn cache_file_path_basename() {
        let p = cache_file_path();
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some("vfw-discovery.json")
        );
    }
}
