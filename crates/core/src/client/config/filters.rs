use rsync_engine::local_copy::DirMergeOptions;

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
        }
    }

    /// Creates a rule that clears previously defined filter rules.
    #[must_use]
    pub fn clear() -> Self {
        Self {
            kind: FilterRuleKind::Clear,
            pattern: String::new(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates a protection rule for deletion passes.
    #[must_use]
    pub fn protect(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Protect,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates a deletion rule that removes previously protected entries.
    #[must_use]
    pub fn risk(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Risk,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
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
        }
    }

    /// Applies per-directory merge options to the rule.
    pub fn with_dir_merge_options(mut self, options: DirMergeOptions) -> Self {
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

    /// Returns the per-directory merge options when present.
    #[must_use]
    pub fn dir_merge_options(&self) -> Option<&DirMergeOptions> {
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
}
