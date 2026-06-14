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
        }
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

    /// Returns whether the merge file itself should be excluded.
    #[must_use]
    pub const fn excludes_self(&self) -> bool {
        self.exclude_self
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
