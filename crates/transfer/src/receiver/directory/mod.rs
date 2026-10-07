//! Directory, symlink, special-file, and hardlink creation; extraneous file
//! deletion.
//!
//! Handles filesystem mutations driven by the received file list, including
//! directory creation (batch and incremental), symlink creation, special-file
//! creation (FIFOs, sockets, and device nodes), hardlink creation for both
//! protocol 30+ and pre-30 modes, and `--delete` scanning.

mod backup;
mod creation;
/// Directory deletion, including the deferred `--delete-delay` queue.
///
/// `pub(in crate::receiver)` so the receiver context can name `DeletedEntry`
/// for its deferred `--delete-delay` victim queue.
pub(in crate::receiver) mod deletion;
mod links;
mod missing_args;
/// Removal of a destination entry obstructing the incoming one.
///
/// `pub(in crate::receiver)` so the regular-file candidate pass can name
/// `MakeWayFor` for its `DEL_FOR_FILE` removal (generator.c:2149).
pub(in crate::receiver) mod obstacle;
mod special;

/// Normalizes a filename for cross-platform comparison.
///
/// On macOS, converts NFD (decomposed) filenames to NFC (composed) so that
/// names from `read_dir` (which returns NFD on HFS+/APFS) match names from
/// the sender's file list (typically NFC from Linux). On all other platforms
/// this returns the input as-is with no allocation overhead.
#[cfg(target_os = "macos")]
fn normalize_filename_for_compare(name: &std::ffi::OsStr) -> std::ffi::OsString {
    apple_fs::normalize_filename(name)
}

/// No-op on non-macOS platforms - direct byte comparison is correct.
#[cfg(not(target_os = "macos"))]
fn normalize_filename_for_compare(name: &std::ffi::OsStr) -> std::ffi::OsString {
    name.to_os_string()
}

/// Tracks directories that failed to create, or that `--existing` skipped.
///
/// Children of both kinds are skipped during incremental processing; only a
/// failure is an error.
/// Mirrors upstream rsync's behavior where `mkdir` failures cause the entire
/// subtree to be skipped rather than producing cascading permission errors.
#[derive(Debug, Default)]
pub(in crate::receiver) struct FailedDirectories {
    /// Failed directory paths, keyed by their exact bytes so distinct
    /// non-UTF-8 names never alias each other.
    paths: std::collections::HashSet<std::path::PathBuf>,
    /// Directories `--existing` skipped because the destination lacks them.
    /// Their subtrees are skipped silently and are not failures.
    missing: std::collections::HashSet<std::path::PathBuf>,
}

impl FailedDirectories {
    /// Creates a new empty tracker.
    pub(in crate::receiver) fn new() -> Self {
        Self::default()
    }

    /// Marks a directory as failed.
    pub(in crate::receiver) fn mark_failed(&mut self, path: impl AsRef<std::path::Path>) {
        self.paths.insert(path.as_ref().to_path_buf());
    }

    /// Marks a directory as skipped because `--existing` found it missing.
    ///
    /// upstream: generator.c:1755-1761 - sets `skip_dir` and
    /// `FLAG_MISSING_DIR` without touching `io_error`.
    pub(in crate::receiver) fn mark_missing(&mut self, path: impl AsRef<std::path::Path>) {
        self.missing.insert(path.as_ref().to_path_buf());
    }

    /// Checks if an entry path, or any of its ancestors, is a directory
    /// skipped as missing under `--existing`.
    ///
    /// upstream: generator.c:1646-1656 - `is_below(file, skip_dir)` returns
    /// early, silently, for every entry below the skipped directory.
    pub(in crate::receiver) fn is_missing_or_below(
        &self,
        entry_path: impl AsRef<std::path::Path>,
    ) -> bool {
        entry_path
            .as_ref()
            .ancestors()
            .any(|p| self.missing.contains(p))
    }

    /// Checks if an entry path, or any of its ancestors, is a failed directory.
    ///
    /// Returns the closest failed path if found, `None` otherwise.
    pub(in crate::receiver) fn failed_ancestor(
        &self,
        entry_path: impl AsRef<std::path::Path>,
    ) -> Option<&std::path::Path> {
        entry_path
            .as_ref()
            .ancestors()
            .find_map(|p| self.paths.get(p).map(std::path::PathBuf::as_path))
    }

    /// Returns the number of failed directories.
    #[cfg(test)]
    pub(in crate::receiver) fn count(&self) -> usize {
        self.paths.len()
    }
}
