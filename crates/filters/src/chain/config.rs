//! Per-directory merge file configuration and modifier application.
//!
//! A [`DirMergeConfig`] captures the filename to search for in each directory
//! and the behavioral modifiers (inherit, exclude-self, sender/receiver-only,
//! anchor-root, perishable) that control how parsed rules are processed.
//!
//! # Upstream Reference
//!
//! Mirrors upstream rsync's dir-merge filter entry (`:` prefix or `dir-merge`
//! keyword) defined in `exclude.c`.

use crate::FilterRule;

/// Configuration for a per-directory merge file.
///
/// Specifies the filename to search for in each directory and behavioral
/// modifiers that control how rules from the file are processed. This
/// corresponds to upstream rsync's dir-merge filter entry (`:` prefix or
/// `dir-merge` keyword).
///
/// # Examples
///
/// ```
/// use filters::DirMergeConfig;
///
/// // Default: read `.rsync-filter`, inherit rules to subdirectories
/// let config = DirMergeConfig::new(".rsync-filter");
///
/// // No-inherit: rules apply only in the directory where the file is found
/// let config = DirMergeConfig::new(".rsync-filter").with_inherit(false);
///
/// // Exclude the filter file itself from transfer
/// let config = DirMergeConfig::new(".rsync-filter").with_exclude_self(true);
/// ```
#[derive(Clone, Debug)]
pub struct DirMergeConfig {
    filename: String,
    /// upstream: exclude.c - FILTRULE_NO_INHERIT flag
    inherit: bool,
    /// upstream: exclude.c - `e` modifier on dir-merge rules
    exclude_self: bool,
    sender_only: bool,
    receiver_only: bool,
    anchor_root: bool,
    perishable: bool,
    /// upstream: exclude.c - `C` modifier on dir-merge rules (FILTRULE_CVS_IGNORE).
    /// Treats the merge file as a CVS-style ignore list: each whitespace
    /// separated token is an exclude rule with no filter prefixes.
    cvs_mode: bool,
    /// upstream: exclude.c:1116-1133 - FILTRULE_NO_PREFIXES (`-`/`+` modifier
    /// on a dir-merge rule). When set, each line in the per-dir merge file is
    /// consumed as a literal pattern: the short-prefix dispatch (`+`, `-`,
    /// `:`, `.`, modifiers) is skipped entirely.
    no_prefixes: bool,
    /// Pairs with [`Self::no_prefixes`] to select the `+` variant.
    ///
    /// When `no_prefixes && no_prefixes_include`, each literal line becomes
    /// an include rule; otherwise it becomes an exclude rule.
    no_prefixes_include: bool,
    /// upstream: exclude.c:1279-1283 - FILTRULE_WORD_SPLIT (`w` modifier on a
    /// dir-merge rule). When set, the per-directory merge file is tokenised on
    /// any whitespace with each token parsed as its own rule, rather than one
    /// rule per line.
    word_split: bool,
    /// Number of order-bearing global rules that preceded this `dir-merge`
    /// directive in the source rule stream.
    ///
    /// upstream: exclude.c:1046-1050 - `check_filter()` walks one list and
    /// consults a `FILTRULE_PERDIR_MERGE` entry at its own position, so a global
    /// rule defined before the directive is checked before the merge file's
    /// rules and one defined after is checked afterwards. This index records
    /// that position (measured in the same units as `CompiledRule::order`) so
    /// the chain can interleave global rules and this scope by source order.
    /// Defaults to `0` (directive precedes all global rules), which reproduces
    /// the historical "per-directory scope always overrides global" behaviour
    /// for callers that do not record a position.
    directive_order: usize,
}

impl DirMergeConfig {
    /// Creates a new configuration for a per-directory merge file.
    ///
    /// By default, rules are inherited by subdirectories and the file itself
    /// is not excluded from transfer.
    #[must_use]
    pub fn new(filename: impl Into<String>) -> Self {
        Self {
            filename: filename.into(),
            inherit: true,
            exclude_self: false,
            sender_only: false,
            receiver_only: false,
            anchor_root: false,
            perishable: false,
            cvs_mode: false,
            no_prefixes: false,
            no_prefixes_include: false,
            word_split: false,
            directive_order: 0,
        }
    }

    /// Records the source-stream position of this `dir-merge` directive.
    ///
    /// `order` is the count of order-bearing global rules (include/exclude/
    /// protect/risk) that preceded the directive, matching the units of
    /// `CompiledRule::order`. Global rules with a smaller order are checked
    /// before this scope's rules; those with an equal-or-greater order are
    /// checked after. Mirrors upstream `exclude.c:1046-1050`, where a
    /// `FILTRULE_PERDIR_MERGE` entry is consulted at its own list position.
    #[must_use]
    pub const fn with_directive_order(mut self, order: usize) -> Self {
        self.directive_order = order;
        self
    }

    /// Returns the source-stream position recorded by
    /// [`with_directive_order`](Self::with_directive_order).
    #[must_use]
    pub(super) const fn directive_order(&self) -> usize {
        self.directive_order
    }

    /// Sets whether rules from this merge file are inherited by subdirectories.
    ///
    /// When `false`, rules only apply within the directory containing the merge
    /// file. When `true` (default), rules propagate to all descendant directories
    /// unless overridden by a deeper merge file.
    ///
    /// Corresponds to upstream rsync's `n` modifier (no-inherit).
    #[must_use]
    pub const fn with_inherit(mut self, inherit: bool) -> Self {
        self.inherit = inherit;
        self
    }

    /// Sets whether the merge file itself should be excluded from transfer.
    ///
    /// Corresponds to upstream rsync's `e` modifier on dir-merge rules.
    #[must_use]
    pub const fn with_exclude_self(mut self, exclude: bool) -> Self {
        self.exclude_self = exclude;
        self
    }

