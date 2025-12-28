/// Rule kind enforced for entries inside a dir-merge file when modifiers
/// request include-only or exclude-only semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirMergeEnforcedKind {
    /// All entries are treated as include rules.
    Include,
    /// All entries are treated as exclude rules.
    Exclude,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DirMergeParser {
    Lines {
        enforce_kind: Option<DirMergeEnforcedKind>,
        allow_comments: bool,
    },
    Whitespace {
        enforce_kind: Option<DirMergeEnforcedKind>,
    },
}

impl DirMergeParser {
    pub(crate) const fn enforce_kind(&self) -> Option<DirMergeEnforcedKind> {
        match self {
            Self::Lines { enforce_kind, .. } | Self::Whitespace { enforce_kind } => *enforce_kind,
        }
    }

    pub(crate) const fn allows_comments(&self) -> bool {
        matches!(
            self,
            Self::Lines {
                allow_comments: true,
                ..
            }
        )
    }

    pub(crate) const fn is_whitespace(&self) -> bool {
        matches!(self, Self::Whitespace { .. })
    }
}

/// Behavioural modifiers applied to a per-directory filter merge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirMergeOptions {
    inherit: bool,
    exclude_self: bool,
    parser: DirMergeParser,
    allow_list_clear: bool,
    sender_side: SideState,
    receiver_side: SideState,
    anchor_root: bool,
    perishable: bool,
}

