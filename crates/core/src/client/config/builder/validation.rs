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

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    #[test]
    fn checksum_sets_flag() {
        let config = builder().checksum(true).build();
        assert!(config.checksum());
    }

    #[test]
    fn checksum_false_clears_flag() {
        let config = builder()
            .checksum(true)
            .checksum(false)
            .build();
        assert!(!config.checksum());
    }

    #[test]
    fn checksum_choice_sets_value() {
        let choice = StrongChecksumChoice::parse("xxh3").unwrap();
        let config = builder()
            .checksum_choice(choice)
            .build();
        // Verify the choice was set by checking the argument representation
        assert_eq!(config.checksum_choice().to_argument(), "xxh3");
    }

    #[test]
    fn checksum_choice_md5() {
        let choice = StrongChecksumChoice::parse("md5").unwrap();
        let config = builder()
            .checksum_choice(choice)
            .build();
        assert_eq!(config.checksum_choice().to_argument(), "md5");
    }

    #[test]
    fn checksum_seed_sets_value() {
        let config = builder().checksum_seed(Some(12345)).build();
        assert_eq!(config.checksum_seed(), Some(12345));
    }

    #[test]
    fn checksum_seed_none_clears_value() {
        let config = builder()
            .checksum_seed(Some(12345))
            .checksum_seed(None)
            .build();
        assert!(config.checksum_seed().is_none());
    }

    #[test]
    fn force_event_collection_sets_flag() {
        let config = builder().force_event_collection(true).build();
        assert!(config.force_event_collection());
    }

    #[test]
    fn force_event_collection_false_clears_flag() {
        let config = builder()
            .force_event_collection(true)
            .force_event_collection(false)
            .build();
        assert!(!config.force_event_collection());
    }

    #[test]
    fn default_checksum_is_false() {
        let config = builder().build();
        assert!(!config.checksum());
    }

    #[test]
    fn default_checksum_seed_is_none() {
        let config = builder().build();
        assert!(config.checksum_seed().is_none());
    }

    #[test]
    fn default_force_event_collection_is_false() {
        let config = builder().build();
        assert!(!config.force_event_collection());
    }
}
