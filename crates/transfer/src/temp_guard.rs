//! RAII guard for temporary file cleanup and upstream-compatible temp file naming.
//!
//! This module provides:
//! - `TempFileGuard`: RAII guard ensuring temporary files are deleted on error.
//! - `open_tmpfile`: Creates a temp file using upstream rsync's `.filename.XXXXXX`
//!   naming convention with `O_EXCL` atomicity.
//!
//! # Upstream Reference
//!
//! - `receiver.c:get_tmpname()` - temp file path construction
//! - `receiver.c:open_tmpfile()` → `syscall.c:do_mkstemp()` - atomic creation

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;

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
/// * `dest` - Final destination path for the file.
/// * `temp_dir` - Optional `--temp-dir` directory.
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

    // upstream: receiver.c:get_tmpname() - ".filename.XXXXXX" convention.
    // No leading dot when using --temp-dir. Dotfiles consume original dot.
    let temp_name = if temp_dir.is_some() {
        format!("{file_name}.XXXXXX")
    } else {
        let name = if let Some(stripped) = file_name.strip_prefix('.') {
            stripped
        } else {
            &file_name
        };
        format!(".{name}.XXXXXX")
    };

    let max_name_len = NAME_MAX;
    let truncated = if temp_name.len() > max_name_len {
        truncate_utf8_safe(&temp_name, max_name_len)
    } else {
        temp_name
    };

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

    let suffix = &s[s.len() - TMPNAME_SUFFIX_LEN..];
    let prefix_budget = max_len - TMPNAME_SUFFIX_LEN;
    let prefix = &s[..s.len() - TMPNAME_SUFFIX_LEN];
    let mut safe_end = prefix_budget.min(prefix.len());
    while safe_end > 0 && !prefix.is_char_boundary(safe_end) {
        safe_end -= 1;
    }

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
/// * `dest` - Final destination path.
/// * `temp_dir` - Optional `--temp-dir`.
///
/// # Returns
///
/// A tuple of `(File, TempFileGuard)` - the open file handle and an RAII guard
/// that cleans up the temp file on drop unless `keep()` is called.
pub fn open_tmpfile(dest: &Path, temp_dir: Option<&Path>) -> io::Result<(fs::File, TempFileGuard)> {
    open_tmpfile_inner(
        dest,
        temp_dir,
        #[cfg(unix)]
        None,
        #[cfg(unix)]
        None,
    )
}

/// Like [`open_tmpfile`] but routes the create through the SEC-1.r
/// `DirSandbox` carrier when both the temp parent and the temp leaf reduce
/// to a single component beneath `dest_dir`.
///
/// The returned [`TempFileGuard`] inherits the same sandbox anchor so its
/// Drop cleanup runs through `unlinkat(sandbox.current_dirfd(), leaf, 0)`,
/// closing the symlink-swap window between the receiver's decide-to-create
/// moment and the eventual unlink. When the sandbox is absent or the temp
/// parent does not match `dest_dir` (for example `--temp-dir` pointing at a
/// sibling tree), the helper falls back to the path-based open and the
/// guard falls back to `std::fs::remove_file`.
#[cfg(unix)]
pub fn open_tmpfile_sandboxed(
    dest: &Path,
    temp_dir: Option<&Path>,
    sandbox: Option<&Arc<fast_io::DirSandbox>>,
    dest_dir: Option<&Path>,
) -> io::Result<(fs::File, TempFileGuard)> {
    open_tmpfile_inner(dest, temp_dir, sandbox, dest_dir)
}

