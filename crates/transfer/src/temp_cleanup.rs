//! Startup cleanup for stale temporary files left by interrupted transfers.
//!
//! When a transfer is killed (SIGKILL, OOM, power loss), the RAII guard in
//! [`super::temp_guard::TempFileGuard`] never runs, leaving `.filename.XXXXXX`
//! temp files on disk. This module scans a destination directory at transfer
//! startup and removes temp files older than a configurable age threshold.
//!
//! # Upstream Reference
//!
//! - `cleanup.c` - upstream rsync cleanup of partial/temp files on abnormal exit

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use logging::debug_log;

/// Default age threshold for considering a temp file stale: 24 hours.
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Length of the random alphanumeric suffix (excluding the leading dot separator).
const SUFFIX_LEN: usize = 6;

/// Returns `true` if `name` matches the temp file pattern produced by
/// [`super::temp_guard::open_tmpfile`].
///
/// The pattern is `.{basename}.XXXXXX` where `XXXXXX` is exactly 6
/// alphanumeric characters from `RAND_CHARS` (`A-Za-z0-9`). For dotfiles
/// the original leading dot is consumed, so `.bashrc.XXXXXX` is also valid.
///
/// Note: when `--temp-dir` is active, temp files use `{name}.XXXXXX` (no
/// leading dot) and are placed in a separate directory. This function only
/// covers the default in-destination pattern.
///
/// Matching rules:
/// - Must start with `.`
/// - Must contain a second `.` separating the basename from the random suffix
/// - The suffix after the last `.` must be exactly 6 alphanumeric characters
/// - The basename portion (between first `.` and last `.`) must be non-empty
fn is_temp_file_name(name: &str) -> bool {
    if !name.starts_with('.') {
        return false;
    }

    let last_dot = match name.rfind('.') {
        Some(pos) if pos > 0 => pos,
        _ => return false,
    };

    let suffix = &name[last_dot + 1..];
    if suffix.len() != SUFFIX_LEN {
        return false;
    }
    if !suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }

    // For ".file.txt.AbCdEf" the basename is "file.txt"; for ".bashrc.XyZ123" it is "bashrc".
    let basename = &name[1..last_dot];
    if basename.is_empty() {
        return false;
    }

    true
}

