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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    // Tests for verbosity
    #[test]
    fn verbosity_default_is_zero() {
        let config = default_config();
        assert_eq!(config.verbosity(), 0);
    }

    // Tests for progress
    #[test]
    fn progress_default_is_false() {
        let config = default_config();
        assert!(!config.progress());
    }

    // Tests for stats
    #[test]
    fn stats_default_is_false() {
        let config = default_config();
        assert!(!config.stats());
    }

    // Tests for human readable
    #[test]
    fn human_readable_default_is_false() {
        let config = default_config();
        assert!(!config.human_readable());
    }

    // Tests for force event collection
    #[test]
    fn force_event_collection_default_is_false() {
        let config = default_config();
        assert!(!config.force_event_collection());
    }

    // Tests for collect_events
    #[test]
    fn collect_events_default_is_false() {
        let config = default_config();
        // By default: force_event_collection=false, verbosity=0, progress=false, list_only=false
        assert!(!config.collect_events());
    }
}
