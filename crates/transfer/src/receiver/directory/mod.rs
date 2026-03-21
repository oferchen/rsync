//! Directory, symlink, and hardlink creation; extraneous file deletion.
//!
//! Handles filesystem mutations driven by the received file list, including
//! directory creation (batch and incremental), symlink creation, hardlink
//! creation for both protocol 30+ and pre-30 modes, and `--delete` scanning.

mod creation;
mod deletion;
mod links;

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

/// Tracks directories that failed to create.
///
/// Children of failed directories are skipped during incremental processing.
#[derive(Debug, Default)]
pub(in crate::receiver) struct FailedDirectories {
    /// Failed directory paths (normalized, no trailing slash).
    paths: std::collections::HashSet<String>,
}

impl FailedDirectories {
    /// Creates a new empty tracker.
    pub(in crate::receiver) fn new() -> Self {
        Self::default()
    }

    /// Marks a directory as failed.
    pub(in crate::receiver) fn mark_failed(&mut self, path: &str) {
        self.paths.insert(path.to_string());
    }

    /// Checks if an entry path has a failed ancestor directory.
    ///
    /// Returns the failed ancestor path if found, `None` otherwise.
    pub(in crate::receiver) fn failed_ancestor(&self, entry_path: &str) -> Option<&str> {
        // Check if exact path is failed
        if self.paths.contains(entry_path) {
            return self.paths.get(entry_path).map(|s| s.as_str());
        }

        // Check each parent path component
        let mut check_path = entry_path;
        while let Some(pos) = check_path.rfind('/') {
            check_path = &check_path[..pos];
            if let Some(failed) = self.paths.get(check_path) {
                return Some(failed.as_str());
            }
        }
        None
    }

    /// Returns the number of failed directories.
    #[cfg(test)]
    pub(in crate::receiver) fn count(&self) -> usize {
        self.paths.len()
    }
}