fn open_tmpfile_inner(
    dest: &Path,
    temp_dir: Option<&Path>,
    #[cfg(unix)] sandbox: Option<&Arc<fast_io::DirSandbox>>,
    #[cfg(unix)] dest_dir: Option<&Path>,
) -> io::Result<(fs::File, TempFileGuard)> {
    let template = get_tmpname(dest, temp_dir)?;
    let template_str = template.to_string_lossy().into_owned();

    for _ in 0..MAX_OPEN_ATTEMPTS {
        let concrete = fill_random_suffix(&template_str);
        let concrete_path = PathBuf::from(&concrete);

        match try_create_new(
            &concrete_path,
            #[cfg(unix)]
            sandbox,
            #[cfg(unix)]
            dest_dir,
        ) {
            Ok(file) => {
                #[cfg(unix)]
                let guard = TempFileGuard::with_anchor(concrete_path.clone(), sandbox, dest_dir);
                #[cfg(not(unix))]
                let guard = TempFileGuard::new(concrete_path);
                return Ok((file, guard));
            }
            Err(ref e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(ref e) if e.kind() == io::ErrorKind::NotFound => {
                // upstream: receiver.c:283-293 - the ENOENT-recovery that
                // called `make_path(fnametmp, ...)` before re-`do_mkstemp()`
                // is compiled out (`#if 0`). Upstream never creates a missing
                // parent chain at temp-file open time; a missing destination
                // ancestor is a hard error (`mkstemp %s failed`). In-tree
                // subdirectories are created up-front by the receiver's
                // directory pass (generator.c:1317-1326 / create_directories),
                // and the dest-arg path is created only under `--mkpath` in
                // `ensure_dest_root_exists` (upstream main.c:736). Auto-creating
                // here would resurrect the no-`--mkpath` deep-path bug, so we
                // surface the ENOENT verbatim to match upstream.
                return Err(io::Error::new(e.kind(), e.to_string()));
            }
            Err(e) => return Err(e),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("failed to create temp file after {MAX_OPEN_ATTEMPTS} attempts: {template_str}"),
    ))
}

/// Atomically creates `concrete_path` with `O_RDWR | O_CREAT | O_EXCL |
/// O_NOFOLLOW` semantics, routing through the SEC-1.r sandbox carrier when
/// possible.
fn try_create_new(
    concrete_path: &Path,
    #[cfg(unix)] sandbox: Option<&Arc<fast_io::DirSandbox>>,
    #[cfg(unix)] dest_dir: Option<&Path>,
) -> io::Result<fs::File> {
    #[cfg(unix)]
    {
        if let (Some(sandbox), Some(dest_dir)) = (sandbox, dest_dir) {
            if let Some(leaf_name) = concrete_path.file_name() {
                let parent = concrete_path.parent().unwrap_or(Path::new(""));
                if parent == dest_dir {
                    let relative = Path::new(leaf_name);
                    // Mirror the fallback's `OpenOptions::new().write(true).create_new(true)`
                    // exactly: `O_WRONLY | O_CREAT | O_EXCL`, plus `O_NOFOLLOW` so a
                    // pre-planted symlink at the leaf path cannot redirect the create.
                    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW;
                    return fast_io::openat_via_sandbox_or_fallback(
                        Some(sandbox.as_ref()),
                        dest_dir,
                        relative,
                        concrete_path,
                        flags,
                        0o600,
                    );
                }
            }
        }
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(concrete_path)
}

/// Replaces the trailing `XXXXXX` in a template string with random alphanumeric
/// characters using `getrandom` for entropy.
fn fill_random_suffix(template: &str) -> String {
    let mut random_bytes = [0u8; 6];
    getrandom::fill(&mut random_bytes).expect("getrandom failed");

    let suffix: String = random_bytes
        .iter()
        .map(|&b| RAND_CHARS[(b as usize) % RAND_CHARS.len()] as char)
        .collect();

    let prefix = &template[..template.len() - 6];
    format!("{prefix}{suffix}")
}

/// SEC-1.r sandbox anchor carried by [`TempFileGuard`] so the Drop unlink
/// runs through `unlinkat(sandbox.current_dirfd(), leaf, 0)` against the
/// same parent the create resolved against.
#[cfg(unix)]
#[derive(Debug)]
struct SandboxAnchor {
    sandbox: Arc<fast_io::DirSandbox>,
    dest_dir: PathBuf,
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
    /// When `true`, this path was registered with the global
    /// [`engine::CleanupManager`] and Drop must remove it from the registry
    /// after the file is deleted-or-committed, so an errored transfer does not
    /// leak its `PathBuf` into the process-global set forever.
    ///
    /// [`mark_registered`]: TempFileGuard::mark_registered
    registered: bool,
    /// SEC-1.r sandbox anchor: when present, Drop routes the unlink through
    /// `unlinkat(sandbox.current_dirfd(), leaf, 0)` so a symlink swap on the
    /// temp parent between create and unlink cannot redirect the cleanup.
    #[cfg(unix)]
    anchor: Option<SandboxAnchor>,
}

impl TempFileGuard {
    /// Create a new guard for the given temp file path.
    ///
    /// The Drop cleanup uses [`std::fs::remove_file`] against the stored
    /// path. Use [`open_tmpfile_sandboxed`] to install a SEC-1.r sandbox
    /// anchor that routes the unlink through `unlinkat`.
    #[inline]
    pub const fn new(path: PathBuf) -> Self {
        Self {
            path,
            keep_on_drop: false,
            registered: false,
            #[cfg(unix)]
            anchor: None,
        }
    }

    /// Create a guard for an in-place / device write target that must never
    /// be unlinked on a mid-transfer error.
    ///
    /// Unlike [`new`](TempFileGuard::new), `path` is the real destination
    /// file (not a temp file), so the guard is constructed already
    /// keep-on-drop: an aborted `--inplace` or device transfer leaves the
    /// partial write in place instead of deleting the user's existing file.
    /// This mirrors upstream `receiver.c:1054`, which gates the destination
    /// unlink on `!one_inplace` and so never unlinks an in-place target.
    /// The success path still calls [`keep`](TempFileGuard::keep), which is
    /// an idempotent no-op here.
    #[inline]
    pub const fn keep_dest(path: PathBuf) -> Self {
        Self {
            path,
            keep_on_drop: true,
            registered: false,
            #[cfg(unix)]
            anchor: None,
        }
    }

    /// Create a new guard with an optional SEC-1.r sandbox anchor.
    ///
    /// When `sandbox` and `dest_dir` are both `Some` and the temp file lives
    /// directly beneath `dest_dir`, the guard's Drop unlinks the file through
    /// `unlinkat(sandbox.current_dirfd(), leaf, 0)` instead of the path-based
    /// `std::fs::remove_file`. Otherwise the guard falls back to the
    /// path-based cleanup.
    #[cfg(unix)]
    fn with_anchor(
        path: PathBuf,
        sandbox: Option<&Arc<fast_io::DirSandbox>>,
        dest_dir: Option<&Path>,
    ) -> Self {
        let anchor = match (sandbox, dest_dir) {
            (Some(sandbox), Some(dest_dir)) => {
                let parent = path.parent().unwrap_or(Path::new(""));
                if parent == dest_dir {
                    Some(SandboxAnchor {
                        sandbox: Arc::clone(sandbox),
                        dest_dir: dest_dir.to_path_buf(),
                    })
                } else {
                    None
                }
            }
            _ => None,
        };
        Self {
            path,
            keep_on_drop: false,
            registered: false,
            anchor,
        }
    }

    /// Mark the temp file as successful - don't delete on drop.
    #[inline]
    pub const fn keep(&mut self) {
        self.keep_on_drop = true;
    }

    /// Record that this guard's path was registered with the global
    /// [`engine::CleanupManager`].
    ///
    /// Callers that register the temp path for signal-handler cleanup must
    /// call this so the guard's Drop removes the path from the registry once
    /// the file has been deleted-or-committed. This closes the leak where an
    /// errored transfer's `PathBuf` stayed in the process-global set forever
    /// in a long-running daemon.
    ///
    /// The signal-handler contract is preserved: the entry is removed only in
    /// Drop, after the file is unlinked (error path) or has been committed
    /// (kept path), so a SIGINT before Drop still finds the in-flight temp
    /// registered and cleans it up.
    #[inline]
    pub const fn mark_registered(&mut self) {
        self.registered = true;
    }

    /// Get the path to the temp file.
    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Renames the temp file into a partial directory for later resume.
    ///
    /// Computes the target path as `partial_dir / filename` where `filename`
    /// is derived from `dest_path` (the final destination). Creates the
    /// partial directory if it does not exist.
    ///
    /// On success, marks the guard as kept (no deletion on drop) and returns
    /// `Ok(partial_path)`. On failure, returns the I/O error - the caller
    /// should let the guard drop normally to clean up the temp file.
    ///
    /// # Upstream Reference
    ///
    /// - `cleanup.c:105-115` - `handle_partial_dir()` constructs the partial
    ///   path and calls `do_rename()` to move the temp file there.
    /// - `util1.c:robust_rename()` - cross-device fallback with copy+remove.
    pub fn rename_to_partial_dir(
        &mut self,
        dest_path: &Path,
        partial_dir: &Path,
    ) -> io::Result<PathBuf> {
        let partial_path = partial_dir_fname(dest_path, partial_dir).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no filename")
        })?;

        // Create the partial directory if it does not exist.
        if let Some(parent) = partial_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }

        // Rename temp file to the partial path, with cross-device fallback.
        match fs::rename(self.path(), &partial_path) {
            Ok(()) => {
                self.keep_on_drop = true;
                Ok(partial_path)
            }
            Err(ref e) if is_cross_device_error(e) => {
                // upstream: util1.c:robust_rename() - copy + unlink on EXDEV
                fs::copy(self.path(), &partial_path)?;
                // The guard's drop will remove the source temp file.
                self.keep_on_drop = true;
                let _ = fs::remove_file(self.path());
                Ok(partial_path)
            }
            Err(e) => Err(e),
        }
    }
}

