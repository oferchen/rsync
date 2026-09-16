//! Environment helpers for the ssh_config compression lookup.
//!
//! The config-file load order (user file, system file, `-F` override)
//! lives in [`crate::ssh::config_files`]; this module keeps only the
//! local-user env var used by `Match localuser`.

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
