use super::*;

impl ClientConfig {
    /// Returns the ordered list of filter rules supplied by the caller.
    #[must_use]
    pub fn filter_rules(&self) -> &[FilterRuleSpec] {
        &self.filter_rules
    }

    /// Returns the debug categories requested via `--debug`.
    #[must_use]
    #[doc(alias = "--debug")]
    pub fn debug_flags(&self) -> &[OsString] {
        &self.debug_flags
    }
}
