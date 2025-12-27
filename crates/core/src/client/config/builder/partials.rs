use super::*;

impl ClientConfigBuilder {
    /// Enables or disables retention of partial files on failure.
    #[must_use]
    #[doc(alias = "--partial")]
    #[doc(alias = "--no-partial")]
    #[doc(alias = "-P")]
    pub const fn partial(mut self, partial: bool) -> Self {
        self.partial = partial;
        self
    }

    /// Enables or disables delayed update commits, mirroring `--delay-updates`.
    #[must_use]
    #[doc(alias = "--delay-updates")]
    pub const fn delay_updates(mut self, delay: bool) -> Self {
        self.delay_updates = delay;
        self
    }

    /// Configures the directory used to store partial files when transfers fail.
    #[must_use]
    #[doc(alias = "--partial-dir")]
    pub fn partial_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.partial_dir = directory.map(Into::into);
        if self.partial_dir.is_some() {
            self.partial = true;
        }
        self
    }

    /// Configures the directory used for temporary files when staging updates.
    #[must_use]
    #[doc(alias = "--temp-dir")]
    #[doc(alias = "--tmp-dir")]
    pub fn temp_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.temp_directory = directory.map(Into::into);
        self
    }

    /// Enables or disables in-place updates for destination files.
    #[must_use]
    #[doc(alias = "--inplace")]
    #[doc(alias = "--no-inplace")]
    pub const fn inplace(mut self, inplace: bool) -> Self {
        self.inplace = inplace;
        self
    }

    /// Enables append-only transfers for existing destination files.
    #[must_use]
    #[doc(alias = "--append")]
    pub const fn append(mut self, append: bool) -> Self {
        self.append = append;
        if !append {
            self.append_verify = false;
        }
        self
    }

    /// Enables append verification for existing destination files.
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

    /// Requests that updated destination files be synchronised with storage after writing.
    #[must_use]
    #[doc(alias = "--fsync")]
    pub const fn fsync(mut self, fsync: bool) -> Self {
        self.fsync = fsync;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    #[test]
    fn partial_sets_flag() {
        let config = builder().partial(true).build();
        assert!(config.partial());
    }

    #[test]
    fn partial_false_clears_flag() {
        let config = builder().partial(true).partial(false).build();
        assert!(!config.partial());
    }

    #[test]
    fn delay_updates_sets_flag() {
        let config = builder().delay_updates(true).build();
        assert!(config.delay_updates());
    }

    #[test]
    fn partial_directory_sets_path() {
        let config = builder().partial_directory(Some("/tmp/partial")).build();
        assert!(config.partial_directory().is_some());
        assert_eq!(
            config.partial_directory().unwrap().to_str().unwrap(),
            "/tmp/partial"
        );
    }

    #[test]
    fn partial_directory_enables_partial() {
        let config = builder().partial_directory(Some("/tmp/partial")).build();
        assert!(config.partial());
    }

    #[test]
    fn partial_directory_none_clears_path() {
        let config = builder()
            .partial_directory(Some("/tmp/partial"))
            .partial_directory(None::<&str>)
            .build();
        assert!(config.partial_directory().is_none());
    }

    #[test]
    fn temp_directory_sets_path() {
        let config = builder().temp_directory(Some("/tmp/staging")).build();
        assert!(config.temp_directory().is_some());
    }

    #[test]
    fn temp_directory_none_clears_path() {
        let config = builder()
            .temp_directory(Some("/tmp/staging"))
            .temp_directory(None::<&str>)
            .build();
        assert!(config.temp_directory().is_none());
    }

    #[test]
    fn inplace_sets_flag() {
        let config = builder().inplace(true).build();
        assert!(config.inplace());
    }

    #[test]
    fn inplace_false_clears_flag() {
        let config = builder().inplace(true).inplace(false).build();
        assert!(!config.inplace());
    }

    #[test]
    fn append_sets_flag() {
        let config = builder().append(true).build();
        assert!(config.append());
    }

    #[test]
    fn append_false_clears_flag_and_verify() {
        let config = builder().append_verify(true).append(false).build();
        assert!(!config.append());
        assert!(!config.append_verify());
    }

    #[test]
    fn append_verify_enables_append() {
        let config = builder().append_verify(true).build();
        assert!(config.append());
        assert!(config.append_verify());
    }

    #[test]
    fn append_verify_false_only_clears_verify() {
        let config = builder().append_verify(true).append_verify(false).build();
        assert!(config.append());
        assert!(!config.append_verify());
    }

    #[test]
    fn fsync_sets_flag() {
        let config = builder().fsync(true).build();
        assert!(config.fsync());
    }

    #[test]
    fn fsync_false_clears_flag() {
        let config = builder().fsync(true).fsync(false).build();
        assert!(!config.fsync());
    }
}
