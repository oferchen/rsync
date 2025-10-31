use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Returns the set of candidate executable paths derived from `binary`.
///
/// When the supplied value contains path separators the helper returns a
/// single-element vector with the provided path. Otherwise it expands the
/// candidates across the directories listed in the current `PATH` environment
/// variable, mirroring the behaviour used by [`std::process::Command`] when
/// launching child processes.
#[must_use]
pub fn fallback_binary_candidates(binary: &OsStr) -> Vec<PathBuf> {
    let direct_path = Path::new(binary);
    if has_explicit_path(direct_path) {
        return vec![direct_path.to_path_buf()];
    }

    let Some(path_env) = env::var_os("PATH") else {
        return Vec::new();
    };

    let mut results = Vec::new();
    let mut seen = HashSet::new();

    #[cfg(windows)]
    let extensions = collect_windows_extensions(direct_path.extension());

    #[cfg(not(windows))]
    let extensions: Vec<OsString> = vec![OsString::new()];

    for dir in env::split_paths(&path_env) {
        if dir.as_os_str().is_empty() {
            continue;
        }

        #[allow(clippy::needless_borrow)]
        for ext in &extensions {
            let mut candidate = dir.join(direct_path);
            if !ext.is_empty() {
                let ext_text = ext.to_string_lossy();
                let trimmed = ext_text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let ext_without_dot = trimmed.strip_prefix('.').unwrap_or(trimmed);
                candidate.set_extension(ext_without_dot);
            }

            if seen.insert(candidate.clone()) {
                results.push(candidate);
            }
        }
    }

    results
}

/// Reports whether the provided fallback executable exists and is runnable.
#[must_use]
pub fn fallback_binary_available(binary: &OsStr) -> bool {
    fallback_binary_candidates(binary)
        .into_iter()
        .any(|candidate| candidate_is_executable(&candidate))
}

/// Formats a diagnostic explaining that a fallback executable is unavailable.
#[must_use]
pub fn describe_missing_fallback_binary(binary: &OsStr, env_vars: &[&str]) -> String {
    let display = Path::new(binary).display();
    let directive = match env_vars.len() {
        0 => String::from("set an override environment variable to an explicit path"),
        1 => format!("set {} to an explicit path", env_vars[0]),
        2 => format!("set {} or {} to an explicit path", env_vars[0], env_vars[1]),
        _ => {
            let (head, tail) = env_vars.split_at(env_vars.len() - 1);
            let mut joined = head.join(", ");
            joined.push_str(", or ");
            joined.push_str(tail[0]);
            format!("set {} to an explicit path", joined)
        }
    };

    format!(
        "fallback rsync binary '{}' is not available on PATH or is not executable; install upstream rsync or {}",
        display, directive
    )
}

fn has_explicit_path(path: &Path) -> bool {
    path.is_absolute() || path.components().count() > 1
}

fn candidate_is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };

    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        mode & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(windows)]
fn collect_windows_extensions(current_ext: Option<&OsStr>) -> Vec<OsString> {
    let mut exts = Vec::new();

    if let Some(ext) = current_ext {
        exts.push(ext.to_os_string());
    }

    if let Some(path_ext) = env::var_os("PATHEXT") {
        for ext in path_ext.split(';') {
            if !ext.trim().is_empty() {
                exts.push(OsString::from(ext));
            }
        }
    }

    if exts.is_empty() {
        exts.push(OsString::from(".exe"));
        exts.push(OsString::from(".com"));
        exts.push(OsString::from(".bat"));
        exts.push(OsString::from(".cmd"));
    }

    exts
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(unix))]
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn describe_missing_fallback_binary_lists_single_env() {
        let message =
            describe_missing_fallback_binary(OsStr::new("/usr/bin/rsync"), &["OC_RSYNC_FALLBACK"]);
        assert!(message.contains("install upstream rsync"));
        assert!(message.contains("OC_RSYNC_FALLBACK"));
        assert!(!message.contains(","));
    }

    #[test]
    fn describe_missing_fallback_binary_lists_multiple_envs() {
        let message = describe_missing_fallback_binary(
            OsStr::new("/usr/bin/rsync"),
            &["OC_RSYNC_DAEMON_FALLBACK", "OC_RSYNC_FALLBACK"],
        );
        assert!(message.contains("OC_RSYNC_DAEMON_FALLBACK"));
        assert!(message.contains("OC_RSYNC_FALLBACK"));
        assert!(message.contains("or"));
    }

    #[test]
    fn fallback_binary_available_detects_executable() {
        #[allow(unused_mut)]
        let mut temp = NamedTempFile::new().expect("tempfile");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = temp.as_file().metadata().expect("metadata").permissions();
            permissions.set_mode(0o755);
            temp.as_file().set_permissions(permissions).expect("chmod");
        }

        #[cfg(not(unix))]
        {
            writeln!(temp, "echo ok").expect("write");
        }

        assert!(fallback_binary_available(temp.path().as_os_str()));
    }

    #[test]
    fn fallback_binary_available_rejects_missing_file() {
        let missing = Path::new("/nonexistent/path/to/rsync-binary");
        assert!(!fallback_binary_available(missing.as_os_str()));
    }
}
