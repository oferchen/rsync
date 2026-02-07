use std::path::{Path, PathBuf};

use super::types::LocalCopyOptions;

/// Default log-file-format matching upstream rsync.
const DEFAULT_LOG_FILE_FORMAT: &str = "%i %n%L";

impl LocalCopyOptions {
    /// Configures the file path used for logging transfer activity.
    #[must_use]
    #[doc(alias = "--log-file")]
    pub fn with_log_file<P: Into<PathBuf>>(mut self, path: Option<P>) -> Self {
        self.log_file = path.map(Into::into);
        self
    }

    /// Configures the per-item log format string.
    #[must_use]
    #[doc(alias = "--log-file-format")]
    pub fn with_log_file_format<S: Into<String>>(mut self, format: Option<S>) -> Self {
        self.log_file_format = format.map(Into::into);
        self
    }

    /// Returns the configured log file path, if any.
    #[must_use]
    pub fn log_file_path(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    /// Returns the configured log file format string, if any.
    #[must_use]
    pub fn log_file_format(&self) -> Option<&str> {
        self.log_file_format.as_deref()
    }

    /// Returns the effective log file format, falling back to the upstream
    /// default `"%i %n%L"` when no custom format has been configured.
    #[must_use]
    pub fn effective_log_file_format(&self) -> &str {
        self.log_file_format
            .as_deref()
            .unwrap_or(DEFAULT_LOG_FILE_FORMAT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_default_is_none() {
        let opts = LocalCopyOptions::new();
        assert!(opts.log_file_path().is_none());
    }

    #[test]
    fn log_file_format_default_is_none() {
        let opts = LocalCopyOptions::new();
        assert!(opts.log_file_format().is_none());
    }

    #[test]
    fn with_log_file_sets_path() {
        let opts = LocalCopyOptions::new().with_log_file(Some("/var/log/rsync.log"));
        assert_eq!(opts.log_file_path(), Some(Path::new("/var/log/rsync.log")));
    }

    #[test]
    fn with_log_file_none_clears_path() {
        let opts = LocalCopyOptions::new()
            .with_log_file(Some("/var/log/rsync.log"))
            .with_log_file::<PathBuf>(None);
        assert!(opts.log_file_path().is_none());
    }

    #[test]
    fn with_log_file_accepts_pathbuf() {
        let path = PathBuf::from("/tmp/transfer.log");
        let opts = LocalCopyOptions::new().with_log_file(Some(path));
        assert_eq!(opts.log_file_path(), Some(Path::new("/tmp/transfer.log")));
    }

    #[test]
    fn with_log_file_format_sets_format() {
        let opts = LocalCopyOptions::new().with_log_file_format(Some("%t %f %b"));
        assert_eq!(opts.log_file_format(), Some("%t %f %b"));
    }

    #[test]
    fn with_log_file_format_none_clears_format() {
        let opts = LocalCopyOptions::new()
            .with_log_file_format(Some("%t %f %b"))
            .with_log_file_format::<String>(None);
        assert!(opts.log_file_format().is_none());
    }

    #[test]
    fn with_log_file_format_accepts_string() {
        let fmt = String::from("%o %n");
        let opts = LocalCopyOptions::new().with_log_file_format(Some(fmt));
        assert_eq!(opts.log_file_format(), Some("%o %n"));
    }

    #[test]
    fn effective_log_file_format_returns_default_when_not_set() {
        let opts = LocalCopyOptions::new();
        assert_eq!(opts.effective_log_file_format(), "%i %n%L");
    }

    #[test]
    fn effective_log_file_format_returns_custom_when_set() {
        let opts = LocalCopyOptions::new().with_log_file_format(Some("%t %f %b"));
        assert_eq!(opts.effective_log_file_format(), "%t %f %b");
    }

    #[test]
    fn log_file_and_format_can_be_set_together() {
        let opts = LocalCopyOptions::new()
            .with_log_file(Some("/var/log/rsync.log"))
            .with_log_file_format(Some("%o %n %b"));
        assert_eq!(opts.log_file_path(), Some(Path::new("/var/log/rsync.log")));
        assert_eq!(opts.log_file_format(), Some("%o %n %b"));
        assert_eq!(opts.effective_log_file_format(), "%o %n %b");
    }
}
