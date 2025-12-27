use std::path::{Path, PathBuf};

use super::types::LocalCopyOptions;

impl LocalCopyOptions {
    /// Enables sparse file handling during copies.
    #[must_use]
    #[doc(alias = "--sparse")]
    pub const fn sparse(mut self, sparse: bool) -> Self {
        self.sparse = sparse;
        self
    }

    /// Requests that partial transfers leave temporary files.
    #[must_use]
    #[doc(alias = "--partial")]
    pub const fn partial(mut self, partial: bool) -> Self {
        self.partial = partial;
        self
    }

    /// Selects the directory used for temporary files when staging updates.
    #[must_use]
    #[doc(alias = "--temp-dir")]
    #[doc(alias = "--tmp-dir")]
    pub fn with_temp_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.temp_dir = directory.map(Into::into);
        self
    }

    /// Requests that updated files be renamed into place after the transfer completes.
    #[must_use]
    #[doc(alias = "--delay-updates")]
    pub const fn delay_updates(mut self, delay: bool) -> Self {
        self.delay_updates = delay;
        if delay {
            self.partial = true;
        }
        self
    }

    /// Requests that updated files be flushed to stable storage once writing completes.
    #[must_use]
    #[doc(alias = "--fsync")]
    pub const fn fsync(mut self, fsync: bool) -> Self {
        self.fsync = fsync;
        self
    }

    /// Selects the directory used to retain partial files when transfers fail.
    #[must_use]
    #[doc(alias = "--partial-dir")]
    pub fn with_partial_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.partial_dir = directory.map(Into::into);
        if self.partial_dir.is_some() {
            self.partial = true;
        }
        self
    }

    /// Requests in-place destination updates.
    #[must_use]
    #[doc(alias = "--inplace")]
    pub const fn inplace(mut self, inplace: bool) -> Self {
        self.inplace = inplace;
        self
    }

    /// Enables appending to existing destination files when they are shorter than the source.
    #[must_use]
    #[doc(alias = "--append")]
    pub const fn append(mut self, append: bool) -> Self {
        self.append = append;
        if !append {
            self.append_verify = false;
        }
        self
    }

    /// Enables append-with-verification semantics.
    #[must_use]
    #[doc(alias = "--append-verify")]
    pub const fn append_verify(mut self, verify: bool) -> Self {
        if verify {
            self.append = true;
            self.append_verify = true;
        } else {
            self.append_verify = false;
        }
        self
    }

    /// Enables collection of transfer events that describe work performed by the engine.
    #[must_use]
    pub const fn collect_events(mut self, collect: bool) -> Self {
        self.collect_events = collect;
        self
    }

    /// Reports whether sparse handling has been requested.
    #[must_use]
    pub const fn sparse_enabled(&self) -> bool {
        self.sparse
    }

    /// Reports whether partial transfer handling has been requested.
    #[must_use]
    pub const fn partial_enabled(&self) -> bool {
        self.partial || self.partial_dir.is_some()
    }

    /// Returns the configured partial directory when present.
    #[must_use]
    pub fn partial_directory_path(&self) -> Option<&Path> {
        self.partial_dir.as_deref()
    }

    /// Returns the configured temporary directory for staged updates when present.
    #[must_use]
    pub fn temp_directory_path(&self) -> Option<&Path> {
        self.temp_dir.as_deref()
    }

    /// Reports whether destination updates should be delayed until the end of the transfer.
    #[must_use]
    pub const fn delay_updates_enabled(&self) -> bool {
        self.delay_updates
    }

    /// Reports whether destination files should be fsynced after updates.
    #[must_use]
    pub const fn fsync_enabled(&self) -> bool {
        self.fsync
    }

    /// Reports whether in-place destination updates have been requested.
    #[must_use]
    pub const fn inplace_enabled(&self) -> bool {
        self.inplace
    }

    /// Returns `true` when appending to existing destinations is enabled.
    #[must_use]
    pub const fn append_enabled(&self) -> bool {
        self.append
    }

    /// Returns `true` when append verification is requested.
    #[must_use]
    pub const fn append_verify_enabled(&self) -> bool {
        self.append_verify
    }

    /// Reports whether the execution should record transfer events.
    #[must_use]
    pub const fn events_enabled(&self) -> bool {
        self.collect_events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_enables() {
        let opts = LocalCopyOptions::new().sparse(true);
        assert!(opts.sparse_enabled());
    }

    #[test]
    fn sparse_disables() {
        let opts = LocalCopyOptions::new().sparse(true).sparse(false);
        assert!(!opts.sparse_enabled());
    }

    #[test]
    fn partial_enables() {
        let opts = LocalCopyOptions::new().partial(true);
        assert!(opts.partial_enabled());
    }

    #[test]
    fn partial_disables() {
        let opts = LocalCopyOptions::new().partial(true).partial(false);
        assert!(!opts.partial_enabled());
    }

    #[test]
    fn with_temp_directory_sets_path() {
        let opts = LocalCopyOptions::new().with_temp_directory(Some("/tmp/staging"));
        assert_eq!(opts.temp_directory_path(), Some(Path::new("/tmp/staging")));
    }

    #[test]
    fn with_temp_directory_none_clears() {
        let opts = LocalCopyOptions::new()
            .with_temp_directory(Some("/tmp/staging"))
            .with_temp_directory::<PathBuf>(None);
        assert!(opts.temp_directory_path().is_none());
    }

    #[test]
    fn delay_updates_enables_and_sets_partial() {
        let opts = LocalCopyOptions::new().delay_updates(true);
        assert!(opts.delay_updates_enabled());
        assert!(opts.partial_enabled());
    }

    #[test]
    fn fsync_enables() {
        let opts = LocalCopyOptions::new().fsync(true);
        assert!(opts.fsync_enabled());
    }

    #[test]
    fn fsync_disables() {
        let opts = LocalCopyOptions::new().fsync(true).fsync(false);
        assert!(!opts.fsync_enabled());
    }

    #[test]
    fn with_partial_directory_sets_path_and_enables_partial() {
        let opts = LocalCopyOptions::new().with_partial_directory(Some("/tmp/partial"));
        assert_eq!(opts.partial_directory_path(), Some(Path::new("/tmp/partial")));
        assert!(opts.partial_enabled());
    }

    #[test]
    fn with_partial_directory_none_clears() {
        let opts = LocalCopyOptions::new()
            .with_partial_directory(Some("/tmp/partial"))
            .with_partial_directory::<PathBuf>(None);
        assert!(opts.partial_directory_path().is_none());
    }

    #[test]
    fn inplace_enables() {
        let opts = LocalCopyOptions::new().inplace(true);
        assert!(opts.inplace_enabled());
    }

    #[test]
    fn inplace_disables() {
        let opts = LocalCopyOptions::new().inplace(true).inplace(false);
        assert!(!opts.inplace_enabled());
    }

    #[test]
    fn append_enables() {
        let opts = LocalCopyOptions::new().append(true);
        assert!(opts.append_enabled());
    }

    #[test]
    fn append_false_clears_verify() {
        let opts = LocalCopyOptions::new()
            .append_verify(true)
            .append(false);
        assert!(!opts.append_enabled());
        assert!(!opts.append_verify_enabled());
    }

    #[test]
    fn append_verify_enables_both() {
        let opts = LocalCopyOptions::new().append_verify(true);
        assert!(opts.append_enabled());
        assert!(opts.append_verify_enabled());
    }

    #[test]
    fn append_verify_false_clears_verify_only() {
        let opts = LocalCopyOptions::new()
            .append_verify(true)
            .append_verify(false);
        assert!(opts.append_enabled()); // append remains on
        assert!(!opts.append_verify_enabled());
    }

    #[test]
    fn collect_events_enables() {
        let opts = LocalCopyOptions::new().collect_events(true);
        assert!(opts.events_enabled());
    }

    #[test]
    fn collect_events_disables() {
        let opts = LocalCopyOptions::new()
            .collect_events(true)
            .collect_events(false);
        assert!(!opts.events_enabled());
    }

    #[test]
    fn defaults_have_no_staging_options() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.sparse_enabled());
        assert!(!opts.partial_enabled());
        assert!(opts.partial_directory_path().is_none());
        assert!(opts.temp_directory_path().is_none());
        assert!(!opts.delay_updates_enabled());
        assert!(!opts.fsync_enabled());
        assert!(!opts.inplace_enabled());
        assert!(!opts.append_enabled());
        assert!(!opts.append_verify_enabled());
        assert!(!opts.events_enabled());
    }
}
