use super::*;

impl ClientConfig {
    /// Reports whether the client must delegate to the legacy rsync binary.
    #[must_use]
    pub const fn force_fallback(&self) -> bool {
        self.force_fallback
    }
}