/// Removes stale temporary files from `dest_dir` that were left behind by
/// interrupted transfers.
///
/// Scans directory entries for files matching the `.{name}.XXXXXX` pattern
/// created by [`super::temp_guard::open_tmpfile`] and removes those whose
/// modification time is older than `max_age`. Returns the count of removed
/// files.
///
/// This function is best-effort - it skips entries that cannot be read or
/// deleted (e.g., due to permission errors) without aborting the scan.
///
/// # Arguments
///
/// * `dest_dir` - Directory to scan for stale temp files.
/// * `max_age` - Only remove files older than this duration. Pass `None` to
///   use the default of 24 hours.
///
/// # Upstream Reference
///
/// upstream: `cleanup.c` - cleanup of partial transfers on abnormal exit.
/// We perform this proactively at startup rather than only on signal, since
/// SIGKILL bypasses all cleanup handlers.
pub fn cleanup_stale_temp_files(dest_dir: &Path, max_age: Option<Duration>) -> io::Result<usize> {
    let threshold = max_age.unwrap_or(DEFAULT_MAX_AGE);
    let now = SystemTime::now();
    let mut removed = 0;

    let entries = match fs::read_dir(dest_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !is_temp_file_name(&name) {
            continue;
        }

        // file_type() is lstat-equivalent so symlinks named like temp files are skipped.
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !file_type.is_file() {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = match metadata.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        // Treat future-dated files (clock skew) as age zero so they are kept.
        let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
        if age < threshold {
            continue;
        }

        let path = entry.path();
        match fs::remove_file(&path) {
            Ok(()) => {
                debug_log!(Exit, 2, "removed stale temp file: {}", path.display());
                removed += 1;
            }
            Err(_) => continue,
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn matches_standard_temp_pattern() {
        assert!(is_temp_file_name(".file.txt.AbCdEf"));
        assert!(is_temp_file_name(".data.bin.a1b2c3"));
        assert!(is_temp_file_name(".README.ABCDEF"));
        assert!(is_temp_file_name(".photo.jpg.Zz9Aa0"));
    }

    #[test]
    fn matches_dotfile_temp_pattern() {
        // Dotfiles consume the leading dot: .bashrc becomes .bashrc.XXXXXX.
        assert!(is_temp_file_name(".bashrc.AbCdEf"));
        assert!(is_temp_file_name(".gitignore.x1y2z3"));
    }

    #[test]
    fn rejects_non_dot_prefix() {
        assert!(!is_temp_file_name("file.txt.AbCdEf"));
        assert!(!is_temp_file_name("noprefix.ABCDEF"));
    }

    #[test]
    fn rejects_wrong_suffix_length() {
        assert!(!is_temp_file_name(".file.txt.ABCDE"));
        assert!(!is_temp_file_name(".file.txt.ABCDEFG"));
        assert!(!is_temp_file_name(".file.txt."));
    }

    #[test]
    fn rejects_non_alphanumeric_suffix() {
        assert!(!is_temp_file_name(".file.txt.abc-ef"));
        assert!(!is_temp_file_name(".file.txt.abc_ef"));
        assert!(!is_temp_file_name(".file.txt.abc ef"));
        assert!(!is_temp_file_name(".file.txt.abc.ef"));
    }

    #[test]
    fn rejects_empty_basename() {
        assert!(!is_temp_file_name("..ABCDEF"));
    }

    #[test]
    fn rejects_regular_dotfiles() {
        assert!(!is_temp_file_name(".bashrc"));
        assert!(!is_temp_file_name(".gitignore"));
        assert!(!is_temp_file_name(".DS_Store"));
    }

    #[test]
    fn rejects_regular_files() {
        assert!(!is_temp_file_name("file.txt"));
        assert!(!is_temp_file_name("data.bin"));
    }

    #[test]
    fn matches_multi_dot_basename() {
        // For ".file.tar.gz.AbCdEf" the basename is "file.tar.gz".
        assert!(is_temp_file_name(".file.tar.gz.AbCdEf"));
    }

    #[test]
    fn empty_directory_returns_zero() {
        let dir = tempdir().expect("create temp dir");
        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::ZERO)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn nonexistent_directory_returns_zero() {
        let dir = tempdir().expect("create temp dir");
        let missing = dir.path().join("does_not_exist");
        let count = cleanup_stale_temp_files(&missing, Some(Duration::ZERO)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn removes_stale_temp_files() {
        let dir = tempdir().expect("create temp dir");

        let temp1 = dir.path().join(".file.txt.AbCdEf");
        let temp2 = dir.path().join(".data.bin.x1y2z3");
        fs::write(&temp1, b"stale data 1").unwrap();
        fs::write(&temp2, b"stale data 2").unwrap();

        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::ZERO)).unwrap();
        assert_eq!(count, 2);
        assert!(!temp1.exists());
        assert!(!temp2.exists());
    }

    #[test]
    fn preserves_non_matching_files() {
        let dir = tempdir().expect("create temp dir");

        let regular = dir.path().join("file.txt");
        let dotfile = dir.path().join(".bashrc");
        let wrong_suffix = dir.path().join(".file.txt.ABCDE");
        fs::write(&regular, b"keep").unwrap();
        fs::write(&dotfile, b"keep").unwrap();
        fs::write(&wrong_suffix, b"keep").unwrap();

        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::ZERO)).unwrap();
        assert_eq!(count, 0);
        assert!(regular.exists());
        assert!(dotfile.exists());
        assert!(wrong_suffix.exists());
    }

    #[test]
    fn respects_age_threshold() {
        let dir = tempdir().expect("create temp dir");

        let temp = dir.path().join(".recent.AbCdEf");
        fs::write(&temp, b"fresh data").unwrap();

        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::from_secs(3600))).unwrap();
        assert_eq!(count, 0);
        assert!(temp.exists());

        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::ZERO)).unwrap();
        assert_eq!(count, 1);
        assert!(!temp.exists());
    }

    #[test]
    fn skips_directories_matching_pattern() {
        let dir = tempdir().expect("create temp dir");

        let temp_dir = dir.path().join(".subdir.AbCdEf");
        fs::create_dir(&temp_dir).unwrap();

        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::ZERO)).unwrap();
        assert_eq!(count, 0);
        assert!(temp_dir.exists());
    }

    #[test]
    fn mixed_matching_and_non_matching() {
        let dir = tempdir().expect("create temp dir");

        let stale = dir.path().join(".transfer.dat.Qw3rTy");
        fs::write(&stale, b"stale").unwrap();

        let normal = dir.path().join("important.dat");
        fs::write(&normal, b"keep").unwrap();

        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::ZERO)).unwrap();
        assert_eq!(count, 1);
        assert!(!stale.exists());
        assert!(normal.exists());
    }

    #[cfg(unix)]
    #[test]
    fn handles_permission_errors_gracefully() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("create temp dir");
        let subdir = dir.path().join("restricted");
        fs::create_dir(&subdir).unwrap();

        let temp = dir.path().join(".secret.AbCdEf");
        fs::write(&temp, b"data").unwrap();

        fs::set_permissions(&subdir, fs::Permissions::from_mode(0o000)).unwrap();

        let result = cleanup_stale_temp_files(&subdir, Some(Duration::ZERO));
        fs::set_permissions(&subdir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err());
    }
}