    /// Restricts rules to the sender side only.
    ///
    /// Corresponds to upstream rsync's `s` modifier.
    #[must_use]
    pub const fn with_sender_only(mut self, sender_only: bool) -> Self {
        self.sender_only = sender_only;
        self
    }

    /// Restricts rules to the receiver side only.
    ///
    /// Corresponds to upstream rsync's `r` modifier.
    #[must_use]
    pub const fn with_receiver_only(mut self, receiver_only: bool) -> Self {
        self.receiver_only = receiver_only;
        self
    }

    /// Anchors patterns to the transfer root.
    #[must_use]
    pub const fn with_anchor_root(mut self, anchor: bool) -> Self {
        self.anchor_root = anchor;
        self
    }

    /// Returns whether patterns are anchored to the transfer root (the `/`
    /// FILTRULE_ABS_PATH modifier on the dir-merge rule).
    #[must_use]
    pub(super) const fn is_anchor_root(&self) -> bool {
        self.anchor_root
    }

    /// Marks parsed rules as perishable.
    #[must_use]
    pub const fn with_perishable(mut self, perishable: bool) -> Self {
        self.perishable = perishable;
        self
    }

    /// Configures this merge file as a CVS-style ignore list.
    ///
    /// When enabled, the file's contents are parsed as whitespace-separated
    /// tokens; each token becomes an exclude rule with no filter prefix
    /// honoured. This mirrors upstream rsync's `C` modifier on dir-merge rules
    /// (`FILTRULE_CVS_IGNORE`), which is set when the wire delivers
    /// `:C .cvsignore`.
    ///
    /// upstream: exclude.c:1248 - `C` modifier toggles FILTRULE_NO_PREFIXES |
    /// FILTRULE_WORD_SPLIT | FILTRULE_NO_INHERIT | FILTRULE_CVS_IGNORE.
    #[must_use]
    pub const fn with_cvs_mode(mut self, cvs_mode: bool) -> Self {
        self.cvs_mode = cvs_mode;
        self
    }

    /// Per-directory merge filename configured for this rule.
    #[must_use]
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Returns whether rules are inherited by subdirectories.
    #[must_use]
    pub const fn inherits(&self) -> bool {
        self.inherit
    }

    /// Returns whether this dir-merge uses CVS-style parsing.
    #[must_use]
    pub const fn cvs_mode(&self) -> bool {
        self.cvs_mode
    }

    /// Sets the `w` (word-split) modifier on this dir-merge.
    ///
    /// When enabled, the per-directory merge file is tokenised on any
    /// whitespace (space, tab, newline) with each token parsed as its own
    /// rule, mirroring upstream rsync's FILTRULE_WORD_SPLIT.
    ///
    /// upstream: exclude.c:1279-1283 - `w` modifier sets FILTRULE_WORD_SPLIT.
    #[must_use]
    pub const fn with_word_split(mut self, word_split: bool) -> Self {
        self.word_split = word_split;
        self
    }

    /// Returns whether this dir-merge tokenises its merge file on whitespace.
    #[must_use]
    pub const fn word_split(&self) -> bool {
        self.word_split
    }

    /// Marks this dir-merge as having FILTRULE_NO_PREFIXES set (the `-`/`+`
    /// modifier on a `:` rule). Per-dir merge file contents are then consumed
    /// as literal patterns rather than being run through the normal rule
    /// parser. `include` selects the `+` variant (literal includes); the
    /// default and `-` variant emit literal excludes.
    ///
    /// upstream: exclude.c:1116-1133 parse_rule_tok - prefix dispatch is
    /// skipped when FILTRULE_NO_PREFIXES is set on the template.
    #[must_use]
    pub const fn with_no_prefixes(mut self, no_prefixes: bool, include: bool) -> Self {
        self.no_prefixes = no_prefixes;
        self.no_prefixes_include = include;
        self
    }

    /// Returns whether FILTRULE_NO_PREFIXES applies to this dir-merge.
    #[must_use]
    pub const fn no_prefixes(&self) -> bool {
        self.no_prefixes
    }

    /// Returns whether literal lines should be treated as includes (true) or
    /// excludes (false) when [`Self::no_prefixes`] is set.
    #[must_use]
    pub const fn no_prefixes_include(&self) -> bool {
        self.no_prefixes_include
    }

    /// Returns whether the merge file itself should be excluded.
    #[must_use]
    pub const fn excludes_self(&self) -> bool {
        self.exclude_self
    }

    /// Reports whether this config restricts its rules to the sender side.
    ///
    /// Used to propagate the `s` modifier of a side-restricted per-directory
    /// merge into any `dir-merge` directives nested inside it, mirroring
    /// upstream `exclude.c:1293-1303` (`rflags |= template->rflags & SIDES`).
    #[must_use]
    pub(super) const fn is_sender_only(&self) -> bool {
        self.sender_only && !self.receiver_only
    }

    /// Reports whether this config restricts its rules to the receiver side.
    ///
    /// Mirror of [`Self::is_sender_only`] for the `r` modifier.
    #[must_use]
    pub(super) const fn is_receiver_only(&self) -> bool {
        self.receiver_only && !self.sender_only
    }

    /// Applies configured modifiers to a parsed rule.
    pub(super) fn apply_modifiers(&self, mut rule: FilterRule) -> FilterRule {
        if self.anchor_root {
            rule = rule.anchor_to_root();
        }
        if self.perishable {
            rule = rule.with_perishable(true);
        }
        if self.sender_only && !self.receiver_only {
            rule = rule.with_sides(true, false);
        } else if self.receiver_only && !self.sender_only {
            rule = rule.with_sides(false, true);
        }
        rule
    }
}
