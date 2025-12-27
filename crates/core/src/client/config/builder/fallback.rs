use super::*;

impl ClientConfigBuilder {
    /// Forces the client orchestration to delegate to the legacy rsync binary.
    ///
    /// The native engine does not yet support batch file generation or replay,
    /// so the CLI triggers delegation when `--write-batch`,
    /// `--only-write-batch`, or `--read-batch` is supplied. Setting this flag
    /// ensures [`run_client_or_fallback`](crate::client::run_client_or_fallback)
    /// invokes the fallback even when the local plan would otherwise be
    /// executable.
    #[must_use]
    pub const fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
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
    fn force_fallback_sets_flag() {
        let config = builder().force_fallback(true).build();
        assert!(config.force_fallback());
    }

    #[test]
    fn force_fallback_false_clears_flag() {
        let config = builder().force_fallback(true).force_fallback(false).build();
        assert!(!config.force_fallback());
    }

    #[test]
    fn default_force_fallback_is_false() {
        let config = builder().build();
        assert!(!config.force_fallback());
    }
}
