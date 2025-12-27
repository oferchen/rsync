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
    #[must_use]
    #[doc(alias = "--suffix")]
    pub fn with_backup_suffix<S: Into<OsString>>(mut self, suffix: Option<S>) -> Self {
        match suffix {
            Some(value) => {
                self.backup_suffix = value.into();
                self.backup = true;
            }
            None => {
                self.backup_suffix = OsString::from("~");
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
    #[must_use]
    pub fn backup_directory(&self) -> Option<&Path> {
        self.backup_dir.as_deref()
    }

    /// Returns the suffix appended to backup file names.
    #[must_use]
    pub fn backup_suffix(&self) -> &OsStr {
        &self.backup_suffix
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
}
