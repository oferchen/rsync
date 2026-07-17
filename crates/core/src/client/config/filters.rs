use engine::local_copy::DirMergeOptions;

/// Classifies a filter rule as inclusive or exclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterRuleKind {
    /// Include matching paths.
    Include,
    /// Exclude matching paths.
    Exclude,
    /// Clear all previously defined filter rules.
    Clear,
    /// Protect matching destination paths from deletion.
    Protect,
    /// Remove protection for matching destination paths.
    Risk,
    /// Merge per-directory filter rules from `.rsync-filter` style files.
    DirMerge,
    /// Exclude directories containing a specific marker file.
    ExcludeIfPresent,
}

/// Filter rule supplied by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterRuleSpec {
    kind: FilterRuleKind,
    pattern: String,
    dir_merge_options: Option<DirMergeOptions>,
    applies_to_sender: bool,
    applies_to_receiver: bool,
    perishable: bool,
    xattr_only: bool,
    negate: bool,
    /// Marks a rule produced by the `-C`/`--cvs-exclude` built-in expansion so
    /// the wire projection can reproduce upstream's CVS send gating (local on a
    /// receiving client; `:C` only on protocol >= 29).
    ///
    /// upstream: exclude.c:1652-1668 send_filter_list().
    cvs_origin: bool,
}

