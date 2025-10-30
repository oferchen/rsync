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
