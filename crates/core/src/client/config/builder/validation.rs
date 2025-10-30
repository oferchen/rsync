use super::*;

impl ClientConfigBuilder {
    /// Enables or disables checksum-based change detection.
    #[must_use]
    #[doc(alias = "--checksum")]
    #[doc(alias = "-c")]
    pub const fn checksum(mut self, checksum: bool) -> Self {
        self.checksum = checksum;
        self
    }

    /// Overrides the strong checksum selection used during validation.
    #[must_use]
    #[doc(alias = "--checksum-choice")]
    pub const fn checksum_choice(mut self, choice: StrongChecksumChoice) -> Self {
        self.checksum_choice = choice;
        self
    }

    /// Configures the checksum seed forwarded to the engine and fallback binary.
    #[must_use]
    #[doc(alias = "--checksum-seed")]
    pub const fn checksum_seed(mut self, seed: Option<u32>) -> Self {
        self.checksum_seed = seed;
        self
    }

    /// Forces collection of transfer events regardless of verbosity.
    #[must_use]
    pub const fn force_event_collection(mut self, force: bool) -> Self {
        self.force_event_collection = force;
        self
    }
}
