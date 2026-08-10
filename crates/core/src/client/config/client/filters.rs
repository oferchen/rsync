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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::config::filters::FilterRuleKind;
    use std::ffi::OsString;

    #[test]
    fn filter_rules_preserve_builder_order() {
        let config = ClientConfig::builder()
            .add_filter_rule(FilterRuleSpec::include("*.rs"))
            .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
            .build();

        let rules = config.filter_rules();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "*.rs");
        assert_eq!(rules[0].kind(), FilterRuleKind::Include);
        assert_eq!(rules[1].pattern(), "*.tmp");
        assert_eq!(rules[1].kind(), FilterRuleKind::Exclude);
    }

    #[test]
    fn debug_flags_expose_supplied_values() {
        let config = ClientConfig::builder()
            .debug_flags([OsString::from("io"), OsString::from("stats")])
            .build();

        let flags = config.debug_flags();
        assert_eq!(flags, &[OsString::from("io"), OsString::from("stats")]);
    }
}
