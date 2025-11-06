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