impl DirMergeOptions {
    /// Creates default merge options: inherited rules, line-based parsing,
    /// comment support, and permission for list-clearing directives.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inherit: true,
            exclude_self: false,
            parser: DirMergeParser::Lines {
                enforce_kind: None,
                allow_comments: true,
            },
            allow_list_clear: true,
            sender_side: SideState::Unspecified,
            receiver_side: SideState::Unspecified,
            anchor_root: false,
            perishable: false,
        }
    }

    /// Requests that the parsed rules be inherited by subdirectories.
    #[must_use]
    pub const fn inherit(mut self, inherit: bool) -> Self {
        self.inherit = inherit;
        self
    }

    /// Requests that the filter file be excluded from the transfer.
    #[must_use]
    pub const fn exclude_filter_file(mut self, exclude: bool) -> Self {
        self.exclude_self = exclude;
        self
    }

    /// Applies an enforced rule kind to entries parsed from the file.
    #[must_use]
    pub const fn with_enforced_kind(mut self, kind: Option<DirMergeEnforcedKind>) -> Self {
        self.parser = match self.parser {
            DirMergeParser::Lines { allow_comments, .. } => DirMergeParser::Lines {
                enforce_kind: kind,
                allow_comments,
            },
            DirMergeParser::Whitespace { .. } => DirMergeParser::Whitespace { enforce_kind: kind },
        };
        self
    }

    /// Switches parsing to whitespace-separated tokens instead of whole lines.
    #[must_use]
    pub const fn use_whitespace(mut self) -> Self {
        let enforce = self.parser.enforce_kind();
        self.parser = DirMergeParser::Whitespace {
            enforce_kind: enforce,
        };
        self
    }

    /// Toggles comment handling for line-based parsing.
    #[must_use]
    pub const fn allow_comments(mut self, allow: bool) -> Self {
        self.parser = match self.parser {
            DirMergeParser::Lines { enforce_kind, .. } => DirMergeParser::Lines {
                enforce_kind,
                allow_comments: allow,
            },
            other => other,
        };
        self
    }

    /// Permits list-clearing `!` directives inside the merge file.
    #[must_use]
    pub const fn allow_list_clearing(mut self, allow: bool) -> Self {
        self.allow_list_clear = allow;
        self
    }

    /// Applies the sender-side modifier to rules loaded from the filter file.
    #[must_use]
    pub const fn sender_modifier(mut self) -> Self {
        self.sender_side = SideState::Enabled;
        if matches!(self.receiver_side, SideState::Unspecified) {
            self.receiver_side = SideState::Disabled;
        }
        self
    }

    /// Applies the receiver-side modifier to rules loaded from the filter file.
    #[must_use]
    pub const fn receiver_modifier(mut self) -> Self {
        self.receiver_side = SideState::Enabled;
        if matches!(self.sender_side, SideState::Unspecified) {
            self.sender_side = SideState::Disabled;
        }
        self
    }

    /// Overrides the sender/receiver applicability flags without inferring defaults.
    #[must_use]
    pub const fn with_side_overrides(
        mut self,
        sender: Option<bool>,
        receiver: Option<bool>,
    ) -> Self {
        self.sender_side = match sender {
            Some(true) => SideState::Enabled,
            Some(false) => SideState::Disabled,
            None => SideState::Unspecified,
        };
        self.receiver_side = match receiver {
            Some(true) => SideState::Enabled,
            Some(false) => SideState::Disabled,
            None => SideState::Unspecified,
        };
        self
    }

    /// Requests that patterns within the filter file be anchored to the transfer root.
    #[must_use]
    pub const fn anchor_root(mut self, anchor: bool) -> Self {
        self.anchor_root = anchor;
        self
    }

    /// Marks rules parsed from the file as perishable.
    #[must_use]
    pub const fn mark_perishable(mut self) -> Self {
        self.perishable = true;
        self
    }

    /// Returns whether the parsed rules should be inherited.
    #[must_use]
    pub const fn inherit_rules(&self) -> bool {
        self.inherit
    }

    /// Returns whether the filter file itself should be excluded from transfer.
    #[must_use]
    pub const fn excludes_self(&self) -> bool {
        self.exclude_self
    }

    /// Returns whether list-clearing directives are permitted.
    #[must_use]
    pub const fn list_clear_allowed(&self) -> bool {
        self.allow_list_clear
    }

    /// Returns the parser configuration used when reading the file.
    #[must_use]
    pub(crate) const fn parser(&self) -> &DirMergeParser {
        &self.parser
    }

    /// Reports whether whitespace tokenisation is enabled.
    #[must_use]
    pub const fn uses_whitespace(&self) -> bool {
        self.parser.is_whitespace()
    }

    /// Reports whether comment lines are honoured when parsing.
    #[must_use]
    pub const fn allows_comments(&self) -> bool {
        self.parser.allows_comments()
    }

    /// Returns the enforced rule kind, if any.
    #[must_use]
    pub const fn enforced_kind(&self) -> Option<DirMergeEnforcedKind> {
        self.parser.enforce_kind()
    }

    /// Reports whether loaded rules should apply to the sending side.
    #[must_use]
    pub const fn applies_to_sender(&self) -> bool {
        !matches!(self.sender_side, SideState::Disabled)
    }

    /// Optional override for the sender side when explicitly requested by modifiers.
    #[must_use]
    pub const fn sender_side_override(&self) -> Option<bool> {
        match self.sender_side {
            SideState::Unspecified => None,
            SideState::Enabled => Some(true),
            SideState::Disabled => Some(false),
        }
    }

    /// Reports whether loaded rules should apply to the receiving side.
    #[must_use]
    pub const fn applies_to_receiver(&self) -> bool {
        !matches!(self.receiver_side, SideState::Disabled)
    }

    /// Optional override for the receiver side when explicitly requested by modifiers.
    #[must_use]
    pub const fn receiver_side_override(&self) -> Option<bool> {
        match self.receiver_side {
            SideState::Unspecified => None,
            SideState::Enabled => Some(true),
            SideState::Disabled => Some(false),
        }
    }

    /// Reports whether patterns should be anchored to the transfer root.
    #[must_use]
    pub const fn anchor_root_enabled(&self) -> bool {
        self.anchor_root
    }

    /// Returns whether rules should be marked as perishable.
    #[must_use]
    pub const fn perishable(&self) -> bool {
        self.perishable
    }
}

impl Default for DirMergeOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SideState {
    Unspecified,
    Enabled,
    Disabled,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== DirMergeEnforcedKind tests ====================

