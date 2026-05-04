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
