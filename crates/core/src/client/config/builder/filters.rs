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

    /// Replaces the collected explicit `--info` categories with the provided
    /// list.
    ///
    /// Each item is a normalized `name{level}` token; the remote builders
    /// forward these to the peer as `--info=`, mirroring upstream
    /// `make_output_option()` (`options.c:354`).
    #[must_use]
    #[doc(alias = "--info")]
    pub fn info_flags<I, S>(mut self, flags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.info_flags = flags.into_iter().map(Into::into).collect();
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
    ///
    /// A bare `--delay-updates` (with no explicit `--partial-dir`) uses the
    /// implicit `.~tmp~` staging directory, so the same protective exclude is
    /// injected for it. upstream: options.c:2421 `if (delay_updates &&
    /// !partial_dir) partial_dir = tmp_partialdir;` (`tmp_partialdir[] =
    /// ".~tmp~"`), consumed by the compat.c:791 filter injection.
    pub(super) fn push_implicit_partial_dir_filter(&mut self) {
        let mut pattern = match self.partial_dir.as_ref() {
            Some(dir) => dir.to_string_lossy().into_owned(),
            None if self.delay_updates => ".~tmp~".to_owned(),
            None => return,
        };
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

    /// Appends the implicit protect rule that upstream rsync injects to keep
    /// backup-suffix files out of a `--delete` sweep.
    ///
    /// upstream: options.c:2336-2339. When `make_backups && delete_mode &&
    /// !delete_excluded && !am_server` and no `--backup-dir` is set, rsync
    /// injects `P *<suffix>` (default suffix `~`) at the tail of the filter
    /// list via `parse_filter_str(&filter_list, "P *~", rule_template(0), 0)`.
    /// Backups are written beside the destination as `name<suffix>`, so without
    /// this rule the very files just saved as backups become extraneous entries
    /// and are removed by the delete pass. Appended after every CLI rule, it
    /// carries the lowest precedence, so a user rule matching the same pattern
    /// still wins under first-match evaluation.
    ///
    /// `am_server` is inherently false here: this builder constructs the client
    /// configuration only. A `--backup-dir` places backups in a separate tree,
    /// so no protect rule is needed (upstream: options.c:2328-2329). It is not
    /// marked perishable, mirroring upstream's `rule_template(0)`.
    pub(super) fn push_backup_protect_filter(&mut self) {
        if !self.backup || self.backup_dir.is_some() {
            return;
        }
        if !self.delete_mode.is_enabled() || self.delete_excluded {
            return;
        }
        // upstream: options.c:2296-2297 - the effective suffix defaults to `~`
        // when no `--backup-dir` is set (the only branch reached here).
        let suffix = self
            .backup_suffix
            .as_deref()
            .map_or_else(|| "~".to_owned(), |s| s.to_string_lossy().into_owned());
        self.filter_rules
            .push(FilterRuleSpec::protect(format!("*{suffix}")));
    }
}
