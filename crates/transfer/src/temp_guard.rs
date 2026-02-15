//! RAII guard for temporary file cleanup and upstream-compatible temp file naming.
//!
//! This module provides:
//! - [`TempFileGuard`]: RAII guard ensuring temporary files are deleted on error.
//! - [`open_tmpfile`]: Creates a temp file using upstream rsync's `.filename.XXXXXX`
//!   naming convention with `O_EXCL` atomicity.
//!
//! # Upstream Reference
//!
//! - `receiver.c:get_tmpname()` — temp file path construction
//! - `receiver.c:open_tmpfile()` → `syscall.c:do_mkstemp()` — atomic creation

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Length of the random suffix including the leading dot: `.XXXXXX` = 7 bytes.
const TMPNAME_SUFFIX_LEN: usize = 7;

/// Characters used for random suffix generation (alphanumeric, matching typical
/// `mkstemp` implementations).
const RAND_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Maximum attempts to find a unique temp file name before giving up.
const MAX_OPEN_ATTEMPTS: u32 = 100;

/// Maximum filename component length (NAME_MAX on most POSIX systems).
const NAME_MAX: usize = 255;

/// Generates a temporary file path following upstream rsync's naming convention.
///
/// The pattern is `.filename.XXXXXX` where `XXXXXX` is replaced with random
/// alphanumeric characters. This mirrors upstream `receiver.c:get_tmpname()`.
///
/// # Naming Rules (matching upstream)
///
/// - A leading dot is added to hide the temp file from directory listings.
/// - For dotfiles (`.bashrc`), the original leading dot is consumed to avoid
///   double-dot prefixes (upstream macOS compatibility).
/// - Long filenames are truncated to respect `NAME_MAX` (255), with UTF-8
///   multi-byte sequence safety.
/// - When `temp_dir` is provided, the temp file is placed there without an
///   extra leading dot (matching upstream `--temp-dir` behavior).
///
/// # Arguments
///
/// * `dest` — Final destination path for the file.
/// * `temp_dir` — Optional `--temp-dir` directory.
///
/// # Returns
///
/// A path template with a `.XXXXXX` suffix that must be filled with random chars
/// before use. Use [`open_tmpfile`] to atomically create the file.
fn get_tmpname(dest: &Path, temp_dir: Option<&Path>) -> io::Result<PathBuf> {
    let file_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rsync".to_owned());

    // Build temp filename: ".filename.XXXXXX"
    // For dotfiles, skip the leading dot to avoid double-dot (upstream macOS compat).
    // When using --temp-dir, no leading dot is added (file is already hidden in temp dir).
    let temp_name = if temp_dir.is_some() {
        // No leading dot when using temp_dir (mirrors upstream)
        format!("{file_name}.XXXXXX")
    } else {
        let name = if let Some(stripped) = file_name.strip_prefix('.') {
            // Dotfile: consume original dot, add our own → ".bashrc.XXXXXX"
            stripped
        } else {
            // Regular file: add leading dot → ".file.txt.XXXXXX"
            &file_name
        };
        format!(".{name}.XXXXXX")
    };

    // Truncate if the temp name exceeds NAME_MAX.
    // Reserve space for the suffix (.XXXXXX = 7 bytes) + leading dot (1 byte).
    let max_name_len = NAME_MAX;
    let truncated = if temp_name.len() > max_name_len {
        truncate_utf8_safe(&temp_name, max_name_len)
    } else {
        temp_name
    };

    // Place in temp_dir or same directory as destination
    let dir = temp_dir.unwrap_or_else(|| dest.parent().unwrap_or(Path::new(".")));
    Ok(dir.join(truncated))
}

/// Truncates a UTF-8 string to at most `max_len` bytes without splitting
/// multi-byte sequences, preserving the `.XXXXXX` suffix.
///
/// Mirrors upstream rsync's multi-byte truncation safety in `get_tmpname()`.
fn truncate_utf8_safe(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_owned();
    }

    // We need to keep the last TMPNAME_SUFFIX_LEN bytes (".XXXXXX")
    // and truncate the middle (the original filename part).
    let suffix = &s[s.len() - TMPNAME_SUFFIX_LEN..];
    let prefix_budget = max_len - TMPNAME_SUFFIX_LEN;

    // Find the largest UTF-8-safe prefix
    let prefix = &s[..s.len() - TMPNAME_SUFFIX_LEN];
    let mut safe_end = prefix_budget.min(prefix.len());
    while safe_end > 0 && !prefix.is_char_boundary(safe_end) {
        safe_end -= 1;
    }

    // Trim trailing dot to avoid double-dot before suffix (mirrors upstream)
    let trimmed = prefix[..safe_end].trim_end_matches('.');

    format!("{trimmed}{suffix}")
}

