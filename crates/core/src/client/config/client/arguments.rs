use super::*;

impl ClientConfig {
    /// Returns the raw transfer arguments provided by the caller.
    #[must_use]
    pub fn transfer_args(&self) -> &[OsString] {
        &self.transfer_args
    }

    /// Returns the ordered reference directories supplied via `--compare-dest`,
    /// `--copy-dest`, or `--link-dest`.
    #[must_use]
    #[doc(alias = "--compare-dest")]
    #[doc(alias = "--copy-dest")]
    #[doc(alias = "--link-dest")]
    pub fn reference_directories(&self) -> &[ReferenceDirectory] {
        &self.reference_directories
    }

    /// Reports whether transfers should be listed without mutating the destination.
    #[must_use]
    #[doc(alias = "--list-only")]
    pub const fn list_only(&self) -> bool {
        self.list_only
    }

    /// Reports whether a transfer was explicitly requested.
    #[must_use]
    pub const fn has_transfer_request(&self) -> bool {
        !self.transfer_args.is_empty()
    }

    /// Returns the configured batch mode settings, if any.
    #[must_use]
    #[doc(alias = "--write-batch")]
    #[doc(alias = "--only-write-batch")]
    #[doc(alias = "--read-batch")]
    pub const fn batch_config(&self) -> Option<&engine::batch::BatchConfig> {
        self.batch_config.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    // Tests for transfer_args
    #[test]
    fn transfer_args_default_is_empty() {
        let config = default_config();
        assert!(config.transfer_args().is_empty());
    }

    // Tests for reference_directories
    #[test]
    fn reference_directories_default_is_empty() {
        let config = default_config();
        assert!(config.reference_directories().is_empty());
    }

    // Tests for list_only
    #[test]
    fn list_only_default_is_false() {
        let config = default_config();
        assert!(!config.list_only());
    }

    // Tests for has_transfer_request
    #[test]
    fn has_transfer_request_default_is_false() {
        let config = default_config();
        assert!(!config.has_transfer_request());
    }

    // Tests for batch_config
    #[test]
    fn batch_config_default_is_none() {
        let config = default_config();
        assert!(config.batch_config().is_none());
    }
}
