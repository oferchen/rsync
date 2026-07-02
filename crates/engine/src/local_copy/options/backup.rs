//! Setters and accessors for `--backup`, `--backup-dir`, and `--suffix` on
//! [`LocalCopyOptions`].

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use super::types::LocalCopyOptions;

impl LocalCopyOptions {
    /// Enables or disables creation of backups prior to overwriting or deleting entries.
    #[must_use]
    #[doc(alias = "--backup")]
    #[doc(alias = "-b")]
    pub const fn backup(mut self, enabled: bool) -> Self {
        self.backup = enabled;
        self
    }

    /// Configures the optional directory that should receive backup entries.
    #[must_use]
    #[doc(alias = "--backup-dir")]
    pub fn with_backup_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.backup_dir = directory.map(Into::into);
        if self.backup_dir.is_some() {
            self.backup = true;
        }
        self
    }

    /// Overrides the suffix appended to backup file names.
    ///
    /// When `None`, the default is `""` if a backup directory is set, or `"~"`
    /// otherwise - matching upstream `options.c:2278-2279`.
    #[must_use]
    #[doc(alias = "--suffix")]
    pub fn with_backup_suffix<S: Into<OsString>>(mut self, suffix: Option<S>) -> Self {
        match suffix {
            Some(value) => {
                self.backup_suffix = value.into();
                self.backup = true;
            }
            None => {
                // upstream: options.c:2278-2279
                // backup_suffix = backup_dir ? "" : BACKUP_SUFFIX
                if self.backup_dir.is_some() {
                    self.backup_suffix = OsString::new();
                } else {
                    self.backup_suffix = OsString::from("~");
                }
            }
        }
        self
    }

    /// Reports whether backups should be created before overwriting or deleting entries.
    #[must_use]
    pub const fn backup_enabled(&self) -> bool {
        self.backup
    }

    /// Returns the configured backup directory, if any.
    pub fn backup_directory(&self) -> Option<&Path> {
        self.backup_dir.as_deref()
    }

    /// Returns the suffix appended to backup file names.
    #[must_use]
    pub fn backup_suffix(&self) -> &OsStr {
        &self.backup_suffix
    }

    /// Reports whether `name` already ends with the backup suffix.
    ///
    /// Mirrors upstream `delete.c:37-41 is_backup_file()`: a non-empty suffix
    /// that matches the trailing bytes of the name (with at least one byte
    /// preceding it) marks the file as an existing backup.
    #[must_use]
    pub fn is_backup_file(&self, name: &OsStr) -> bool {
        let suffix = self.backup_suffix.as_encoded_bytes();
        if suffix.is_empty() {
            return false;
        }
        let name = name.as_encoded_bytes();
        // upstream: `k = strlen(fn) - backup_suffix_len; k > 0 && ...`
        name.len() > suffix.len() && name.ends_with(suffix)
    }

    /// Reports whether an extraneous entry should be backed up before it is
    /// deleted, rather than unlinked directly.
    ///
    /// Mirrors the guard on upstream `delete.c:165`:
    /// `make_backups > 0 && (backup_dir || !is_backup_file(fbuf))`. A file
    /// whose name already ends in the backup suffix is unlinked directly (no
    /// re-backup to `<name><suffix><suffix>`) unless a `--backup-dir` is set,
    /// in which case the backup always lands in the separate directory.
    #[must_use]
    pub fn should_backup_before_delete(&self, name: &OsStr) -> bool {
        self.backup && (self.backup_dir.is_some() || !self.is_backup_file(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_enables_backup_mode() {
        let opts = LocalCopyOptions::new().backup(true);
        assert!(opts.backup_enabled());
    }

    #[test]
    fn backup_false_disables_backup_mode() {
        let opts = LocalCopyOptions::new().backup(true).backup(false);
        assert!(!opts.backup_enabled());
    }

    #[test]
    fn with_backup_directory_sets_path_and_enables_backup() {
        let opts = LocalCopyOptions::new().with_backup_directory(Some("/tmp/backup"));
        assert!(opts.backup_enabled());
        assert_eq!(opts.backup_directory(), Some(Path::new("/tmp/backup")));
    }

    #[test]
    fn with_backup_directory_none_clears_path() {
        let opts = LocalCopyOptions::new()
            .with_backup_directory(Some("/tmp/backup"))
            .with_backup_directory::<PathBuf>(None);
        assert!(opts.backup_directory().is_none());
    }

    #[test]
    fn with_backup_suffix_sets_suffix_and_enables_backup() {
        let opts = LocalCopyOptions::new().with_backup_suffix(Some(".bak"));
        assert!(opts.backup_enabled());
        assert_eq!(opts.backup_suffix(), OsStr::new(".bak"));
    }

    #[test]
    fn with_backup_suffix_none_resets_to_default() {
        let opts = LocalCopyOptions::new()
            .with_backup_suffix(Some(".bak"))
            .with_backup_suffix::<OsString>(None);
        assert_eq!(opts.backup_suffix(), OsStr::new("~"));
    }

    #[test]
    fn with_backup_suffix_none_empty_when_backup_dir_set() {
        // upstream: options.c:2278-2279 - backup_suffix = backup_dir ? "" : BACKUP_SUFFIX
        let opts = LocalCopyOptions::new()
            .with_backup_directory(Some("/backups"))
            .with_backup_suffix::<OsString>(None);
        assert_eq!(opts.backup_suffix(), OsStr::new(""));
    }

    #[test]
    fn backup_suffix_default_is_tilde() {
        let opts = LocalCopyOptions::new();
        assert_eq!(opts.backup_suffix(), OsStr::new("~"));
    }

    #[test]
    fn backup_directory_default_is_none() {
        let opts = LocalCopyOptions::new();
        assert!(opts.backup_directory().is_none());
    }

    #[test]
    fn backup_default_is_disabled() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.backup_enabled());
    }

    #[test]
    fn with_backup_directory_accepts_pathbuf() {
        let path = PathBuf::from("/var/backups");
        let opts = LocalCopyOptions::new().with_backup_directory(Some(path));
        assert_eq!(opts.backup_directory(), Some(Path::new("/var/backups")));
    }

    #[test]
    fn with_backup_suffix_accepts_osstring() {
        let suffix = OsString::from(".backup");
        let opts = LocalCopyOptions::new().with_backup_suffix(Some(suffix));
        assert_eq!(opts.backup_suffix(), OsStr::new(".backup"));
    }

    #[test]
    fn is_backup_file_matches_trailing_suffix() {
        // upstream: delete.c:37-41 - k > 0 && strcmp(fn+k, backup_suffix) == 0.
        let opts = LocalCopyOptions::new().backup(true); // default suffix "~"
        assert!(opts.is_backup_file(OsStr::new("stale~")));
        assert!(!opts.is_backup_file(OsStr::new("plain")));
        // k > 0 requires at least one byte before the suffix: bare "~" is not a
        // backup file (strlen("~") - 1 == 0).
        assert!(!opts.is_backup_file(OsStr::new("~")));
    }

    #[test]
    fn is_backup_file_false_when_suffix_empty() {
        // A --backup-dir gives an empty suffix; nothing is ever "already a backup".
        let opts = LocalCopyOptions::new()
            .with_backup_directory(Some("/backups"))
            .with_backup_suffix::<OsString>(None);
        assert!(!opts.is_backup_file(OsStr::new("anything~")));
    }

    #[test]
    fn should_backup_before_delete_skips_already_suffixed() {
        // upstream: delete.c:165 - backup_dir || !is_backup_file(fbuf).
        // With -b and no --backup-dir, a "~"-suffixed extraneous file is
        // unlinked directly, not re-backed-up to "<name>~~".
        let opts = LocalCopyOptions::new().backup(true);
        assert!(opts.should_backup_before_delete(OsStr::new("plain")));
        assert!(!opts.should_backup_before_delete(OsStr::new("stale~")));
    }

    #[test]
    fn should_backup_before_delete_backup_dir_always_backs_up() {
        // With --backup-dir the file lands in the separate directory, so even a
        // suffixed name is backed up (backup_dir short-circuits the guard).
        let opts = LocalCopyOptions::new().with_backup_directory(Some("/backups"));
        assert!(opts.should_backup_before_delete(OsStr::new("stale~")));
    }

    #[test]
    fn should_backup_before_delete_false_without_backup() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.should_backup_before_delete(OsStr::new("plain")));
    }
}
