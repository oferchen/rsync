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
/// Matching rules:
/// - Must start with `.`
/// - Must contain a second `.` separating the basename from the random suffix
/// - The suffix after the last `.` must be exactly 6 alphanumeric characters
/// - The basename portion (between first `.` and last `.`) must be non-empty
fn is_temp_file_name(name: &str) -> bool {
    // Must start with '.'
    if !name.starts_with('.') {
        return false;
    }

    // Find the last '.' which separates basename from random suffix
    let last_dot = match name.rfind('.') {
        Some(pos) if pos > 0 => pos,
        _ => return false,
    };

    // The suffix after the last dot must be exactly SUFFIX_LEN alphanumeric chars
    let suffix = &name[last_dot + 1..];
    if suffix.len() != SUFFIX_LEN {
        return false;
    }
    if !suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }

    // The basename between leading dot and last dot must be non-empty.
    // For ".file.txt.AbCdEf", the part between index 1 and last_dot is "file.txt".
    // For ".bashrc.XyZ123", the part between index 1 and last_dot is "bashrc".
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
            Err(_) => continue, // Skip unreadable entries
        };

        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !is_temp_file_name(&name) {
            continue;
        }

        // Only remove regular files, not directories or symlinks
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue, // Skip if we cannot stat
        };
        if !metadata.is_file() {
            continue;
        }

        // Check age via mtime
        let mtime = match metadata.modified() {
            Ok(t) => t,
            Err(_) => continue, // Platform does not support mtime - skip
        };
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
            Err(_) => {
                // Best-effort - skip files we cannot remove (permissions, etc.)
                continue;
            }
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

    // === is_temp_file_name tests ===

    #[test]
    fn matches_standard_temp_pattern() {
        assert!(is_temp_file_name(".file.txt.AbCdEf"));
        assert!(is_temp_file_name(".data.bin.a1b2c3"));
        assert!(is_temp_file_name(".README.ABCDEF"));
        assert!(is_temp_file_name(".photo.jpg.Zz9Aa0"));
    }

    #[test]
    fn matches_dotfile_temp_pattern() {
        // Dotfiles consume the leading dot: .bashrc -> .bashrc.XXXXXX
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
        assert!(!is_temp_file_name(".file.txt.ABCDE")); // 5 chars
        assert!(!is_temp_file_name(".file.txt.ABCDEFG")); // 7 chars
        assert!(!is_temp_file_name(".file.txt.")); // 0 chars
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
        // "..ABCDEF" has empty basename between dots
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
        // ".file.tar.gz.AbCdEf" - basename is "file.tar.gz"
        assert!(is_temp_file_name(".file.tar.gz.AbCdEf"));
    }

    // === cleanup_stale_temp_files tests ===

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

        // Create temp files matching the pattern
        let temp1 = dir.path().join(".file.txt.AbCdEf");
        let temp2 = dir.path().join(".data.bin.x1y2z3");
        fs::write(&temp1, b"stale data 1").unwrap();
        fs::write(&temp2, b"stale data 2").unwrap();

        // Use Duration::ZERO to consider all files stale
        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::ZERO)).unwrap();
        assert_eq!(count, 2);
        assert!(!temp1.exists());
        assert!(!temp2.exists());
    }

    #[test]
    fn preserves_non_matching_files() {
        let dir = tempdir().expect("create temp dir");

        // Non-matching files
        let regular = dir.path().join("file.txt");
        let dotfile = dir.path().join(".bashrc");
        let wrong_suffix = dir.path().join(".file.txt.ABCDE"); // 5 chars
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

        // Create a temp file - it will be brand new (age ~0)
        let temp = dir.path().join(".recent.AbCdEf");
        fs::write(&temp, b"fresh data").unwrap();

        // Use a large threshold - file is too new to be removed
        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::from_secs(3600))).unwrap();
        assert_eq!(count, 0);
        assert!(temp.exists());

        // Now use Duration::ZERO - everything is stale
        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::ZERO)).unwrap();
        assert_eq!(count, 1);
        assert!(!temp.exists());
    }

    #[test]
    fn skips_directories_matching_pattern() {
        let dir = tempdir().expect("create temp dir");

        // Create a directory that matches the naming pattern
        let temp_dir = dir.path().join(".subdir.AbCdEf");
        fs::create_dir(&temp_dir).unwrap();

        let count = cleanup_stale_temp_files(dir.path(), Some(Duration::ZERO)).unwrap();
        assert_eq!(count, 0);
        assert!(temp_dir.exists());
    }

    #[test]
    fn mixed_matching_and_non_matching() {
        let dir = tempdir().expect("create temp dir");

        // Matching (stale)
        let stale = dir.path().join(".transfer.dat.Qw3rTy");
        fs::write(&stale, b"stale").unwrap();

        // Non-matching
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

        // Create a temp file in a subdirectory, then remove read permission
        // from the parent to simulate permission errors. We test the top-level
        // directory scan resilience by creating an unreadable file.
        let temp = dir.path().join(".secret.AbCdEf");
        fs::write(&temp, b"data").unwrap();

        // Make file read-only - removal should still work on most systems
        // since we own the directory. Instead, test that unreadable dirs
        // return an error gracefully.
        fs::set_permissions(&subdir, fs::Permissions::from_mode(0o000)).unwrap();

        // Scanning the restricted subdir should fail gracefully
        let result = cleanup_stale_temp_files(&subdir, Some(Duration::ZERO));
        // Restore permissions for cleanup
        fs::set_permissions(&subdir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err());
    }
}
