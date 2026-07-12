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

    /// Appends the implicit exclude rule that upstream rsync injects for a
    /// relative `--partial-dir`.
    ///
    /// upstream: compat.c:791-797 (`setup_protocol`). When `partial_dir` is
    /// set and relative (`*partial_dir != '/'`), rsync appends a
    /// directory-only exclude rule for the partial directory to the tail of
    /// the filter list. Being appended after every CLI rule, it carries the
    /// lowest precedence, so a user rule matching the same path still wins
    /// under first-match evaluation. The rule keeps the partial directory out
    /// of the sender's file list (it is neither listed nor transferred) and
    /// protects it from `--delete` on the receiver. It is marked perishable
    /// so an otherwise-empty parent directory can still be reaped
    /// (upstream sets `FILTRULE_PERISHABLE` at protocol >= 30, which is the
    /// negotiated default).
    ///
    /// An absolute partial directory is left untouched, matching upstream's
    /// `*partial_dir != '/'` guard.
    pub(super) fn push_implicit_partial_dir_filter(&mut self) {
        let Some(dir) = self.partial_dir.as_ref() else {
            return;
        };
        let mut pattern = dir.to_string_lossy().into_owned();
        if pattern.is_empty() {
            return;
        }
        // upstream: compat.c:791 guard `*partial_dir != '/'` - an absolute
        // (leading-slash) partial dir injects no implicit rule. Test the
        // leading slash directly rather than `Path::is_relative()`, which is
        // platform-dependent: on Windows a leading-slash path has no drive
        // prefix and is classified as relative, wrongly injecting the rule.
        if pattern.starts_with('/') {
            return;
        }
        // Trailing slash restricts the rule to directories, mirroring
        // upstream's FILTRULE_DIRECTORY flag.
        if !pattern.ends_with('/') {
            pattern.push('/');
        }
        self.filter_rules
            .push(FilterRuleSpec::exclude(pattern).with_perishable(true));
    }
}
