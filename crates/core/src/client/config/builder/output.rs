use super::*;

impl ClientConfigBuilder {
    /// Sets the verbosity level requested by the caller.
    #[must_use]
    #[doc(alias = "--verbose")]
    #[doc(alias = "-v")]
    pub const fn verbosity(mut self, verbosity: u8) -> Self {
        self.verbosity = verbosity;
        self
    }

    /// Enables or disables progress reporting for the transfer.
    #[must_use]
    #[doc(alias = "--progress")]
    #[doc(alias = "--no-progress")]
    pub const fn progress(mut self, progress: bool) -> Self {
        self.progress = progress;
        self
    }

    /// Enables or disables statistics reporting for the transfer.
    #[must_use]
    #[doc(alias = "--stats")]
    pub const fn stats(mut self, stats: bool) -> Self {
        self.stats = stats;
        self
    }

    /// Enables or disables human-readable output formatting.
    #[must_use]
    #[doc(alias = "--human-readable")]
    pub const fn human_readable(mut self, enabled: bool) -> Self {
        self.human_readable = enabled;
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
    fn verbosity_sets_level() {
        let config = builder().verbosity(2).build();
        assert_eq!(config.verbosity(), 2);
    }

    #[test]
    fn verbosity_zero() {
        let config = builder().verbosity(0).build();
        assert_eq!(config.verbosity(), 0);
    }

    #[test]
    fn verbosity_max() {
        let config = builder().verbosity(u8::MAX).build();
        assert_eq!(config.verbosity(), u8::MAX);
    }

    #[test]
    fn progress_sets_flag() {
        let config = builder().progress(true).build();
        assert!(config.progress());
    }

    #[test]
    fn progress_false_clears_flag() {
        let config = builder().progress(true).progress(false).build();
        assert!(!config.progress());
    }

    #[test]
    fn stats_sets_flag() {
        let config = builder().stats(true).build();
        assert!(config.stats());
    }

    #[test]
    fn stats_false_clears_flag() {
        let config = builder().stats(true).stats(false).build();
        assert!(!config.stats());
    }

    #[test]
    fn human_readable_sets_flag() {
        let config = builder().human_readable(true).build();
        assert!(config.human_readable());
    }

    #[test]
    fn human_readable_false_clears_flag() {
        let config = builder().human_readable(true).human_readable(false).build();
        assert!(!config.human_readable());
    }

    #[test]
    fn default_verbosity_is_zero() {
        let config = builder().build();
        assert_eq!(config.verbosity(), 0);
    }

    #[test]
    fn default_progress_is_false() {
        let config = builder().build();
        assert!(!config.progress());
    }

    #[test]
    fn default_stats_is_false() {
        let config = builder().build();
        assert!(!config.stats());
    }

    #[test]
    fn default_human_readable_is_false() {
        let config = builder().build();
        assert!(!config.human_readable());
    }
}
