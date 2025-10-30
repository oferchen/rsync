use super::*;

impl ClientConfig {
    /// Reports whether partial transfers were requested.
    #[must_use]
    #[doc(alias = "--partial")]
    #[doc(alias = "-P")]
    pub const fn partial(&self) -> bool {
        self.partial
    }

    /// Reports whether updates should be delayed until after the transfer completes.
    #[must_use]
    #[doc(alias = "--delay-updates")]
    pub const fn delay_updates(&self) -> bool {
        self.delay_updates
    }

    /// Returns the optional directory used to store partial files.
    #[must_use]
    #[doc(alias = "--partial-dir")]
    pub fn partial_directory(&self) -> Option<&Path> {
        self.partial_dir.as_deref()
    }

    /// Returns the configured temporary directory used for staged updates.
    #[doc(alias = "--temp-dir")]
    #[doc(alias = "--tmp-dir")]
    pub fn temp_directory(&self) -> Option<&Path> {
        self.temp_directory.as_deref()
    }

    /// Reports whether destination updates should be performed in place.
    #[must_use]
    #[doc(alias = "--inplace")]
    pub const fn inplace(&self) -> bool {
        self.inplace
    }

    /// Reports whether appended transfers are enabled.
    #[must_use]
    #[doc(alias = "--append")]
    pub const fn append(&self) -> bool {
        self.append
    }

    /// Reports whether append verification is enabled.
    #[must_use]
    #[doc(alias = "--append-verify")]
    pub const fn append_verify(&self) -> bool {
        self.append_verify
    }
}