    #[test]
    fn dir_merge_enforced_kind_clone() {
        let kind = DirMergeEnforcedKind::Include;
        let cloned = kind;
        assert_eq!(kind, cloned);
    }

    #[test]
    fn dir_merge_enforced_kind_debug() {
        let include = DirMergeEnforcedKind::Include;
        let exclude = DirMergeEnforcedKind::Exclude;
        assert!(format!("{include:?}").contains("Include"));
        assert!(format!("{exclude:?}").contains("Exclude"));
    }

    #[test]
    fn dir_merge_enforced_kind_eq() {
        assert_eq!(DirMergeEnforcedKind::Include, DirMergeEnforcedKind::Include);
        assert_eq!(DirMergeEnforcedKind::Exclude, DirMergeEnforcedKind::Exclude);
        assert_ne!(DirMergeEnforcedKind::Include, DirMergeEnforcedKind::Exclude);
    }

    // ==================== DirMergeParser tests ====================

    #[test]
    fn dir_merge_parser_lines_enforce_kind_none() {
        let parser = DirMergeParser::Lines {
            enforce_kind: None,
            allow_comments: true,
        };
        assert_eq!(parser.enforce_kind(), None);
    }

    #[test]
    fn dir_merge_parser_lines_enforce_kind_include() {
        let parser = DirMergeParser::Lines {
            enforce_kind: Some(DirMergeEnforcedKind::Include),
            allow_comments: true,
        };
        assert_eq!(parser.enforce_kind(), Some(DirMergeEnforcedKind::Include));
    }

    #[test]
    fn dir_merge_parser_whitespace_enforce_kind() {
        let parser = DirMergeParser::Whitespace {
            enforce_kind: Some(DirMergeEnforcedKind::Exclude),
        };
        assert_eq!(parser.enforce_kind(), Some(DirMergeEnforcedKind::Exclude));
    }

    #[test]
    fn dir_merge_parser_lines_allows_comments() {
        let parser = DirMergeParser::Lines {
            enforce_kind: None,
            allow_comments: true,
        };
        assert!(parser.allows_comments());
    }

    #[test]
    fn dir_merge_parser_lines_disallows_comments() {
        let parser = DirMergeParser::Lines {
            enforce_kind: None,
            allow_comments: false,
        };
        assert!(!parser.allows_comments());
    }

    #[test]
    fn dir_merge_parser_whitespace_no_comments() {
        let parser = DirMergeParser::Whitespace { enforce_kind: None };
        // Whitespace parser never allows comments
        assert!(!parser.allows_comments());
    }

    #[test]
    fn dir_merge_parser_is_whitespace() {
        let lines = DirMergeParser::Lines {
            enforce_kind: None,
            allow_comments: true,
        };
        let whitespace = DirMergeParser::Whitespace { enforce_kind: None };

        assert!(!lines.is_whitespace());
        assert!(whitespace.is_whitespace());
    }

    // ==================== DirMergeOptions construction tests ====================

    #[test]
    fn dir_merge_options_new_default_values() {
        let opts = DirMergeOptions::new();
        assert!(opts.inherit_rules());
        assert!(!opts.excludes_self());
        assert!(opts.list_clear_allowed());
        assert!(opts.allows_comments());
        assert!(!opts.uses_whitespace());
        assert_eq!(opts.enforced_kind(), None);
        assert!(opts.applies_to_sender());
        assert!(opts.applies_to_receiver());
        assert!(!opts.anchor_root_enabled());
        assert!(!opts.perishable());
    }

    #[test]
    fn dir_merge_options_default_matches_new() {
        let new_opts = DirMergeOptions::new();
        let default_opts = DirMergeOptions::default();
        assert_eq!(new_opts, default_opts);
    }

    // ==================== DirMergeOptions builder tests ====================

    #[test]
    fn dir_merge_options_inherit_false() {
        let opts = DirMergeOptions::new().inherit(false);
        assert!(!opts.inherit_rules());
    }

    #[test]
    fn dir_merge_options_inherit_true() {
        let opts = DirMergeOptions::new().inherit(false).inherit(true);
        assert!(opts.inherit_rules());
    }

