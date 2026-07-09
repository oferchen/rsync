//! User-visible filter rule type and builder methods.
//!
//! This module defines [`FilterRule`], the primary input type for constructing
//! a [`FilterSet`](crate::FilterSet). Rules are created via named constructors
//! (e.g., [`FilterRule::include`], [`FilterRule::exclude`]) and configured
//! with builder-style modifier methods.
//!
//! upstream: exclude.c - filter_rule struct and FILTRULE_* flags

use crate::FilterAction;

/// User-visible filter rule consisting of an action and a glob pattern.
///
/// Filter rules control which files are included or excluded during rsync
/// transfers. Each rule pairs a [`FilterAction`] with a pattern string and
/// optional modifier flags.
///
/// # Construction
///
/// Use the named constructors to create rules for each action:
///
/// ```
/// use filters::FilterRule;
///
/// let inc  = FilterRule::include("*.rs");
/// let exc  = FilterRule::exclude("target/");
/// let prot = FilterRule::protect("/data");
/// ```
///
/// Modifier methods use a builder pattern and can be chained:
///
/// ```
/// use filters::FilterRule;
///
/// let rule = FilterRule::exclude("*.tmp")
///     .with_perishable(true)
///     .with_sender(false);
/// ```
///
/// # Pattern syntax
///
/// Patterns follow rsync's glob rules:
///
/// - `*` matches any characters except `/`.
/// - `**` matches any characters including `/` (recursive wildcard).
/// - `?` matches a single character except `/`.
/// - A leading `/` anchors the pattern to the transfer root.
/// - A trailing `/` restricts the rule to directories only.
/// - Without a leading `/` and without internal `/` separators, the pattern
///   matches at any depth (an implicit `**/` prefix is added).
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
    /// `e` modifier: forces rule to act as exclude regardless of action.
    pub(crate) exclude_only: bool,
    /// `n` modifier: on merge rules, do not inherit parent-directory rules.
    pub(crate) no_inherit: bool,
    /// `C` modifier on merge / dir-merge rules: parse the merged file as
    /// CVS-style ignores (whitespace-split, no comments, no prefixes,
    /// no inheritance).
    ///
    /// upstream: exclude.c add_rule() ':C' modifier — CVS-ignore semantics
    /// (sets `FILTRULE_NO_PREFIXES | FILTRULE_WORD_SPLIT | FILTRULE_NO_INHERIT
    /// | FILTRULE_CVS_IGNORE`).
    pub(crate) cvs_mode: bool,
    /// `/` modifier on a merge / dir-merge rule: FILTRULE_ABS_PATH. Anchors the
    /// merged rules to the transfer root rather than the merge file's directory.
    ///
    /// upstream: exclude.c:1215-1216 - `case '/': rule->rflags |= FILTRULE_ABS_PATH;`
    pub(crate) abs_path: bool,
    /// `w` modifier on a merge / dir-merge rule: FILTRULE_WORD_SPLIT. The
    /// referenced file is tokenised on any whitespace (space, tab, newline)
    /// with each token parsed as its own rule, rather than one rule per line.
    ///
    /// upstream: exclude.c:1279-1283 - `case 'w': rule->rflags |= FILTRULE_WORD_SPLIT;`
    pub(crate) word_split: bool,
    /// `-`/`+` modifier on a merge / dir-merge rule: FILTRULE_NO_PREFIXES.
    /// Consumes the merged file's lines as literal patterns instead of running
    /// them through the prefix dispatch.
    ///
    /// upstream: exclude.c:1197-1213 - `case '-'`/`case '+'`.
    pub(crate) no_prefixes: bool,
    /// Pairs with [`Self::no_prefixes`] to select the `+` (include) variant.
    /// When both are set, literal lines become include rules; the `-` variant
    /// leaves this false and lines become exclude rules.
    ///
    /// upstream: exclude.c:1210-1213 - `+` also sets FILTRULE_INCLUDE.
    pub(crate) no_prefixes_include: bool,
}

