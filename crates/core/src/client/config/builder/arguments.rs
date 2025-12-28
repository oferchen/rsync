use super::*;

impl ClientConfigBuilder {
    /// Sets the transfer arguments that should be propagated to the engine.
    #[must_use]
    pub fn transfer_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.transfer_args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Enables or disables dry-run mode.
    #[must_use]
    #[doc(alias = "--dry-run")]
    #[doc(alias = "-n")]
    pub const fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Enables or disables list-only mode, mirroring `--list-only`.
    #[must_use]
    #[doc(alias = "--list-only")]
    pub const fn list_only(mut self, list_only: bool) -> Self {
        self.list_only = list_only;
        self
    }

    /// Configures batch mode for offline/disconnected transfer workflows.
    ///
    /// This method accepts an optional `BatchConfig` which determines how the
    /// transfer will interact with batch files.
    ///
    /// # Example
    /// ```
    /// # use core::client::ClientConfig;
    /// # use engine::batch::{BatchMode, BatchConfig};
    /// let batch_config = BatchConfig::new(
    ///     BatchMode::Write,
    ///     "mybatch".to_string(),
    ///     32,
    /// );
    /// let config = ClientConfig::builder()
    ///     .batch_config(Some(batch_config))
    ///     .build();
    /// ```
    #[must_use]
    #[doc(alias = "--write-batch")]
    #[doc(alias = "--only-write-batch")]
    #[doc(alias = "--read-batch")]
    pub fn batch_config(mut self, config: Option<engine::batch::BatchConfig>) -> Self {
        self.batch_config = config;
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
    fn transfer_args_sets_values() {
        let config = builder().transfer_args(["--verbose", "--progress"]).build();
        assert_eq!(config.transfer_args().len(), 2);
    }

    #[test]
    fn transfer_args_empty_clears_values() {
        let config = builder()
            .transfer_args(["--verbose"])
            .transfer_args(Vec::<&str>::new())
            .build();
        assert!(config.transfer_args().is_empty());
    }

    #[test]
    fn transfer_args_accepts_osstrings() {
        let args: Vec<OsString> = vec![OsString::from("--test")];
        let config = builder().transfer_args(args).build();
        assert_eq!(config.transfer_args().len(), 1);
    }

    #[test]
    fn dry_run_sets_flag() {
        let config = builder().dry_run(true).build();
        assert!(config.dry_run());
    }

    #[test]
    fn dry_run_false_clears_flag() {
        let config = builder().dry_run(true).dry_run(false).build();
        assert!(!config.dry_run());
    }

    #[test]
    fn list_only_sets_flag() {
        let config = builder().list_only(true).build();
        assert!(config.list_only());
    }

    #[test]
    fn list_only_false_clears_flag() {
        let config = builder().list_only(true).list_only(false).build();
        assert!(!config.list_only());
    }

    #[test]
    fn batch_config_sets_value() {
        let batch = engine::batch::BatchConfig::new(
            engine::batch::BatchMode::Write,
            "testbatch".to_owned(),
            32,
        );
        let config = builder().batch_config(Some(batch)).build();
        assert!(config.batch_config().is_some());
    }

    #[test]
    fn batch_config_none_clears_value() {
        let batch = engine::batch::BatchConfig::new(
            engine::batch::BatchMode::Write,
            "testbatch".to_owned(),
            32,
        );
        let config = builder()
            .batch_config(Some(batch))
            .batch_config(None)
            .build();
        assert!(config.batch_config().is_none());
    }

    #[test]
    fn default_transfer_args_is_empty() {
        let config = builder().build();
        assert!(config.transfer_args().is_empty());
    }

    #[test]
    fn default_dry_run_is_false() {
        let config = builder().build();
        assert!(!config.dry_run());
    }

    #[test]
    fn default_list_only_is_false() {
        let config = builder().build();
        assert!(!config.list_only());
    }

    #[test]
    fn default_batch_config_is_none() {
        let config = builder().build();
        assert!(config.batch_config().is_none());
    }
}