    #[test]
    fn dir_merge_options_exclude_filter_file_true() {
        let opts = DirMergeOptions::new().exclude_filter_file(true);
        assert!(opts.excludes_self());
    }

    #[test]
    fn dir_merge_options_exclude_filter_file_false() {
        let opts = DirMergeOptions::new()
            .exclude_filter_file(true)
            .exclude_filter_file(false);
        assert!(!opts.excludes_self());
    }

    #[test]
    fn dir_merge_options_with_enforced_kind_include() {
        let opts = DirMergeOptions::new().with_enforced_kind(Some(DirMergeEnforcedKind::Include));
        assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    }

    #[test]
    fn dir_merge_options_with_enforced_kind_exclude() {
        let opts = DirMergeOptions::new().with_enforced_kind(Some(DirMergeEnforcedKind::Exclude));
        assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
    }

    #[test]
    fn dir_merge_options_with_enforced_kind_none() {
        let opts = DirMergeOptions::new()
            .with_enforced_kind(Some(DirMergeEnforcedKind::Include))
            .with_enforced_kind(None);
        assert_eq!(opts.enforced_kind(), None);
    }

    #[test]
    fn dir_merge_options_use_whitespace() {
        let opts = DirMergeOptions::new().use_whitespace();
        assert!(opts.uses_whitespace());
        // Comments should be disabled with whitespace parser
        assert!(!opts.allows_comments());
    }

    #[test]
    fn dir_merge_options_use_whitespace_preserves_enforced_kind() {
        let opts = DirMergeOptions::new()
            .with_enforced_kind(Some(DirMergeEnforcedKind::Include))
            .use_whitespace();
        assert!(opts.uses_whitespace());
        assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    }

    #[test]
    fn dir_merge_options_allow_comments_true() {
        let opts = DirMergeOptions::new()
            .allow_comments(false)
            .allow_comments(true);
        assert!(opts.allows_comments());
    }

    #[test]
    fn dir_merge_options_allow_comments_false() {
        let opts = DirMergeOptions::new().allow_comments(false);
        assert!(!opts.allows_comments());
    }

    #[test]
    fn dir_merge_options_allow_comments_ignored_for_whitespace() {
        let opts = DirMergeOptions::new().use_whitespace().allow_comments(true);
        // allow_comments should have no effect on whitespace parser
        assert!(!opts.allows_comments());
    }

    #[test]
    fn dir_merge_options_allow_list_clearing_false() {
        let opts = DirMergeOptions::new().allow_list_clearing(false);
        assert!(!opts.list_clear_allowed());
    }

    #[test]
    fn dir_merge_options_allow_list_clearing_true() {
        let opts = DirMergeOptions::new()
            .allow_list_clearing(false)
            .allow_list_clearing(true);
        assert!(opts.list_clear_allowed());
    }

    // ==================== sender/receiver modifier tests ====================

    #[test]
    fn dir_merge_options_sender_modifier() {
        let opts = DirMergeOptions::new().sender_modifier();
        assert!(opts.applies_to_sender());
        assert!(!opts.applies_to_receiver());
        assert_eq!(opts.sender_side_override(), Some(true));
        assert_eq!(opts.receiver_side_override(), Some(false));
    }

    #[test]
    fn dir_merge_options_receiver_modifier() {
        let opts = DirMergeOptions::new().receiver_modifier();
        assert!(!opts.applies_to_sender());
        assert!(opts.applies_to_receiver());
        assert_eq!(opts.sender_side_override(), Some(false));
        assert_eq!(opts.receiver_side_override(), Some(true));
    }

    #[test]
    fn dir_merge_options_both_modifiers() {
        let opts = DirMergeOptions::new().sender_modifier().receiver_modifier();
        // receiver_modifier doesn't change sender since it's already Enabled
        assert!(opts.applies_to_sender());
        assert!(opts.applies_to_receiver());
    }