/// Computes the partial-dir basis path `partial_dir / <dest-basename>` for a
/// destination file, mirroring upstream `util1.c:partial_dir_fname()`.
///
/// For an absolute `partial_dir` the result is `partial_dir/<basename>`; for a
/// relative one it is placed under the destination's own parent directory
/// (`<dest-parent>/partial_dir/<basename>`), exactly as upstream builds
/// `partialptr` from `fname`. Returns `None` when `dest_path` has no filename.
///
/// # Upstream Reference
///
/// - `util1.c:1300` - `partial_dir_fname()` joins `partial_dir` and the basename
/// - `generator.c:1759` - `partialptr = partial_dir_fname(fname)` basis lookup
pub fn partial_dir_fname(dest_path: &Path, partial_dir: &Path) -> Option<PathBuf> {
    let file_name = dest_path.file_name()?;
    let partial_path = if partial_dir.is_absolute() {
        partial_dir.join(file_name)
    } else {
        let parent = dest_path.parent().unwrap_or(Path::new("."));
        parent.join(partial_dir).join(file_name)
    };
    Some(partial_path)
}

/// Returns `true` when an I/O error represents a cross-device link (EXDEV).
fn is_cross_device_error(e: &io::Error) -> bool {
    match e.raw_os_error() {
        #[cfg(unix)]
        Some(code) => code == libc::EXDEV,
        #[cfg(windows)]
        Some(code) => code == 17, // ERROR_NOT_SAME_DEVICE
        #[cfg(not(any(unix, windows)))]
        Some(_) => false,
        None => false,
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        // Delete the temp file first (unless kept), then remove the path from
        // the global cleanup registry. Ordering matters: a SIGINT arriving
        // before this Drop still finds the in-flight temp registered and
        // cleans it; once the file is unlinked-or-committed here it no longer
        // needs a registry entry, so leaving it in would leak the PathBuf.
        if self.keep_on_drop {
            self.unregister();
            return;
        }
        // Best-effort: the file may never have been created, may already be renamed
        // away, and we cannot propagate errors from drop anyway.
        #[cfg(unix)]
        {
            if let Some(anchor) = self.anchor.as_ref() {
                if let Some(leaf) = self.path.file_name() {
                    let relative = Path::new(leaf);
                    let _ = fast_io::unlink_via_sandbox_or_fallback(
                        Some(anchor.sandbox.as_ref()),
                        &anchor.dest_dir,
                        relative,
                        &self.path,
                        fast_io::UnlinkFlags::File,
                    );
                    self.unregister();
                    return;
                }
            }
        }
        let _ = std::fs::remove_file(&self.path);
        self.unregister();
    }
}

