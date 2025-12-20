use super::*;

impl ClientConfigBuilder {
    /// Enables or disables deletion of extraneous destination files.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(mut self, delete: bool) -> Self {
        self.delete_mode = if delete {
            DeleteMode::During
        } else {
            DeleteMode::Disabled
        };
        self
    }

    /// Requests deletion of extraneous entries before the transfer begins.
    #[must_use]
    #[doc(alias = "--delete-before")]
    pub const fn delete_before(mut self, delete_before: bool) -> Self {
        if delete_before {
            self.delete_mode = DeleteMode::Before;
        } else if matches!(self.delete_mode, DeleteMode::Before) {
            self.delete_mode = DeleteMode::Disabled;
        }
        self
    }

    /// Requests deletion of extraneous entries while directories are processed.
    #[must_use]
    #[doc(alias = "--delete-during")]
    pub const fn delete_during(mut self) -> Self {
        self.delete_mode = DeleteMode::During;
        self
    }

    /// Enables deletion of extraneous entries after the transfer completes.
    #[must_use]
    #[doc(alias = "--delete-after")]
    pub const fn delete_after(mut self, delete_after: bool) -> Self {
        if delete_after {
            self.delete_mode = DeleteMode::After;
        } else if matches!(self.delete_mode, DeleteMode::After) {
            self.delete_mode = DeleteMode::Disabled;
        }
        self
    }

    /// Requests delayed deletion sweeps that run after transfers complete.
    #[must_use]
    #[doc(alias = "--delete-delay")]
    pub const fn delete_delay(mut self, delete_delay: bool) -> Self {
        if delete_delay {
            self.delete_mode = DeleteMode::Delay;
        } else if matches!(self.delete_mode, DeleteMode::Delay) {
            self.delete_mode = DeleteMode::Disabled;
        }
        self
    }

    /// Enables or disables deletion of excluded destination entries.
    #[must_use]
    #[doc(alias = "--delete-excluded")]
    pub const fn delete_excluded(mut self, delete: bool) -> Self {
        self.delete_excluded = delete;
        self
    }

    /// Sets the maximum number of deletions permitted during execution.
    #[must_use]
    #[doc(alias = "--max-delete")]
    pub const fn max_delete(mut self, limit: Option<u64>) -> Self {
        self.max_delete = limit;
        self
    }

    /// Configures whether deletions should proceed even when I/O errors occur.
    ///
    /// When enabled, rsync will continue with the deletion phase even if
    /// there were I/O errors during the transfer. Without this flag,
    /// I/O errors cause the deletion phase to be skipped to prevent
    /// accidental data loss.
    #[must_use]
    #[doc(alias = "--ignore-errors")]
    pub const fn ignore_errors(mut self, ignore: bool) -> Self {
        self.ignore_errors = ignore;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(builder: ClientConfigBuilder) -> ClientConfig {
        builder.build()
    }

    #[test]
    fn delete_sets_during_and_resets_to_disabled() {
        let config = build(ClientConfigBuilder::default().delete(true));
        assert_eq!(config.delete_mode(), DeleteMode::During);
        assert!(config.delete());

        let config = build(ClientConfigBuilder::default().delete(true).delete(false));
        assert_eq!(config.delete_mode(), DeleteMode::Disabled);
        assert!(!config.delete());
    }

    #[test]
    fn delete_before_toggles_mode() {
        let config = build(ClientConfigBuilder::default().delete_before(true));
        assert!(config.delete_before());
        assert_eq!(config.delete_mode(), DeleteMode::Before);

        let config = build(
            ClientConfigBuilder::default()
                .delete_before(true)
                .delete_before(false),
        );
        assert!(!config.delete_before());
        assert_eq!(config.delete_mode(), DeleteMode::Disabled);
    }

    #[test]
    fn delete_after_toggles_mode() {
        let config = build(ClientConfigBuilder::default().delete_after(true));
        assert!(config.delete_after());
        assert_eq!(config.delete_mode(), DeleteMode::After);

        let config = build(
            ClientConfigBuilder::default()
                .delete_after(true)
                .delete_after(false),
        );
        assert!(!config.delete_after());
        assert_eq!(config.delete_mode(), DeleteMode::Disabled);
    }

    #[test]
    fn delete_delay_toggles_mode() {
        let config = build(ClientConfigBuilder::default().delete_delay(true));
        assert!(config.delete_delay());
        assert_eq!(config.delete_mode(), DeleteMode::Delay);

        let config = build(
            ClientConfigBuilder::default()
                .delete_delay(true)
                .delete_delay(false),
        );
        assert!(!config.delete_delay());
        assert_eq!(config.delete_mode(), DeleteMode::Disabled);
    }

    #[test]
    fn delete_excluded_mirrors_builder_setting() {
        let config = build(ClientConfigBuilder::default().delete_excluded(true));
        assert!(config.delete_excluded());

        let config = build(ClientConfigBuilder::default().delete_excluded(false));
        assert!(!config.delete_excluded());
    }

    #[test]
    fn max_delete_propagates_limit() {
        let config = build(ClientConfigBuilder::default().max_delete(Some(128)));
        assert_eq!(config.max_delete(), Some(128));

        let config = build(ClientConfigBuilder::default().max_delete(None));
        assert_eq!(config.max_delete(), None);
    }
}
