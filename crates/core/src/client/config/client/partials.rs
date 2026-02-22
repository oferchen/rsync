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

    /// Reports whether destination files should be fsynced after updates complete.
    #[must_use]
    #[doc(alias = "--fsync")]
    pub const fn fsync(&self) -> bool {
        self.fsync
    }

    /// Reports whether the direct write optimization is enabled.
    #[must_use]
    #[doc(alias = "--direct-write")]
    pub const fn direct_write(&self) -> bool {
        self.direct_write
    }

    /// Returns the io_uring usage policy.
    #[must_use]
    #[doc(alias = "--io-uring")]
    #[doc(alias = "--no-io-uring")]
    pub const fn io_uring_policy(&self) -> fast_io::IoUringPolicy {
        self.io_uring_policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    // Tests for partial
    #[test]
    fn partial_default_is_false() {
        let config = default_config();
        assert!(!config.partial());
    }

    // Tests for delay_updates
    #[test]
    fn delay_updates_default_is_false() {
        let config = default_config();
        assert!(!config.delay_updates());
    }

    // Tests for partial_directory
    #[test]
    fn partial_directory_default_is_none() {
        let config = default_config();
        assert!(config.partial_directory().is_none());
    }

    // Tests for temp_directory
    #[test]
    fn temp_directory_default_is_none() {
        let config = default_config();
        assert!(config.temp_directory().is_none());
    }

    // Tests for inplace
    #[test]
    fn inplace_default_is_false() {
        let config = default_config();
        assert!(!config.inplace());
    }

    // Tests for append
    #[test]
    fn append_default_is_false() {
        let config = default_config();
        assert!(!config.append());
    }

    // Tests for append_verify
    #[test]
    fn append_verify_default_is_false() {
        let config = default_config();
        assert!(!config.append_verify());
    }

    // Tests for fsync
    #[test]
    fn fsync_default_is_false() {
        let config = default_config();
        assert!(!config.fsync());
    }

    // Tests for direct_write
    #[test]
    fn direct_write_default_is_false() {
        let config = default_config();
        assert!(!config.direct_write());
    }

    #[test]
    fn io_uring_policy_default_is_auto() {
        let config = default_config();
        assert_eq!(config.io_uring_policy(), fast_io::IoUringPolicy::Auto);
    }
}
