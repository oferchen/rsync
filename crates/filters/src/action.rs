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
}

impl fmt::Display for FilterAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Include => f.write_str("include"),
            Self::Exclude => f.write_str("exclude"),
            Self::Protect => f.write_str("protect"),
            Self::Risk => f.write_str("risk"),
            Self::Clear => f.write_str("clear"),
        }
    }
}
