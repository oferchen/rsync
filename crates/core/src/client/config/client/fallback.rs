use super::*;

impl ClientConfig {
    /// Reports whether the client must delegate to the legacy rsync binary.
    #[must_use]
    pub const fn force_fallback(&self) -> bool {
        self.force_fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_fallback_default_is_false() {
        let config = ClientConfig::default();
        assert!(!config.force_fallback());
    }
}