    #[test]
    fn dir_merge_options_with_side_overrides_both_enabled() {
        let opts = DirMergeOptions::new().with_side_overrides(Some(true), Some(true));
        assert!(opts.applies_to_sender());
        assert!(opts.applies_to_receiver());
        assert_eq!(opts.sender_side_override(), Some(true));
        assert_eq!(opts.receiver_side_override(), Some(true));
    }

    #[test]
    fn dir_merge_options_with_side_overrides_both_disabled() {
        let opts = DirMergeOptions::new().with_side_overrides(Some(false), Some(false));
        assert!(!opts.applies_to_sender());
        assert!(!opts.applies_to_receiver());
        assert_eq!(opts.sender_side_override(), Some(false));
        assert_eq!(opts.receiver_side_override(), Some(false));
    }

    #[test]
    fn dir_merge_options_with_side_overrides_unspecified() {
        let opts = DirMergeOptions::new().with_side_overrides(None, None);
        assert!(opts.applies_to_sender());
        assert!(opts.applies_to_receiver());
        assert_eq!(opts.sender_side_override(), None);
        assert_eq!(opts.receiver_side_override(), None);
    }

    #[test]
    fn dir_merge_options_with_side_overrides_mixed() {
        let opts = DirMergeOptions::new().with_side_overrides(Some(true), Some(false));
        assert!(opts.applies_to_sender());
        assert!(!opts.applies_to_receiver());
    }

    // ==================== anchor_root and perishable tests ====================

    #[test]
    fn dir_merge_options_anchor_root_true() {
        let opts = DirMergeOptions::new().anchor_root(true);
        assert!(opts.anchor_root_enabled());
    }

    #[test]
    fn dir_merge_options_anchor_root_false() {
        let opts = DirMergeOptions::new().anchor_root(true).anchor_root(false);
        assert!(!opts.anchor_root_enabled());
    }

    #[test]
    fn dir_merge_options_mark_perishable() {
        let opts = DirMergeOptions::new().mark_perishable();
        assert!(opts.perishable());
    }

    // ==================== clone and equality tests ====================

    #[test]
    fn dir_merge_options_clone() {
        let opts = DirMergeOptions::new()
            .inherit(false)
            .exclude_filter_file(true)
            .sender_modifier();
        let cloned = opts.clone();
        assert_eq!(opts, cloned);
    }

    #[test]
    fn dir_merge_options_debug() {
        let opts = DirMergeOptions::new();
        let debug_str = format!("{opts:?}");
        assert!(debug_str.contains("DirMergeOptions"));
    }

    // ==================== parser accessor tests ====================

    #[test]
    fn dir_merge_options_parser_returns_reference() {
        let opts = DirMergeOptions::new();
        let parser = opts.parser();
        assert!(!parser.is_whitespace());
        assert!(parser.allows_comments());
    }

    #[test]
    fn dir_merge_options_parser_whitespace() {
        let opts = DirMergeOptions::new().use_whitespace();
        let parser = opts.parser();
        assert!(parser.is_whitespace());
    }

    // ==================== integration tests ====================

    #[test]
    fn dir_merge_options_complex_configuration() {
        let opts = DirMergeOptions::new()
            .inherit(false)
            .exclude_filter_file(true)
            .with_enforced_kind(Some(DirMergeEnforcedKind::Exclude))
            .allow_list_clearing(false)
            .sender_modifier()
            .anchor_root(true)
            .mark_perishable();

        assert!(!opts.inherit_rules());
        assert!(opts.excludes_self());
        assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
        assert!(!opts.list_clear_allowed());
        assert!(opts.applies_to_sender());
        assert!(!opts.applies_to_receiver());
        assert!(opts.anchor_root_enabled());
        assert!(opts.perishable());
    }

    #[test]
    fn dir_merge_options_whitespace_enforced_kind_roundtrip() {
        let opts = DirMergeOptions::new()
            .use_whitespace()
            .with_enforced_kind(Some(DirMergeEnforcedKind::Include));

        assert!(opts.uses_whitespace());
        assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    }
}
