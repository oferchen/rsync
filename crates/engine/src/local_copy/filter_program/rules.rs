//! Per-directory merge rule descriptor used by `FilterProgram` to load
//! `.rsync-filter`-style files during traversal.

use std::path::{Path, PathBuf};

use super::options::DirMergeOptions;

/// Description of a `.rsync-filter` style per-directory rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirMergeRule {
    pattern: PathBuf,
    options: DirMergeOptions,
}

impl DirMergeRule {
    /// Creates a new [`DirMergeRule`].
    #[must_use]
    pub fn new(pattern: impl Into<PathBuf>, options: DirMergeOptions) -> Self {
        Self {
            pattern: pattern.into(),
            options,
        }
    }

    /// Returns the configured filter file pattern.
    #[must_use]
    pub fn pattern(&self) -> &Path {
        self.pattern.as_path()
    }

    /// Returns the behavioural modifiers applied to this merge rule.
    #[must_use]
    pub const fn options(&self) -> &DirMergeOptions {
        &self.options
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_merge_rule_new() {
        let rule = DirMergeRule::new("/filter", DirMergeOptions::default());
        assert_eq!(rule.pattern(), Path::new("/filter"));
    }

    #[test]
    fn dir_merge_rule_pattern() {
        let rule = DirMergeRule::new(".rsync-filter", DirMergeOptions::default());
        assert_eq!(rule.pattern(), Path::new(".rsync-filter"));
    }

    #[test]
    fn dir_merge_rule_eq() {
        let a = DirMergeRule::new("filter", DirMergeOptions::default());
        let b = DirMergeRule::new("filter", DirMergeOptions::default());
        assert_eq!(a, b);
    }
}
