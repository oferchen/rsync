use std::fmt;

/// Action taken when a rule matches a path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterAction {
    /// Include the matching path.
    Include,
    /// Exclude the matching path.
    Exclude,
    /// Protect the matching path from deletion while leaving transfer decisions unchanged.
    Protect,
    /// Remove previously applied protection, allowing deletion when matched.
    Risk,
    /// Clear previously defined filter rules for the affected transfer sides.
    Clear,
    /// Read additional filter rules from a file (`.` prefix in rsync).
    ///
    /// The pattern field contains the file path to read. Rules are read once
    /// when the filter set is compiled.
    Merge,
    /// Read filter rules per-directory during traversal (`,` prefix in rsync).
    ///
    /// The pattern field contains the filename to look for in each directory.
    /// Rules from the file are applied relative to that directory.
    DirMerge,
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