impl TempFileGuard {
    /// Removes this guard's path from the global [`engine::CleanupManager`]
    /// registry when it was registered. Idempotent: `HashSet::remove` is a
    /// no-op for an absent key, so a double-unregister (e.g. an explicit
    /// pre-`keep` unregister followed by this Drop) is harmless.
    #[inline]
    fn unregister(&self) {
        if self.registered {
            engine::CleanupManager::global().unregister_temp_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

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

    #[test]
    fn tmpname_regular_file() {
        let dest = Path::new("/path/to/file.txt");
        let result = get_tmpname(dest, None).unwrap();
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(".file.txt."), "got: {name}");
        assert!(name.ends_with("XXXXXX"), "got: {name}");
        assert_eq!(result.parent().unwrap(), Path::new("/path/to"));
    }

    #[test]
    fn tmpname_dotfile_no_double_dot() {
        let dest = Path::new("/home/user/.bashrc");
        let result = get_tmpname(dest, None).unwrap();
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(".bashrc."), "got: {name}");
        assert!(!name.starts_with(".."), "double dot: {name}");
        assert!(name.ends_with("XXXXXX"), "got: {name}");
    }

    #[test]
    fn tmpname_with_temp_dir() {
        let dest = Path::new("/path/to/file.txt");
        let temp_dir = Path::new("/tmp/rsync");
        let result = get_tmpname(dest, Some(temp_dir)).unwrap();
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
        // Each emoji is 4 bytes, so 65 of them puts the truncation boundary mid-codepoint.
        let emojis = "\u{1F600}".repeat(65);
        let dest = PathBuf::from(format!("/path/{emojis}"));
        let result = get_tmpname(&dest, None).unwrap();
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(name.len() <= NAME_MAX);
        assert!(name.ends_with("XXXXXX"));
        assert!(!name.contains('\u{FFFD}'), "broken UTF-8: {name}");
    }