/// Fills the `XXXXXX` placeholder in a path template with random characters
/// and atomically creates the file using `O_EXCL`.
///
/// This is the Rust equivalent of upstream's `do_mkstemp()` which calls
/// `mkstemp(3)`. Each call produces a unique filename, so retries succeed
/// even if a previous temp file was not cleaned up.
///
/// # Arguments
///
/// * `dest` — Final destination path.
/// * `temp_dir` — Optional `--temp-dir`.
///
/// # Returns
///
/// A tuple of `(File, TempFileGuard)` — the open file handle and an RAII guard
/// that cleans up the temp file on drop unless `keep()` is called.
pub fn open_tmpfile(dest: &Path, temp_dir: Option<&Path>) -> io::Result<(fs::File, TempFileGuard)> {
    let template = get_tmpname(dest, temp_dir)?;
    let template_str = template.to_string_lossy().into_owned();

    // Replace XXXXXX with random chars, retry on collision (mirrors mkstemp behavior)
    for _ in 0..MAX_OPEN_ATTEMPTS {
        let concrete = fill_random_suffix(&template_str);
        let concrete_path = PathBuf::from(&concrete);

        match fs::OpenOptions::new()
            .write(true)
            .create_new(true) // O_EXCL — fail if exists
            .open(&concrete_path)
        {
            Ok(file) => {
                return Ok((file, TempFileGuard::new(concrete_path)));
            }
            Err(ref e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // Collision — try another random suffix
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("failed to create temp file after {MAX_OPEN_ATTEMPTS} attempts: {template_str}"),
    ))
}

/// Replaces the trailing `XXXXXX` in a template string with random alphanumeric
/// characters using `getrandom` for entropy.
fn fill_random_suffix(template: &str) -> String {
    // Generate 6 random bytes
    let mut random_bytes = [0u8; 6];
    getrandom::fill(&mut random_bytes).expect("getrandom failed");

    // Build suffix from random bytes
    let suffix: String = random_bytes
        .iter()
        .map(|&b| RAND_CHARS[(b as usize) % RAND_CHARS.len()] as char)
        .collect();

    // Replace trailing XXXXXX with random suffix
    let prefix = &template[..template.len() - 6];
    format!("{prefix}{suffix}")
}

/// RAII guard that ensures temp files are deleted on drop.
///
/// By default, the temp file is deleted when the guard is dropped (e.g., on
/// error or panic). Call [`keep()`](TempFileGuard::keep) after a successful
/// rename to prevent deletion.
#[derive(Debug)]
pub struct TempFileGuard {
    path: PathBuf,
    keep_on_drop: bool,
}

impl TempFileGuard {
    /// Create a new guard for the given temp file path.
    #[inline]
    pub const fn new(path: PathBuf) -> Self {
        Self {
            path,
            keep_on_drop: false,
        }
    }

    /// Mark the temp file as successful — don't delete on drop.
    #[inline]
    pub const fn keep(&mut self) {
        self.keep_on_drop = true;
    }

    /// Get the path to the temp file.
    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.keep_on_drop {
            // Best-effort cleanup — ignore errors since:
            // 1. File might not exist (never created)
            // 2. File might already be deleted (renamed away)
            // 3. We're in a drop context (can't propagate errors)
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // === TempFileGuard tests ===

    #[test]
    fn temp_file_deleted_on_drop() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("test.tmp");

        fs::write(&temp_path, b"test data").expect("write temp file");
        assert!(temp_path.exists());

        {
            let _guard = TempFileGuard::new(temp_path.clone());
        }

        assert!(!temp_path.exists());
    }

    #[test]
    fn temp_file_kept_when_keep_called() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("test.tmp");

        fs::write(&temp_path, b"test data").expect("write temp file");
        assert!(temp_path.exists());

        {
            let mut guard = TempFileGuard::new(temp_path.clone());
            guard.keep();
        }

        assert!(temp_path.exists());
    }

    #[test]
    fn temp_file_deleted_on_panic() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("test.tmp");

        fs::write(&temp_path, b"test data").expect("write temp file");
        assert!(temp_path.exists());

        let result = std::panic::catch_unwind(|| {
            let _guard = TempFileGuard::new(temp_path.clone());
            panic!("simulated panic");
        });

        assert!(result.is_err());
        assert!(!temp_path.exists());
    }

    #[test]
    fn temp_file_deleted_on_error_return() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("test.tmp");

        fs::write(&temp_path, b"test data").expect("write temp file");
        assert!(temp_path.exists());

        fn operation_that_fails(path: PathBuf) -> Result<(), std::io::Error> {
            let _guard = TempFileGuard::new(path);
            Err(std::io::Error::other("operation failed"))
        }

        let result = operation_that_fails(temp_path.clone());
        assert!(result.is_err());
        assert!(!temp_path.exists());
    }

    #[test]
    fn path_returns_correct_path() {
        let temp_path = PathBuf::from("/tmp/test.tmp");
        let guard = TempFileGuard::new(temp_path);
        assert_eq!(guard.path(), Path::new("/tmp/test.tmp"));
    }

    #[test]
    fn guard_handles_nonexistent_file() {
        let temp_path = PathBuf::from("/tmp/nonexistent.tmp");
        {
            let _guard = TempFileGuard::new(temp_path);
        }
    }

    // === get_tmpname tests ===

    #[test]
    fn tmpname_regular_file() {
        let dest = Path::new("/path/to/file.txt");
        let result = get_tmpname(dest, None).unwrap();
        let name = result.file_name().unwrap().to_string_lossy();
        // Should be ".file.txt.XXXXXX"
        assert!(name.starts_with(".file.txt."), "got: {name}");
        assert!(name.ends_with("XXXXXX"), "got: {name}");
        assert_eq!(result.parent().unwrap(), Path::new("/path/to"));
    }

    #[test]
    fn tmpname_dotfile_no_double_dot() {
        let dest = Path::new("/home/user/.bashrc");
        let result = get_tmpname(dest, None).unwrap();
        let name = result.file_name().unwrap().to_string_lossy();
        // Should be ".bashrc.XXXXXX" (not "..bashrc.XXXXXX")
        assert!(name.starts_with(".bashrc."), "got: {name}");
        assert!(!name.starts_with(".."), "double dot: {name}");
        assert!(name.ends_with("XXXXXX"), "got: {name}");
    }

    #[test]
    fn tmpname_with_temp_dir() {
        let dest = Path::new("/path/to/file.txt");
        let temp_dir = Path::new("/tmp/rsync");
        let result = get_tmpname(dest, Some(temp_dir)).unwrap();
        // Should be in temp_dir without extra leading dot
        assert!(result.starts_with(temp_dir));
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("file.txt."), "got: {name}");
        assert!(
            !name.starts_with('.'),
            "unexpected dot with temp_dir: {name}"
        );
    }

    #[test]
    fn tmpname_preserves_directory() {
        let dest = Path::new("/some/deep/path/data.bin");
        let result = get_tmpname(dest, None).unwrap();
        assert_eq!(result.parent().unwrap(), Path::new("/some/deep/path"));
    }

    #[test]
    fn tmpname_long_filename_truncated() {
        // Create a filename that exceeds NAME_MAX with the suffix
        let long_name = "a".repeat(260);
        let dest = PathBuf::from(format!("/path/{long_name}"));
        let result = get_tmpname(&dest, None).unwrap();
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(
            name.len() <= NAME_MAX,
            "name too long: {} > {NAME_MAX}",
            name.len()
        );
        assert!(name.ends_with("XXXXXX"), "suffix lost: {name}");
        assert!(name.starts_with('.'), "leading dot lost: {name}");
    }

    #[test]
    fn tmpname_utf8_truncation_safe() {
        // Create a filename with multi-byte UTF-8 chars near the truncation boundary
        // Each emoji is 4 bytes
        let emojis = "\u{1F600}".repeat(65); // 260 bytes
        let dest = PathBuf::from(format!("/path/{emojis}"));
        let result = get_tmpname(&dest, None).unwrap();
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(name.len() <= NAME_MAX);
        assert!(name.ends_with("XXXXXX"));
        // Verify it's valid UTF-8 (would panic on to_string_lossy if not)
        assert!(!name.contains('\u{FFFD}'), "broken UTF-8: {name}");
    }

    #[test]
    fn tmpname_no_filename() {
        let dest = Path::new("/");
        let result = get_tmpname(dest, None).unwrap();
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(".rsync."), "got: {name}");
    }

    // === fill_random_suffix tests ===

    #[test]
    fn fill_random_suffix_replaces_xs() {
        let template = "/path/to/.file.txt.XXXXXX";
        let result = fill_random_suffix(template);
        assert!(!result.ends_with("XXXXXX"), "Xs not replaced: {result}");
        assert_eq!(result.len(), template.len());
        // Verify prefix is preserved
        assert!(result.starts_with("/path/to/.file.txt."));
    }

    #[test]
    fn fill_random_suffix_produces_unique_names() {
        let template = "/tmp/.test.XXXXXX";
        let names: Vec<String> = (0..10).map(|_| fill_random_suffix(template)).collect();
        // With 62^6 = ~56 billion possibilities, duplicates are near-impossible
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert!(unique.len() > 1, "all names identical: {names:?}");
    }

    #[test]
    fn fill_random_suffix_uses_valid_chars() {
        let template = ".test.XXXXXX";
        for _ in 0..100 {
            let result = fill_random_suffix(template);
            let suffix = &result[result.len() - 6..];
            for c in suffix.chars() {
                assert!(
                    c.is_ascii_alphanumeric(),
                    "invalid char '{c}' in suffix: {result}"
                );
            }
        }
    }

    // === open_tmpfile tests ===

    #[test]
    fn open_tmpfile_creates_file() {
        let dir = tempdir().expect("create temp dir");
        let dest = dir.path().join("file.txt");

        let (file, mut guard) = open_tmpfile(&dest, None).unwrap();
        drop(file);

        let temp_path = guard.path().to_path_buf();
        assert!(temp_path.exists());

        let name = temp_path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(".file.txt."), "got: {name}");
        assert!(!name.ends_with("XXXXXX"), "template not filled: {name}");
        assert_eq!(temp_path.parent().unwrap(), dir.path());

        // Guard cleanup
        guard.keep();
        fs::remove_file(&temp_path).ok();
    }

    #[test]
    fn open_tmpfile_unique_per_call() {
        let dir = tempdir().expect("create temp dir");
        let dest = dir.path().join("file.txt");

        let (_f1, mut g1) = open_tmpfile(&dest, None).unwrap();
        let (_f2, mut g2) = open_tmpfile(&dest, None).unwrap();

        assert_ne!(g1.path(), g2.path(), "two calls produced same path");

        g1.keep();
        g2.keep();
    }

    #[test]
    fn open_tmpfile_with_temp_dir() {
        let dest_dir = tempdir().expect("dest dir");
        let tmp_dir = tempdir().expect("temp dir");
        let dest = dest_dir.path().join("file.txt");

        let (_file, mut guard) = open_tmpfile(&dest, Some(tmp_dir.path())).unwrap();

        assert!(guard.path().starts_with(tmp_dir.path()));
        let name = guard.path().file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("file.txt."), "got: {name}");

        guard.keep();
    }

    #[test]
    fn open_tmpfile_cleanup_on_drop() {
        let dir = tempdir().expect("create temp dir");
        let dest = dir.path().join("data.bin");

        let temp_path;
        {
            let (_file, guard) = open_tmpfile(&dest, None).unwrap();
            temp_path = guard.path().to_path_buf();
            assert!(temp_path.exists());
            // guard dropped here — file should be deleted
        }

        assert!(!temp_path.exists());
    }

    // === truncate_utf8_safe tests ===

    #[test]
    fn truncate_short_string_unchanged() {
        let s = ".file.XXXXXX";
        assert_eq!(truncate_utf8_safe(s, 255), s);
    }

    #[test]
    fn truncate_preserves_suffix() {
        let name = format!(".{}.XXXXXX", "a".repeat(300));
        let result = truncate_utf8_safe(&name, 255);
        assert!(result.len() <= 255);
        assert!(result.ends_with(".XXXXXX"));
        assert!(result.starts_with('.'));
    }

    #[test]
    fn truncate_no_split_multibyte() {
        // 2-byte UTF-8 chars (é = 0xC3 0xA9)
        let chars = "é".repeat(130); // 260 bytes
        let name = format!(".{chars}.XXXXXX");
        let result = truncate_utf8_safe(&name, 255);
        assert!(result.len() <= 255);
        assert!(result.ends_with(".XXXXXX"));
        // Verify no replacement char
        assert!(!result.contains('\u{FFFD}'));
    }
}
