use std::fmt;

/// Action taken when a [`FilterRule`](crate::FilterRule) matches a path.
///
/// These actions correspond to upstream rsync's filter rule prefixes defined in
/// `exclude.c`. Short-form prefixes (`+`, `-`, `P`, `R`, `.`, `:`, `!`) and
/// long-form keywords (`include`, `exclude`, etc.) both map to these variants.
///
/// Actions fall into three categories:
///
/// | Category | Variants | Effect |
/// |----------|----------|--------|
/// | Transfer | [`Include`](Self::Include), [`Exclude`](Self::Exclude) | Control whether a path is copied |
/// | Deletion | [`Protect`](Self::Protect), [`Risk`](Self::Risk) | Guard or expose paths during `--delete` |
/// | Meta | [`Clear`](Self::Clear), [`Merge`](Self::Merge), [`DirMerge`](Self::DirMerge) | Modify the rule list itself |
///
/// Transfer rules are evaluated in definition order with first-match-wins
/// semantics. Protect/Risk rules are tracked in a separate list and apply
/// independently of the transfer decision.
///
/// # Display
///
/// Each variant implements [`Display`](std::fmt::Display) as a lowercase token
/// (e.g. `"include"`, `"dir-merge"`), suitable for diagnostics and
/// serialisation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterAction {
    /// Include the matching path in the transfer.
    ///
    /// Corresponds to `+` (short form) or `include` (long form) in filter syntax.
    Include,
    /// Exclude the matching path from the transfer.
    ///
    /// Corresponds to `-` (short form) or `exclude` (long form) in filter syntax.
    Exclude,
    /// Protect the matching path from deletion while leaving transfer decisions unchanged.
    ///
    /// Corresponds to `P` (short form) or `protect` (long form). Protect rules
    /// only apply on the receiver side and prevent `--delete` from removing
    /// matching paths regardless of the include/exclude outcome.
    Protect,
    /// Remove previously applied protection, allowing deletion when matched.
    ///
    /// Corresponds to `R` (short form) or `risk` (long form). A Risk rule
    /// cancels the effect of an earlier Protect rule for the same path.
    Risk,
    /// Clear previously defined filter rules for the affected transfer sides.
    ///
    /// Corresponds to `!` (short form) or `clear` (long form). When processed,
    /// all prior include/exclude and protect/risk rules matching the affected
    /// sides are removed.
    Clear,
    /// Read additional filter rules from a file (`.` prefix in rsync).
    ///
    /// The pattern field contains the file path to read. Rules are read once
    /// when the filter set is compiled.
    Merge,
    /// Read filter rules per-directory during traversal (`:` prefix in rsync).
    ///
    /// The pattern field contains the filename to look for in each directory.
    /// Rules from the file are applied relative to that directory.
    DirMerge,
}

impl FilterAction {
    /// Resolves this action to the include/exclude decision upstream stores for
    /// an xattr-name rule, or `None` for a meta action that carries none.
    ///
    /// upstream: exclude.c:1345-1358 - the four side-bound prefixes are nothing
    /// but (include|exclude) x (sender|receiver) bit pairs:
    ///
    /// | prefix      | upstream rflags                   | decision |
    /// |-------------|-----------------------------------|----------|
    /// | `S`/show    | `FILTRULE_INCLUDE\|SENDER_SIDE`   | include  |
    /// | `H`/hide    | `FILTRULE_SENDER_SIDE`            | exclude  |
    /// | `R`/risk    | `FILTRULE_INCLUDE\|RECEIVER_SIDE` | include  |
    /// | `P`/protect | `FILTRULE_RECEIVER_SIDE`          | exclude  |
    ///
    /// There is no distinct protect/risk *action* upstream. oc models the two
    /// receiver-side spellings as their own variants because its delete pass
    /// needs them separable on the PATH chain; but an xattr name is never
    /// deleted, so on an xattr chain the extra variants have no meaning and
    /// must collapse back onto upstream's two-value decision.
    ///
    /// This is the single owner of that table: both the [`crate`] xattr chain
    /// and the engine's local-copy chain resolve through it, so the two cannot
    /// drift apart.
    ///
    /// # Examples
    ///
    /// ```
    /// use filters::FilterAction;
    /// assert_eq!(FilterAction::Protect.xattr_decision(), Some(FilterAction::Exclude));
    /// assert_eq!(FilterAction::Risk.xattr_decision(), Some(FilterAction::Include));
    /// assert_eq!(FilterAction::Clear.xattr_decision(), None);
    /// ```
    #[must_use]
    pub const fn xattr_decision(self) -> Option<Self> {
        match self {
            Self::Include | Self::Risk => Some(Self::Include),
            Self::Exclude | Self::Protect => Some(Self::Exclude),
            // `!`/clear is handled by the set's clear pass, and the merge
            // prefixes consume `x` and drop it (upstream: exclude.c:1229
            // FILTRULES_FROM_CONTAINER omits XATTR), so no merge rule reaches
            // an xattr chain carrying the flag.
            Self::Clear | Self::Merge | Self::DirMerge => None,
        }
    }
}

impl fmt::Display for FilterAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Include => f.write_str("include"),
            Self::Exclude => f.write_str("exclude"),
            Self::Protect => f.write_str("protect"),
            Self::Risk => f.write_str("risk"),
            Self::Clear => f.write_str("clear"),
            Self::Merge => f.write_str("merge"),
            Self::DirMerge => f.write_str("dir-merge"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FilterAction;

    #[test]
    fn display_variants_matches_expected_tokens() {
        let cases = [
            (FilterAction::Include, "include"),
            (FilterAction::Exclude, "exclude"),
            (FilterAction::Protect, "protect"),
            (FilterAction::Risk, "risk"),
            (FilterAction::Clear, "clear"),
            (FilterAction::Merge, "merge"),
            (FilterAction::DirMerge, "dir-merge"),
        ];

        for (action, expected) in cases {
            assert_eq!(action.to_string(), expected);
        }
    }
}
