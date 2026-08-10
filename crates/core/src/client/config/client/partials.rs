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

    /// Returns the io_uring usage policy.
    #[must_use]
    #[doc(alias = "--io-uring")]
    #[doc(alias = "--no-io-uring")]
    pub const fn io_uring_policy(&self) -> fast_io::IoUringPolicy {
        self.io_uring_policy
    }

    /// Returns the io_uring submission queue depth override, if any.
    ///
    /// `None` means the default depth ([`fast_io::IoUringConfig::sq_entries`])
    /// is used; `Some(n)` overrides it with a user-supplied power-of-two value
    /// previously validated via [`fast_io::validate_io_uring_depth`].
    #[doc(alias = "--io-uring-depth")]
    pub const fn io_uring_depth(&self) -> Option<u32> {
        self.io_uring_depth
    }

    /// Returns the copy-on-write reflink policy for whole-file copies.
    #[must_use]
    #[doc(alias = "--cow")]
    #[doc(alias = "--no-cow")]
    pub const fn cow_policy(&self) -> fast_io::CowPolicy {
        self.cow_policy
    }

    /// Returns the I/O-level zero-copy policy.
    ///
    /// Controls `sendfile`, `splice`, `copy_file_range`, and io_uring
    /// `SEND_ZC`. Orthogonal to the cow policy which gates FS-level
    /// reflink/CoW cloning.
    #[must_use]
    #[doc(alias = "--zero-copy")]
    #[doc(alias = "--no-zero-copy")]
    pub const fn zero_copy_policy(&self) -> fast_io::ZeroCopyPolicy {
        self.zero_copy_policy
    }

    /// Returns whether the opt-in `--parallel-delta-scan` optimization is on.
    ///
    /// Local sender-side only: scans a large basis file's delta across
    /// multiple cores. Never changes the wire protocol and is never forwarded
    /// to a remote peer. Only engages for large, duplicate-free basis files;
    /// token boundaries and matched/literal stats may differ for basis files
    /// with duplicate-content blocks. Reconstruction is unaffected. Default
    /// off.
    #[must_use]
    #[doc(alias = "--parallel-delta-scan")]
    pub const fn parallel_delta_scan(&self) -> bool {
        self.parallel_delta_scan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    #[test]
    fn partial_default_is_false() {
        let config = default_config();
        assert!(!config.partial());
    }

    #[test]
    fn delay_updates_default_is_false() {
        let config = default_config();
        assert!(!config.delay_updates());
    }

    #[test]
    fn partial_directory_default_is_none() {
        let config = default_config();
        assert!(config.partial_directory().is_none());
    }

    #[test]
    fn temp_directory_default_is_none() {
        let config = default_config();
        assert!(config.temp_directory().is_none());
    }

    #[test]
    fn inplace_default_is_false() {
        let config = default_config();
        assert!(!config.inplace());
    }

    #[test]
    fn append_default_is_false() {
        let config = default_config();
        assert!(!config.append());
    }

    #[test]
    fn append_verify_default_is_false() {
        let config = default_config();
        assert!(!config.append_verify());
    }

    #[test]
    fn fsync_default_is_false() {
        let config = default_config();
        assert!(!config.fsync());
    }

    #[test]
    fn io_uring_policy_default_is_auto() {
        let config = default_config();
        assert_eq!(config.io_uring_policy(), fast_io::IoUringPolicy::Auto);
    }

    #[test]
    fn zero_copy_policy_default_is_auto() {
        let config = default_config();
        assert_eq!(config.zero_copy_policy(), fast_io::ZeroCopyPolicy::Auto);
    }

    #[test]
    fn io_uring_depth_default_is_none() {
        let config = default_config();
        assert_eq!(config.io_uring_depth(), None);
    }
}
