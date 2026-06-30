//! File-location helpers for the ssh_config compression lookup.
//!
//! Resolves the ordered list of candidate config paths (honouring a
//! `-F` override), the per-user home directory, and the local-user env
//! var used by `Match localuser`.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// Returns the ordered list of ssh_config paths the caller should
/// consult. Visible to tests so they can verify the precedence chain
/// without touching the real filesystem.
pub(super) fn candidate_paths(options: &[OsString]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(override_path) = extract_dash_f_path(options) {
        paths.push(override_path);
    }
    if let Some(home) = home_dir() {
        paths.push(home.join(".ssh").join("config"));
    }
    paths.push(PathBuf::from("/etc/ssh/ssh_config"));
    paths
}

/// Walks `options` looking for `-F file` (split across two args) or
/// `-Ffile` (concatenated). Returns the first occurrence as a
/// [`PathBuf`].
pub(super) fn extract_dash_f_path(options: &[OsString]) -> Option<PathBuf> {
    let mut iter = options.iter();
    while let Some(opt) = iter.next() {
        if opt == OsStr::new("-F") {
            return iter.next().map(PathBuf::from);
        }
        let bytes = opt.to_string_lossy();
        if let Some(rest) = bytes.strip_prefix("-F")
            && !rest.is_empty()
        {
            return Some(PathBuf::from(rest));
        }
    }
    None
}

/// Returns the local username from `USER` (Unix) or `USERNAME`
/// (Windows). Returns `None` on platforms without either var or when
/// the value is empty.
pub(super) fn local_user_env() -> Option<String> {
    #[cfg(unix)]
    let raw = std::env::var_os("USER");
    #[cfg(windows)]
    let raw = std::env::var_os("USERNAME");
    #[cfg(not(any(unix, windows)))]
    let raw: Option<std::ffi::OsString> = None;

    let value = raw?.to_string_lossy().into_owned();
    if value.is_empty() { None } else { Some(value) }
}

/// Resolves the per-user home directory. Mirrors the helper in
/// `embedded::ssh_config` rather than reaching across module boundaries
/// because the embedded transport is gated behind a different feature.
pub(super) fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}