    #[test]
    fn tmpname_no_filename() {
        let dest = Path::new("/");
        let result = get_tmpname(dest, None).unwrap();
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(".rsync."), "got: {name}");
    }

    #[test]
    fn fill_random_suffix_replaces_xs() {
        let template = "/path/to/.file.txt.XXXXXX";
        let result = fill_random_suffix(template);
        assert!(!result.ends_with("XXXXXX"), "Xs not replaced: {result}");
        assert_eq!(result.len(), template.len());
        assert!(result.starts_with("/path/to/.file.txt."));
    }

    #[test]
    fn fill_random_suffix_produces_unique_names() {
        let template = "/tmp/.test.XXXXXX";
        let names: Vec<String> = (0..10).map(|_| fill_random_suffix(template)).collect();
        // With 62^6 = ~56 billion possibilities, duplicates are near-impossible.
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
        }

        assert!(!temp_path.exists());
    }

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
        // é is 2 bytes (0xC3 0xA9); 130 of them is 260 bytes, exceeding NAME_MAX.
        let chars = "é".repeat(130);
        let name = format!(".{chars}.XXXXXX");
        let result = truncate_utf8_safe(&name, 255);
        assert!(result.len() <= 255);
        assert!(result.ends_with(".XXXXXX"));
        assert!(!result.contains('\u{FFFD}'));
    }

    #[cfg(unix)]
    #[test]
    fn open_tmpfile_sandboxed_creates_and_cleans_up_via_carrier() {
        let dir = tempdir().expect("create temp dir");
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let sandbox = Arc::new(fast_io::DirSandbox::open_root(&root).expect("open sandbox"));
        let dest = root.join("payload.bin");

        let temp_path;
        {
            let (_file, guard) = open_tmpfile_sandboxed(&dest, None, Some(&sandbox), Some(&root))
                .expect("open sandboxed");
            temp_path = guard.path().to_path_buf();
            assert!(temp_path.exists());
            assert_eq!(temp_path.parent().unwrap(), root);
        }

        assert!(
            !temp_path.exists(),
            "sandbox-anchored guard must unlink the temp file on drop"
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_tmpfile_sandboxed_falls_back_when_temp_dir_differs() {
        let dest_root = tempdir().expect("dest dir");
        let dest_canon = std::fs::canonicalize(dest_root.path()).expect("canon dest");
        let temp_root = tempdir().expect("temp dir");
        let temp_canon = std::fs::canonicalize(temp_root.path()).expect("canon temp");
        let sandbox = Arc::new(fast_io::DirSandbox::open_root(&dest_canon).expect("sandbox"));
        let dest = dest_canon.join("file.txt");

        let (_file, mut guard) =
            open_tmpfile_sandboxed(&dest, Some(&temp_canon), Some(&sandbox), Some(&dest_canon))
                .expect("open sandboxed with temp_dir");

        let temp_path = guard.path().to_path_buf();
        assert!(temp_path.starts_with(&temp_canon));
        // Anchor must not engage when the temp parent differs from dest_dir.
        assert!(guard.anchor.is_none());
        guard.keep();
        let _ = std::fs::remove_file(&temp_path);
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_anchored_guard_resists_symlink_swap_on_parent() {
        // Regression: a symlink swap on the temp file's parent between
        // create and unlink would, via path-based remove_file, redirect
        // the unlink to an attacker-chosen inode. The sandbox-anchored
        // Drop unlinks via `unlinkat(dirfd, leaf, 0)`, which is pinned to
        // the original dirfd opened at receiver setup so the swap cannot
        // redirect the cleanup.
        let staging = tempdir().expect("staging");
        let staging_canon = std::fs::canonicalize(staging.path()).expect("canon staging");
        let real_parent = staging_canon.join("real");
        let real_aside = staging_canon.join("real.aside");
        let attacker_parent = staging_canon.join("attacker");
        std::fs::create_dir(&real_parent).expect("real");
        std::fs::create_dir(&attacker_parent).expect("attacker");
        let victim_in_attacker = attacker_parent.join("victim.bin");
        std::fs::write(&victim_in_attacker, b"do not delete me").expect("victim");

        // Open the sandbox before any swap so the dirfd points at `real`.
        let sandbox = Arc::new(fast_io::DirSandbox::open_root(&real_parent).expect("sandbox"));
        let dest = real_parent.join("payload.bin");

        let (_file, guard) =
            open_tmpfile_sandboxed(&dest, None, Some(&sandbox), Some(&real_parent))
                .expect("open sandboxed");
        let temp_path = guard.path().to_path_buf();
        let temp_leaf = temp_path.file_name().unwrap().to_owned();
        assert!(temp_path.exists());

        // Plant a same-leaf decoy inside `attacker` so a confused
        // path-based unlink would target the wrong inode.
        let attacker_decoy = attacker_parent.join(&temp_leaf);
        std::fs::write(&attacker_decoy, b"do not delete me either").expect("decoy");

        // Rename the real directory aside and plant a symlink at the
        // original location pointing at the attacker tree. The sandbox
        // dirfd still references the (now-renamed) real directory; the
        // attacker has overwritten the path-based `real_parent` route.
        std::fs::rename(&real_parent, &real_aside).expect("rename real aside");
        std::os::unix::fs::symlink(&attacker_parent, &real_parent).expect("plant symlink");

        // Drop the guard - the unlinkat must run against the original
        // dirfd, which still resolves to the moved-aside real directory.
        drop(guard);

        // The decoy under the attacker tree must survive: a path-based
        // remove_file(real_parent/<leaf>) after the swap would have
        // unlinked it.
        assert!(
            attacker_decoy.exists(),
            "sandbox-anchored unlink must not delete the attacker-controlled decoy"
        );
        assert!(
            victim_in_attacker.exists(),
            "unrelated attacker-owned file must remain untouched"
        );
        // And the real temp file must be gone from its (renamed) home.
        let renamed_temp = real_aside.join(&temp_leaf);
        assert!(
            !renamed_temp.exists(),
            "sandbox-anchored unlink must remove the original temp file"
        );
    }

    #[test]
    fn rename_to_partial_dir_absolute() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("temp_file.tmp");
        let partial_dir = dir.path().join("partial");
        let dest_path = dir.path().join("final_dest.txt");

        fs::write(&temp_path, b"partial content").unwrap();

        let mut guard = TempFileGuard::new(temp_path.clone());
        let result = guard.rename_to_partial_dir(&dest_path, &partial_dir);

        assert!(result.is_ok());
        let partial_path = result.unwrap();
        assert_eq!(partial_path, partial_dir.join("final_dest.txt"));
        assert!(partial_path.exists());
        assert_eq!(fs::read(&partial_path).unwrap(), b"partial content");
        assert!(!temp_path.exists());
        // Guard should be marked kept.
        assert!(guard.keep_on_drop);
    }

    #[test]
    fn rename_to_partial_dir_relative() {
        let dir = tempdir().expect("create temp dir");
        let dest_dir = dir.path().join("dest");
        fs::create_dir(&dest_dir).unwrap();
        let temp_path = dir.path().join("temp_file.tmp");
        let dest_path = dest_dir.join("file.dat");

        fs::write(&temp_path, b"relative partial").unwrap();

        let mut guard = TempFileGuard::new(temp_path.clone());
        let relative_dir = Path::new(".rsync-partial");
        let result = guard.rename_to_partial_dir(&dest_path, relative_dir);

        assert!(result.is_ok());
        let partial_path = result.unwrap();
        assert_eq!(
            partial_path,
            dest_dir.join(".rsync-partial").join("file.dat")
        );
        assert!(partial_path.exists());
        assert_eq!(fs::read(&partial_path).unwrap(), b"relative partial");
    }

    #[test]
    fn rename_to_partial_dir_creates_missing_directory() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("temp.tmp");
        let partial_dir = dir.path().join("deep").join("nested").join("partial");
        let dest_path = dir.path().join("file.txt");

        fs::write(&temp_path, b"nested partial").unwrap();

        let mut guard = TempFileGuard::new(temp_path.clone());
        let result = guard.rename_to_partial_dir(&dest_path, &partial_dir);

        assert!(result.is_ok());
        let partial_path = result.unwrap();
        assert!(partial_path.exists());
        assert_eq!(fs::read(&partial_path).unwrap(), b"nested partial");
    }

    #[test]
    fn rename_to_partial_dir_no_filename_returns_error() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("temp.tmp");
        let partial_dir = dir.path().join("partial");

        fs::write(&temp_path, b"data").unwrap();

        let mut guard = TempFileGuard::new(temp_path);
        let result = guard.rename_to_partial_dir(Path::new("/"), &partial_dir);

        assert!(result.is_err());
    }

    /// Regression (#515): an errored transfer's temp path must be removed from
    /// the global `CleanupManager` when the guard drops, otherwise a
    /// long-running daemon leaks a `PathBuf` per errored file forever. The
    /// guard is registered, then dropped WITHOUT `keep()` (the error path);
    /// after Drop the registry must no longer contain the path.
    #[test]
    fn errored_guard_unregisters_from_cleanup_manager_on_drop() {
        let _lock = test_support::cleanup_registry_test_guard();
        let manager = engine::CleanupManager::global();
        manager.reset_for_testing();

        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join(".errored.AbCdEf");
        fs::write(&temp_path, b"partial data").unwrap();

        {
            let mut guard = TempFileGuard::new(temp_path.clone());
            manager.register_temp_file(temp_path.clone());
            guard.mark_registered();
            assert_eq!(manager.temp_file_count(), 1);
            // Guard drops here on the error path (no keep()).
        }

        assert_eq!(
            manager.temp_file_count(),
            0,
            "errored guard must unregister its temp path from CleanupManager on drop"
        );
        assert!(
            !temp_path.exists(),
            "errored guard must still delete the temp file on drop"
        );
    }

    /// The kept (success) path must also leave the registry clean after Drop.
    /// The guard is registered and marked kept; on Drop it does not delete the
    /// (committed) file but must remove the stale registry entry.
    #[test]
    fn kept_guard_unregisters_from_cleanup_manager_on_drop() {
        let _lock = test_support::cleanup_registry_test_guard();
        let manager = engine::CleanupManager::global();
        manager.reset_for_testing();

        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join(".kept.XyZ123");
        fs::write(&temp_path, b"committed data").unwrap();

        {
            let mut guard = TempFileGuard::new(temp_path.clone());
            manager.register_temp_file(temp_path.clone());
            guard.mark_registered();
            guard.keep();
        }

        assert_eq!(
            manager.temp_file_count(),
            0,
            "kept guard must unregister its temp path from CleanupManager on drop"
        );
        assert!(
            temp_path.exists(),
            "kept guard must not delete the committed file on drop"
        );
        fs::remove_file(&temp_path).ok();
    }

    /// An unregistered guard (e.g. an in-place / device target that was never
    /// registered) must not touch the registry on Drop.
    #[test]
    fn unregistered_guard_leaves_cleanup_manager_untouched() {
        let _lock = test_support::cleanup_registry_test_guard();
        let manager = engine::CleanupManager::global();
        manager.reset_for_testing();

        let other = PathBuf::from("/tmp/.unrelated.Aa0000");
        manager.register_temp_file(other.clone());

        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join(".never_registered.Zz9999");
        fs::write(&temp_path, b"data").unwrap();

        {
            // No mark_registered(): Drop must not remove any registry entry.
            let _guard = TempFileGuard::new(temp_path);
        }

        assert_eq!(
            manager.temp_file_count(),
            1,
            "an unregistered guard must not disturb other registry entries on drop"
        );
        manager.unregister_temp_file(&other);
    }
}
