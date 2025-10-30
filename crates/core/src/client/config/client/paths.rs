use super::*;

impl ClientConfig {
    /// Returns the ordered list of link-destination directories supplied by the caller.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn link_dest_paths(&self) -> &[PathBuf] {
        &self.link_dest_paths
    }

    /// Reports whether backups should be created before overwriting or deleting entries.
    #[must_use]
    #[doc(alias = "--backup")]
    pub const fn backup(&self) -> bool {
        self.backup
    }

    /// Returns the configured backup directory when `--backup-dir` is supplied.
    #[must_use]
    #[doc(alias = "--backup-dir")]
    pub fn backup_directory(&self) -> Option<&Path> {
        self.backup_dir.as_deref()
    }

    /// Returns the suffix appended to backup entries when specified.
    #[must_use]
    #[doc(alias = "--suffix")]
    pub fn backup_suffix(&self) -> Option<&OsStr> {
        self.backup_suffix.as_deref()
    }
}
