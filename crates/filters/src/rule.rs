use crate::FilterAction;

/// User-visible filter rule consisting of an action and pattern.
///
/// Filter rules control which files are included or excluded during rsync transfers.
/// Each rule specifies an action (include, exclude, protect, risk, clear) and a
/// glob pattern to match against file paths.
///
/// # Negation
///
/// When `negate` is true, the rule's match result is inverted. A negated exclude
/// rule excludes files that do NOT match the pattern, matching upstream rsync's
/// `!` modifier behavior (see `exclude.c` line 906: `ret_match = negate ? 0 : 1`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterRule {
    pub(crate) action: FilterAction,
    pub(crate) pattern: String,
    pub(crate) applies_to_sender: bool,
    pub(crate) applies_to_receiver: bool,
    pub(crate) perishable: bool,
    pub(crate) xattr_only: bool,
    pub(crate) negate: bool,
}

impl FilterRule {
    /// Creates an include rule for `pattern`.
    #[must_use]
    pub fn include(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Include,
            pattern: pattern.into(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
        }
    }

    /// Creates an exclude rule for `pattern`.
    #[must_use]
    pub fn exclude(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Exclude,
            pattern: pattern.into(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
        }
    }

    /// Creates a protect rule for `pattern`.
    #[must_use]
    pub fn protect(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Protect,
            pattern: pattern.into(),
            applies_to_sender: false,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
        }
    }

    /// Creates a risk rule for `pattern`.
    #[must_use]
    pub fn risk(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Risk,
            pattern: pattern.into(),
            applies_to_sender: false,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
        }
    }

    /// Clears all previously configured rules for the applicable transfer sides.
    #[must_use]
    #[doc(alias = "!")]
    pub const fn clear() -> Self {
        Self {
            action: FilterAction::Clear,
            pattern: String::new(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
        }
    }

    /// Creates a sender-only include rule equivalent to `show PATTERN`.
    ///
    /// # Examples
    /// ```
    /// use filters::FilterRule;
    /// let rule = FilterRule::show("logs/**");
    /// assert!(rule.applies_to_sender());
    /// assert!(!rule.applies_to_receiver());
    /// ```
    #[must_use]
    pub fn show(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Include,
            pattern: pattern.into(),
            applies_to_sender: true,
            applies_to_receiver: false,
            perishable: false,
            xattr_only: false,
            negate: false,
        }
    }

    /// Creates a sender-only exclude rule equivalent to `hide PATTERN`.
    ///
    /// # Examples
    /// ```
    /// use filters::FilterRule;
    /// let rule = FilterRule::hide("*.bak");
    /// assert!(rule.applies_to_sender());
    /// assert!(!rule.applies_to_receiver());
    /// ```
    #[must_use]
    pub fn hide(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Exclude,
            pattern: pattern.into(),
            applies_to_sender: true,
            applies_to_receiver: false,
            perishable: false,
            xattr_only: false,
            negate: false,
        }
    }

    /// Creates a merge rule that reads additional filter rules from a file.
    ///
    /// The pattern field contains the file path to read. Rules are read once
    /// when the filter set is compiled. This corresponds to rsync's `.` prefix
    /// in filter rules (e.g., `. /path/to/rules`).
    ///
    /// # Examples
    /// ```
    /// use filters::{FilterRule, FilterAction};
    /// let rule = FilterRule::merge("/etc/rsync/global.rules");
    /// assert_eq!(rule.action(), FilterAction::Merge);
    /// assert_eq!(rule.pattern(), "/etc/rsync/global.rules");
    /// ```
    #[must_use]
    pub fn merge(file_path: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Merge,
            pattern: file_path.into(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
        }
    }

    /// Creates a dir-merge rule that reads filter rules per-directory during traversal.
    ///
    /// The pattern field contains the filename to look for in each directory
    /// (e.g., `.rsync-filter`). Rules from the file are applied relative to
    /// that directory. This corresponds to rsync's `,` prefix in filter rules.
    ///
    /// # Examples
    /// ```
    /// use filters::{FilterRule, FilterAction};
    /// let rule = FilterRule::dir_merge(".rsync-filter");
    /// assert_eq!(rule.action(), FilterAction::DirMerge);
    /// assert_eq!(rule.pattern(), ".rsync-filter");
    /// ```
    #[must_use]
    pub fn dir_merge(filename: impl Into<String>) -> Self {
        Self {
            action: FilterAction::DirMerge,
            pattern: filename.into(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
        }
    }

    /// Returns the rule action.
    #[must_use]
    pub const fn action(&self) -> FilterAction {
        self.action
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

    /// Returns whether the rule affects the sending side.
    #[must_use]
    pub const fn applies_to_sender(&self) -> bool {
        self.applies_to_sender
    }

    /// Returns whether the rule affects the receiving side.
    #[must_use]
    pub const fn applies_to_receiver(&self) -> bool {
        self.applies_to_receiver
    }

    /// Sets whether the rule applies on the sending side.
    #[must_use]
    pub const fn with_sender(mut self, applies: bool) -> Self {
        self.applies_to_sender = applies;
        self
    }

    /// Sets whether the rule applies on the receiving side.
    #[must_use]
    pub const fn with_receiver(mut self, applies: bool) -> Self {
        self.applies_to_receiver = applies;
        self
    }

    /// Updates both side flags at once.
    #[must_use]
    pub const fn with_sides(mut self, sender: bool, receiver: bool) -> Self {
        self.applies_to_sender = sender;
        self.applies_to_receiver = receiver;
        self
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

    /// Returns whether the rule applies exclusively to xattr names.
    #[must_use]
    pub const fn is_xattr_only(&self) -> bool {
        self.xattr_only
    }

    /// Returns whether the rule's match result should be inverted.
    ///
    /// When true, the rule matches files that do NOT match the pattern.
    /// This mirrors upstream rsync's `!` modifier behavior.
    #[must_use]
    pub const fn is_negated(&self) -> bool {
        self.negate
    }

    /// Marks the rule as negated, inverting match behavior.
    ///
    /// A negated rule matches files that do NOT match the pattern.
    /// This mirrors upstream rsync's `!` modifier (e.g., `- ! *.txt`
    /// excludes all files except those matching `*.txt`).
    #[must_use]
    pub const fn with_negate(mut self, negate: bool) -> Self {
        self.negate = negate;
        self
    }

    /// Anchors the pattern to the root of the transfer if it is not already.
    #[must_use]
    pub fn anchor_to_root(mut self) -> Self {
        if !self.pattern.starts_with('/') {
            self.pattern.insert(0, '/');
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod filter_rule_tests {
        use super::*;

        #[test]
        fn include_rule() {
            let rule = FilterRule::include("*.txt");
            assert_eq!(rule.action(), FilterAction::Include);
            assert_eq!(rule.pattern(), "*.txt");
            assert!(rule.applies_to_sender());
            assert!(rule.applies_to_receiver());
            assert!(!rule.is_perishable());
            assert!(!rule.is_xattr_only());
        }

        #[test]
        fn exclude_rule() {
            let rule = FilterRule::exclude("*.bak");
            assert_eq!(rule.action(), FilterAction::Exclude);
            assert_eq!(rule.pattern(), "*.bak");
            assert!(rule.applies_to_sender());
            assert!(rule.applies_to_receiver());
        }

        #[test]
        fn protect_rule() {
            let rule = FilterRule::protect("/important");
            assert_eq!(rule.action(), FilterAction::Protect);
            assert!(!rule.applies_to_sender());
            assert!(rule.applies_to_receiver());
        }

        #[test]
        fn risk_rule() {
            let rule = FilterRule::risk("/temp");
            assert_eq!(rule.action(), FilterAction::Risk);
            assert!(!rule.applies_to_sender());
            assert!(rule.applies_to_receiver());
        }

        #[test]
        fn clear_rule() {
            let rule = FilterRule::clear();
            assert_eq!(rule.action(), FilterAction::Clear);
            assert!(rule.pattern().is_empty());
            assert!(rule.applies_to_sender());
            assert!(rule.applies_to_receiver());
        }

        #[test]
        fn show_rule() {
            let rule = FilterRule::show("logs/**");
            assert_eq!(rule.action(), FilterAction::Include);
            assert!(rule.applies_to_sender());
            assert!(!rule.applies_to_receiver());
        }

        #[test]
        fn hide_rule() {
            let rule = FilterRule::hide("*.bak");
            assert_eq!(rule.action(), FilterAction::Exclude);
            assert!(rule.applies_to_sender());
            assert!(!rule.applies_to_receiver());
        }

        #[test]
        fn merge_rule() {
            let rule = FilterRule::merge("/etc/rsync/global.rules");
            assert_eq!(rule.action(), FilterAction::Merge);
            assert_eq!(rule.pattern(), "/etc/rsync/global.rules");
            assert!(rule.applies_to_sender());
            assert!(rule.applies_to_receiver());
            assert!(!rule.is_perishable());
            assert!(!rule.is_xattr_only());
        }

        #[test]
        fn dir_merge_rule() {
            let rule = FilterRule::dir_merge(".rsync-filter");
            assert_eq!(rule.action(), FilterAction::DirMerge);
            assert_eq!(rule.pattern(), ".rsync-filter");
            assert!(rule.applies_to_sender());
            assert!(rule.applies_to_receiver());
            assert!(!rule.is_perishable());
            assert!(!rule.is_xattr_only());
        }

        #[test]
        fn with_sender() {
            let rule = FilterRule::include("*").with_sender(false);
            assert!(!rule.applies_to_sender());
        }

        #[test]
        fn with_receiver() {
            let rule = FilterRule::include("*").with_receiver(false);
            assert!(!rule.applies_to_receiver());
        }

        #[test]
        fn with_sides() {
            let rule = FilterRule::include("*").with_sides(true, false);
            assert!(rule.applies_to_sender());
            assert!(!rule.applies_to_receiver());
        }

        #[test]
        fn with_perishable() {
            let rule = FilterRule::include("*").with_perishable(true);
            assert!(rule.is_perishable());
        }

        #[test]
        fn with_xattr_only() {
            let rule = FilterRule::include("*").with_xattr_only(true);
            assert!(rule.is_xattr_only());
        }

        #[test]
        fn with_negate() {
            let rule = FilterRule::exclude("*.txt").with_negate(true);
            assert!(rule.is_negated());

            let rule2 = FilterRule::exclude("*.txt").with_negate(false);
            assert!(!rule2.is_negated());
        }

        #[test]
        fn negate_default_false() {
            // All constructors should have negate=false by default
            assert!(!FilterRule::include("*").is_negated());
            assert!(!FilterRule::exclude("*").is_negated());
            assert!(!FilterRule::protect("*").is_negated());
            assert!(!FilterRule::risk("*").is_negated());
            assert!(!FilterRule::clear().is_negated());
            assert!(!FilterRule::show("*").is_negated());
            assert!(!FilterRule::hide("*").is_negated());
            assert!(!FilterRule::merge("file").is_negated());
            assert!(!FilterRule::dir_merge("file").is_negated());
        }

        #[test]
        fn negate_included_in_equality() {
            let rule1 = FilterRule::exclude("*.txt");
            let rule2 = FilterRule::exclude("*.txt").with_negate(true);
            // Rules should not be equal since negate differs
            assert_ne!(rule1, rule2);
        }

        #[test]
        fn negate_included_in_debug() {
            let rule = FilterRule::exclude("*.txt").with_negate(true);
            let debug = format!("{rule:?}");
            assert!(debug.contains("negate"));
        }

        #[test]
        fn anchor_to_root_adds_slash() {
            let rule = FilterRule::include("test").anchor_to_root();
            assert_eq!(rule.pattern(), "/test");
        }

        #[test]
        fn anchor_to_root_idempotent() {
            let rule = FilterRule::include("/test").anchor_to_root();
            assert_eq!(rule.pattern(), "/test");
        }

        #[test]
        fn clone_and_eq() {
            let rule = FilterRule::include("test");
            let cloned = rule.clone();
            assert_eq!(rule, cloned);
        }

        #[test]
        fn debug_format() {
            let rule = FilterRule::include("test");
            let debug = format!("{rule:?}");
            assert!(debug.contains("FilterRule"));
            assert!(debug.contains("Include"));
            assert!(debug.contains("test"));
        }

        #[test]
        fn pattern_accepts_string() {
            let pattern = String::from("dynamic");
            let rule = FilterRule::include(pattern);
            assert_eq!(rule.pattern(), "dynamic");
        }
    }
}
