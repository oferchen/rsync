use super::*;

impl ClientConfigBuilder {
    /// Replaces the collected debug flags with the provided list.
    #[must_use]
    #[doc(alias = "--debug")]
    pub fn debug_flags<I, S>(mut self, flags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.debug_flags = flags.into_iter().map(Into::into).collect();
        self
    }

    /// Appends a filter rule to the configuration being constructed.
    #[must_use]
    pub fn add_filter_rule(mut self, rule: FilterRuleSpec) -> Self {
        self.filter_rules.push(rule);
        self
    }

    /// Extends the builder with a collection of filter rules.
    #[must_use]
    pub fn extend_filter_rules<I>(mut self, rules: I) -> Self
    where
        I: IntoIterator<Item = FilterRuleSpec>,
    {
        self.filter_rules.extend(rules);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::config::FilterRuleKind;

    fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    #[test]
    fn debug_flags_sets_values() {
        let config = builder()
            .debug_flags(["FILTER", "SEND"])
            .build();
        assert_eq!(config.debug_flags().len(), 2);
    }

    #[test]
    fn debug_flags_empty_clears_values() {
        let config = builder()
            .debug_flags(["FILTER"])
            .debug_flags(Vec::<&str>::new())
            .build();
        assert!(config.debug_flags().is_empty());
    }

    #[test]
    fn debug_flags_accepts_osstrings() {
        let flags: Vec<OsString> = vec![OsString::from("DEBUG")];
        let config = builder().debug_flags(flags).build();
        assert_eq!(config.debug_flags().len(), 1);
    }

    #[test]
    fn add_filter_rule_appends_rule() {
        let config = builder()
            .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
            .build();
        assert_eq!(config.filter_rules().len(), 1);
        assert_eq!(config.filter_rules()[0].pattern(), "*.tmp");
    }

    #[test]
    fn add_filter_rule_multiple_accumulates() {
        let config = builder()
            .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
            .add_filter_rule(FilterRuleSpec::include("*.rs"))
            .add_filter_rule(FilterRuleSpec::protect("important"))
            .build();
        assert_eq!(config.filter_rules().len(), 3);
    }

    #[test]
    fn add_filter_rule_include() {
        let config = builder()
            .add_filter_rule(FilterRuleSpec::include("*.rs"))
            .build();
        assert_eq!(config.filter_rules()[0].kind(), FilterRuleKind::Include);
    }

    #[test]
    fn add_filter_rule_exclude() {
        let config = builder()
            .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
            .build();
        assert_eq!(config.filter_rules()[0].kind(), FilterRuleKind::Exclude);
    }

    #[test]
    fn add_filter_rule_protect() {
        let config = builder()
            .add_filter_rule(FilterRuleSpec::protect("keep"))
            .build();
        assert_eq!(config.filter_rules()[0].kind(), FilterRuleKind::Protect);
    }

    #[test]
    fn add_filter_rule_clear() {
        let config = builder()
            .add_filter_rule(FilterRuleSpec::clear())
            .build();
        assert_eq!(config.filter_rules()[0].kind(), FilterRuleKind::Clear);
    }

    #[test]
    fn extend_filter_rules_appends_collection() {
        let rules = vec![
            FilterRuleSpec::exclude("*.tmp"),
            FilterRuleSpec::exclude("*.bak"),
        ];
        let config = builder().extend_filter_rules(rules).build();
        assert_eq!(config.filter_rules().len(), 2);
    }

    #[test]
    fn extend_filter_rules_accumulates_with_add() {
        let rules = vec![
            FilterRuleSpec::exclude("*.tmp"),
            FilterRuleSpec::exclude("*.bak"),
        ];
        let config = builder()
            .add_filter_rule(FilterRuleSpec::include("*.rs"))
            .extend_filter_rules(rules)
            .build();
        assert_eq!(config.filter_rules().len(), 3);
    }

    #[test]
    fn extend_filter_rules_empty_adds_nothing() {
        let config = builder()
            .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
            .extend_filter_rules(Vec::new())
            .build();
        assert_eq!(config.filter_rules().len(), 1);
    }

    #[test]
    fn default_debug_flags_is_empty() {
        let config = builder().build();
        assert!(config.debug_flags().is_empty());
    }

    #[test]
    fn default_filter_rules_is_empty() {
        let config = builder().build();
        assert!(config.filter_rules().is_empty());
    }
}
