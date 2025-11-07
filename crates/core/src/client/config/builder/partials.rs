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
