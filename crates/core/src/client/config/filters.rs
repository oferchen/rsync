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
    #[must_use]
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
}