impl FilterRuleSpec {
    /// Creates an include rule for the given pattern text.
    #[must_use]
    #[doc(alias = "show")]
    pub fn include(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Include,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            cvs_origin: false,
        }
    }

    /// Creates an exclude rule for the given pattern text.
    #[must_use]
    #[doc(alias = "hide")]
    pub fn exclude(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Exclude,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            cvs_origin: false,
        }
    }

    /// Creates a rule that clears previously defined filter rules.
    #[must_use]
    pub const fn clear() -> Self {
        Self {
            kind: FilterRuleKind::Clear,
            pattern: String::new(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            cvs_origin: false,
        }
    }

    /// Creates a protection rule for deletion passes.
    #[must_use]
    pub fn protect(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Protect,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: false,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            cvs_origin: false,
        }
    }

    /// Creates a deletion rule that removes previously protected entries.
    #[must_use]
    pub fn risk(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Risk,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: false,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            cvs_origin: false,
        }
    }

    /// Creates a per-directory merge rule mirroring `--filter=': {RULE}'`.
    #[must_use]
    #[doc(alias = ":")]
    pub fn dir_merge(pattern: impl Into<String>, options: DirMergeOptions) -> Self {
        Self {
            kind: FilterRuleKind::DirMerge,
            pattern: pattern.into(),
            dir_merge_options: Some(options),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            cvs_origin: false,
        }
    }

    /// Creates an `exclude-if-present` rule mirroring `--filter='H {FILE}'`.
    #[must_use]
    #[doc(alias = "H")]
    pub fn exclude_if_present(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::ExcludeIfPresent,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            cvs_origin: false,
        }
    }

    /// Creates an include rule that only affects the sending side.
    #[must_use]
    pub fn show(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Include,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: false,
            perishable: false,
            xattr_only: false,
            negate: false,
            cvs_origin: false,
        }
    }

    /// Creates an exclude rule that only affects the sending side.
    #[must_use]
    pub fn hide(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Exclude,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: false,
            perishable: false,
            xattr_only: false,
            negate: false,
            cvs_origin: false,
        }
    }

    /// Applies per-directory merge options to the rule.
    pub const fn with_dir_merge_options(mut self, options: DirMergeOptions) -> Self {
        self.dir_merge_options = Some(options);
        self
    }

    /// Returns the rule kind.
    #[must_use]
    pub const fn kind(&self) -> FilterRuleKind {
        self.kind
    }

    /// Returns the pattern text associated with the rule.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Returns whether the rule should be ignored when pruning directories.
    #[must_use]
    pub const fn is_perishable(&self) -> bool {
        self.perishable
    }

    /// Returns the per-directory merge options when present.
    pub const fn dir_merge_options(&self) -> Option<&DirMergeOptions> {
        self.dir_merge_options.as_ref()
    }

    /// Reports whether the rule applies to the sending side.
    #[must_use]
    pub const fn applies_to_sender(&self) -> bool {
        self.applies_to_sender
    }

    /// Reports whether the rule applies to the receiving side.
    #[must_use]
    pub const fn applies_to_receiver(&self) -> bool {
        self.applies_to_receiver
    }

    /// Applies the implicit sender-side flag that upstream rsync sets when
    /// `--delete-excluded` is active and the rule carries no explicit side
    /// modifier.
    ///
    /// Returns `true` if the rule was mutated.
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1330-1332` (`add_rule`):
    ///
    /// ```c
    /// if (delete_excluded
    ///  && !(rule->rflags & (FILTRULES_SIDES|FILTRULE_MERGE_FILE|FILTRULE_PERDIR_MERGE)))
    ///     rule->rflags |= FILTRULE_SENDER_SIDE;
    /// ```
    ///
    /// In oc-rsync, `applies_to_sender == true && applies_to_receiver == true`
    /// represents "no `FILTRULES_SIDES` bit set" - the rule applies to both
    /// sides by default. Merge and dir-merge rules are excluded because they
    /// expand into per-file rules that carry their own side information.
    /// Protect and Risk rules already restrict themselves to the receiver
    /// side, so the implicit flag never fires for them.
    pub fn apply_implicit_sender_side_for_delete_excluded(&mut self) -> bool {
        if !matches!(self.kind, FilterRuleKind::Include | FilterRuleKind::Exclude) {
            return false;
        }
        if !(self.applies_to_sender && self.applies_to_receiver) {
            return false;
        }
        self.applies_to_receiver = false;
        true
    }

    /// Applies dir-merge style overrides (anchor, side modifiers) to the rule.
    pub fn apply_dir_merge_overrides(&mut self, options: &DirMergeOptions) {
        if matches!(self.kind, FilterRuleKind::Clear) {
            return;
        }

        if self.xattr_only {
            // Xattr-only rules are not subject to per-directory overrides.
            return;
        }

        if options.perishable() {
            self.perishable = true;
        }

        if options.anchor_root_enabled() && !self.pattern.starts_with('/') {
            self.pattern.insert(0, '/');
        }

        if let Some(sender) = options.sender_side_override() {
            self.applies_to_sender = sender;
        }

        if let Some(receiver) = options.receiver_side_override() {
            self.applies_to_receiver = receiver;
        }
    }

    /// Marks the rule as perishable.
    #[must_use]
    pub const fn with_perishable(mut self, perishable: bool) -> Self {
        self.perishable = perishable;
        self
    }

    /// Marks the rule as applying exclusively to xattr names.
    #[must_use]
    pub const fn with_xattr_only(mut self, xattr_only: bool) -> Self {
        self.xattr_only = xattr_only;
        self
    }

    /// Reports whether the rule applies exclusively to xattr names.
    #[must_use]
    pub const fn is_xattr_only(&self) -> bool {
        self.xattr_only
    }

    /// Applies sender-side overrides.
    #[must_use]
    pub const fn with_sender(mut self, applies: bool) -> Self {
        self.applies_to_sender = applies;
        self
    }

    /// Applies receiver-side overrides.
    #[must_use]
    pub const fn with_receiver(mut self, applies: bool) -> Self {
        self.applies_to_receiver = applies;
        self
    }

    /// Inverts the match result of the pattern.
    ///
    /// A negated exclude rule keeps paths that match the pattern instead of
    /// excluding them. Mirrors the `!` modifier from upstream `exclude.c`.
    #[must_use]
    pub const fn with_negate(mut self, negate: bool) -> Self {
        self.negate = negate;
        self
    }

    /// Reports whether the rule inverts its match result.
    #[must_use]
    pub const fn is_negated(&self) -> bool {
        self.negate
    }

    /// Marks the rule as originating from the `-C`/`--cvs-exclude` built-in
    /// expansion.
    ///
    /// upstream: exclude.c:1652-1668 send_filter_list() - CVS rules follow a
    /// distinct wire-vs-local send path keyed on the sending role and protocol
    /// version.
    #[must_use]
    pub const fn with_cvs_origin(mut self, cvs_origin: bool) -> Self {
        self.cvs_origin = cvs_origin;
        self
    }

    /// Reports whether the rule came from the `-C`/`--cvs-exclude` expansion.
    #[must_use]
    pub const fn is_cvs_origin(&self) -> bool {
        self.cvs_origin
    }

    /// Anchors the pattern to the root of the transfer when requested.
    #[must_use]
    pub fn with_anchor(mut self) -> Self {
        if !self.pattern.starts_with('/') {
            self.pattern.insert(0, '/');
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_defaults_to_receiver_side_only() {
        let rule = FilterRuleSpec::protect("keep");
        assert!(!rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn risk_defaults_to_receiver_side_only() {
        let rule = FilterRuleSpec::risk("keep");
        assert!(!rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn dir_merge_sender_override_enables_sender_rules() {
        let mut rule = FilterRuleSpec::protect("keep");
        let options = DirMergeOptions::new().sender_modifier();
        rule.apply_dir_merge_overrides(&options);

        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    /// upstream: exclude.c:1330-1332 add_rule() applies the implicit
    /// FILTRULE_SENDER_SIDE flag when --delete-excluded is active and the
    /// rule carries neither FILTRULES_SIDES nor merge/dir-merge. A bare
    /// `--exclude *.tmp` must therefore become sender-side under
    /// --delete-excluded so the receiver can still delete matches.
    #[test]
    fn delete_excluded_applies_implicit_sender_side_to_exclude() {
        let mut rule = FilterRuleSpec::exclude("*.tmp");
        assert!(rule.applies_to_sender());
        assert!(rule.applies_to_receiver());

        let changed = rule.apply_implicit_sender_side_for_delete_excluded();
        assert!(changed);
        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    /// Include rules without an explicit side also gain the implicit
    /// FILTRULE_SENDER_SIDE flag, matching upstream `exclude.c:1330-1332`.
    #[test]
    fn delete_excluded_applies_implicit_sender_side_to_include() {
        let mut rule = FilterRuleSpec::include("keep/**");
        let changed = rule.apply_implicit_sender_side_for_delete_excluded();
        assert!(changed);
        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    /// Upstream's check at `exclude.c:1331` masks against `FILTRULES_SIDES`.
    /// A rule that already specifies one side (via `s`, `r`, `show`, `hide`,
    /// etc.) must not be retargeted.
    #[test]
    fn delete_excluded_respects_explicit_sender_only_rule() {
        let mut rule = FilterRuleSpec::hide("*.tmp"); // sender-only exclude
        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());

        let changed = rule.apply_implicit_sender_side_for_delete_excluded();
        assert!(!changed);
        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    #[test]
    fn delete_excluded_respects_explicit_receiver_only_rule() {
        let mut rule = FilterRuleSpec::exclude("*.tmp").with_sender(false);
        let changed = rule.apply_implicit_sender_side_for_delete_excluded();
        assert!(!changed);
        assert!(!rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    /// Protect/Risk and the merge rule families never trigger the implicit
    /// flag at the top-level CLI compile step - they either restrict to the
    /// receiver side already (`FilterRuleSpec::protect`/`risk` set
    /// `sender=false`) or carry their own `FILTRULE_MERGE_FILE` /
    /// `FILTRULE_PERDIR_MERGE` bits that upstream excludes from the mask in
    /// `exclude.c:1330-1332`.
    ///
    /// DirMerge wrappers themselves remain a no-op here because the wrapper
    /// directive is the "merge file" rule that upstream's mask spares; the
    /// implicit FILTRULE_SENDER_SIDE flip applies instead to each per-token
    /// rule expanded out of the merge file. That expansion happens in the
    /// engine's dir-merge loader (`apply_dir_merge_rule_defaults`) and the
    /// receiver's filter chain (`FilterChain::with_delete_excluded`), both
    /// of which thread the same `delete_excluded` flag through their parse
    /// path so the implicit flip fires once per expanded rule.
    #[test]
    fn delete_excluded_skips_non_include_exclude_kinds() {
        let mut protect = FilterRuleSpec::protect("keep");
        assert!(!protect.apply_implicit_sender_side_for_delete_excluded());

        let mut risk = FilterRuleSpec::risk("scratch");
        assert!(!risk.apply_implicit_sender_side_for_delete_excluded());

        let mut clear = FilterRuleSpec::clear();
        assert!(!clear.apply_implicit_sender_side_for_delete_excluded());

        // DirMerge wrappers are unchanged at compile_filter_program time so
        // they are forwarded to the engine unmodified. The engine's
        // load_dir_merge_rules_recursive path is what applies the implicit
        // sender-side flip to each per-token rule expanded from the merge
        // file under --delete-excluded.
        let mut dir_merge = FilterRuleSpec::dir_merge(".rsync-filter", DirMergeOptions::new());
        assert!(!dir_merge.apply_implicit_sender_side_for_delete_excluded());
        assert!(dir_merge.applies_to_sender());
        assert!(dir_merge.applies_to_receiver());
    }
}