impl FilterRule {
    /// Creates an include rule for `pattern`.
    ///
    /// The returned rule applies to both the sender and receiver sides by
    /// default. Use [`with_sender`](Self::with_sender) /
    /// [`with_receiver`](Self::with_receiver) to restrict it.
    ///
    /// # Examples
    ///
    /// ```
    /// use filters::{FilterRule, FilterAction};
    ///
    /// let rule = FilterRule::include("*.rs");
    /// assert_eq!(rule.action(), FilterAction::Include);
    /// assert_eq!(rule.pattern(), "*.rs");
    /// ```
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
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
        }
    }

    /// Creates an exclude rule for `pattern`.
    ///
    /// The returned rule applies to both the sender and receiver sides by
    /// default. Excluded directories also exclude their descendants
    /// automatically.
    ///
    /// # Examples
    ///
    /// ```
    /// use filters::{FilterRule, FilterAction};
    ///
    /// let rule = FilterRule::exclude("*.bak");
    /// assert_eq!(rule.action(), FilterAction::Exclude);
    /// assert_eq!(rule.pattern(), "*.bak");
    /// ```
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
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
        }
    }

    /// Creates a protect rule for `pattern`.
    ///
    /// Protect rules only apply on the receiver side and prevent `--delete`
    /// from removing matching destination paths.
    ///
    /// # Examples
    ///
    /// ```
    /// use filters::{FilterRule, FilterAction};
    ///
    /// let rule = FilterRule::protect("/data");
    /// assert_eq!(rule.action(), FilterAction::Protect);
    /// assert!(!rule.applies_to_sender());
    /// assert!(rule.applies_to_receiver());
    /// ```
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
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
        }
    }

    /// Creates a risk rule for `pattern`.
    ///
    /// Risk rules cancel an earlier [`Protect`](FilterAction::Protect) for the
    /// same path, re-allowing deletion. Like protect, risk only applies on the
    /// receiver side.
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
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
        }
    }

    /// Clears all previously configured rules for the applicable transfer sides.
    ///
    /// Equivalent to rsync's `!` token. When compiled into a [`FilterSet`](crate::FilterSet),
    /// a clear rule removes every prior include/exclude and protect/risk rule
    /// that applies to the same side(s).
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
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
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
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
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
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
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
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
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
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
        }
    }

    /// Include or exclude action applied when this rule matches.
    #[must_use]
    pub const fn action(&self) -> FilterAction {
        self.action
    }

    /// Glob or literal pattern text for matching.
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

    /// Returns whether the rule is exclude-only (`e` modifier).
    ///
    /// When true, the rule always acts as an exclude regardless of action.
    #[must_use]
    pub const fn is_exclude_only(&self) -> bool {
        self.exclude_only
    }

    /// Marks the rule as exclude-only (`e` modifier).
    ///
    /// Forces the rule to act as an exclude even if the action is Include.
    #[must_use]
    pub const fn with_exclude_only(mut self, exclude_only: bool) -> Self {
        self.exclude_only = exclude_only;
        self
    }

    /// Returns whether the rule has no-inherit set (`n` modifier).
    ///
    /// When true on merge rules, child rules don't inherit parent rules.
    #[must_use]
    pub const fn is_no_inherit(&self) -> bool {
        self.no_inherit
    }

    /// Marks the rule with no-inherit (`n` modifier).
    ///
    /// For merge rules, this prevents child rules from inheriting parent rules.
    #[must_use]
    pub const fn with_no_inherit(mut self, no_inherit: bool) -> Self {
        self.no_inherit = no_inherit;
        self
    }

    /// Returns whether the rule carries the `C` (CVS-ignore) modifier.
    ///
    /// On merge / dir-merge rules, this signals that the referenced file
    /// must be parsed as CVS-style ignores (whitespace-split, no prefixes,
    /// no inheritance).
    ///
    /// upstream: exclude.c add_rule() ':C' modifier — CVS-ignore semantics
    #[must_use]
    pub const fn is_cvs_mode(&self) -> bool {
        self.cvs_mode
    }

    /// Sets the `C` (CVS-ignore) modifier on this rule.
    ///
    /// upstream: exclude.c add_rule() ':C' modifier — CVS-ignore semantics
    #[must_use]
    pub const fn with_cvs_mode(mut self, cvs_mode: bool) -> Self {
        self.cvs_mode = cvs_mode;
        self
    }

    /// Returns whether the rule carries the `w` (word-split) modifier.
    ///
    /// On merge / dir-merge rules, this signals that the referenced file must
    /// be tokenised on any whitespace, with each token parsed as its own rule.
    ///
    /// upstream: exclude.c:1279-1283 - `w` sets FILTRULE_WORD_SPLIT.
    #[must_use]
    pub const fn is_word_split(&self) -> bool {
        self.word_split
    }

    /// Sets the `w` (word-split) modifier on this rule.
    ///
    /// upstream: exclude.c:1279-1283 - `w` sets FILTRULE_WORD_SPLIT.
    #[must_use]
    pub const fn with_word_split(mut self, word_split: bool) -> Self {
        self.word_split = word_split;
        self
    }

    /// Returns whether the rule carries the `/` (FILTRULE_ABS_PATH) modifier.
    ///
    /// On merge / dir-merge rules this anchors the merged rules to the transfer
    /// root instead of the merge file's own directory.
    ///
    /// upstream: exclude.c:1215-1216 - `case '/'`
    #[must_use]
    pub const fn is_abs_path(&self) -> bool {
        self.abs_path
    }

    /// Sets the `/` (FILTRULE_ABS_PATH) modifier on this rule.
    #[must_use]
    pub const fn with_abs_path(mut self, abs_path: bool) -> Self {
        self.abs_path = abs_path;
        self
    }

    /// Returns whether the rule carries the `-`/`+` (FILTRULE_NO_PREFIXES)
    /// modifier and, via the second element, whether it is the `+` (include)
    /// variant.
    ///
    /// upstream: exclude.c:1197-1213 - `case '-'`/`case '+'`
    #[must_use]
    pub const fn no_prefixes(&self) -> (bool, bool) {
        (self.no_prefixes, self.no_prefixes_include)
    }

    /// Sets the `-`/`+` (FILTRULE_NO_PREFIXES) modifier on this rule. `include`
    /// selects the `+` variant (literal includes); otherwise literal excludes.
    #[must_use]
    pub const fn with_no_prefixes(mut self, no_prefixes: bool, include: bool) -> Self {
        self.no_prefixes = no_prefixes;
        self.no_prefixes_include = include;
        self
    }

    /// Anchors the pattern to the root of the transfer if it is not already.
    ///
    /// Prepends `/` to the pattern when it does not already start with one.
    /// An anchored pattern only matches at the top level of the transfer tree
    /// rather than at any depth.
    ///
    /// This method is idempotent: calling it on an already-anchored rule is a
    /// no-op.
    #[must_use]
    pub fn anchor_to_root(mut self) -> Self {
        if !self.pattern.starts_with('/') {
            self.pattern.insert(0, '/');
        }
        self
    }

    /// Returns the rule with its pattern text replaced, preserving every other
    /// attribute (action, side flags, perishability, negation, modifiers).
    ///
    /// Used to re-anchor a per-directory merge rule against the merge file's
    /// directory without rebuilding the rule from scratch.
    #[must_use]
    pub fn with_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.pattern = pattern.into();
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
