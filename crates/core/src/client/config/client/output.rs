use super::*;

impl ClientConfig {
    /// Returns the requested verbosity level.
    #[must_use]
    #[doc(alias = "--verbose")]
    #[doc(alias = "-v")]
    pub const fn verbosity(&self) -> u8 {
        self.verbosity
    }

    /// Reports whether progress output was requested.
    #[must_use]
    #[doc(alias = "--progress")]
    pub const fn progress(&self) -> bool {
        self.progress
    }

    /// Reports whether a statistics summary should be emitted after the transfer.
    #[must_use]
    #[doc(alias = "--stats")]
    pub const fn stats(&self) -> bool {
        self.stats
    }

    /// Reports whether human-readable formatting should be applied to byte counts.
    #[must_use]
    #[doc(alias = "--human-readable")]
    pub const fn human_readable(&self) -> bool {
        self.human_readable
    }

    /// Reports whether itemize-changes output was requested.
    ///
    /// When true, the transfer emits per-file change summaries in the
    /// 11-character `YXcstpoguax` format. For remote transfers, this flag
    /// is forwarded to the server so it emits itemize output as MSG_INFO.
    ///
    /// upstream: options.c - `itemize_changes` / `-i`
    #[must_use]
    #[doc(alias = "--itemize-changes")]
    #[doc(alias = "-i")]
    pub const fn itemize_changes(&self) -> bool {
        self.itemize_changes
    }

    /// Reports whether event collection has been explicitly requested by the caller.
    #[must_use]
    pub const fn force_event_collection(&self) -> bool {
        self.force_event_collection
    }

    /// Returns whether the configuration requires collection of transfer events.
    #[must_use]
    pub const fn collect_events(&self) -> bool {
        self.force_event_collection || self.verbosity > 0 || self.progress || self.list_only
    }

    /// Reports whether daemon MOTD output should be suppressed.
    #[must_use]
    #[doc(alias = "--no-motd")]
    pub const fn no_motd(&self) -> bool {
        self.no_motd
    }

    /// Returns the daemon parameter overrides to send during the daemon handshake.
    ///
    /// Each entry is a `key=value` string that overrides a module-level
    /// configuration directive. Mirrors upstream rsync's `--dparam` / `-M` option.
    #[must_use]
    #[doc(alias = "--dparam")]
    #[doc(alias = "-M")]
    pub fn daemon_params(&self) -> &[String] {
        &self.daemon_params
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    #[test]
    fn verbosity_default_is_zero() {
        let config = default_config();
        assert_eq!(config.verbosity(), 0);
    }

    #[test]
    fn progress_default_is_false() {
        let config = default_config();
        assert!(!config.progress());
    }

    #[test]
    fn stats_default_is_false() {
        let config = default_config();
        assert!(!config.stats());
    }

    #[test]
    fn human_readable_default_is_false() {
        let config = default_config();
        assert!(!config.human_readable());
    }

    #[test]
    fn itemize_changes_default_is_false() {
        let config = default_config();
        assert!(!config.itemize_changes());
    }

    #[test]
    fn force_event_collection_default_is_false() {
        let config = default_config();
        assert!(!config.force_event_collection());
    }

    #[test]
    fn collect_events_default_is_false() {
        let config = default_config();
        assert!(!config.collect_events());
    }

    #[test]
    fn daemon_params_default_is_empty() {
        let config = default_config();
        assert!(config.daemon_params().is_empty());
    }
}
