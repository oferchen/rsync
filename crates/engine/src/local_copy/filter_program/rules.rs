use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::super::LocalCopyError;
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

/// Excludes directories that contain a particular marker file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExcludeIfPresentRule {
    raw_pattern: String,
    pattern: PathBuf,
}

impl ExcludeIfPresentRule {
    /// Creates a new rule that checks for the provided marker file.
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        let raw_pattern = pattern.into();
        let pattern = PathBuf::from(&raw_pattern);
        Self {
            raw_pattern,
            pattern,
        }
    }

    pub(crate) fn marker_path(&self, directory: &Path) -> PathBuf {
        if self.pattern.is_absolute() {
            self.pattern.clone()
        } else {
            directory.join(&self.pattern)
        }
    }

    pub(crate) fn marker_exists(&self, directory: &Path) -> io::Result<bool> {
        let target = self.marker_path(directory);
        match fs::symlink_metadata(&target) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

pub(crate) fn directory_has_marker(
    rules: &[ExcludeIfPresentRule],
    directory: &Path,
) -> Result<bool, LocalCopyError> {
    for rule in rules {
        match rule.marker_exists(directory) {
            Ok(true) => return Ok(true),
            Ok(false) => continue,
            Err(error) => {
                let path = rule.marker_path(directory);
                return Err(LocalCopyError::io(
                    "inspect exclude-if-present marker",
                    path,
                    error,
                ));
            }
        }
    }

    Ok(false)
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

    #[test]
    fn exclude_if_present_rule_new() {
        let rule = ExcludeIfPresentRule::new(".nobackup");
        assert_eq!(rule.pattern, PathBuf::from(".nobackup"));
        assert_eq!(rule.raw_pattern, ".nobackup");
    }

    #[test]
    fn exclude_if_present_marker_path_relative() {
        let rule = ExcludeIfPresentRule::new(".nobackup");
        let path = rule.marker_path(Path::new("/home/user/docs"));
        assert_eq!(path, PathBuf::from("/home/user/docs/.nobackup"));
    }

    #[test]
    fn exclude_if_present_marker_path_absolute() {
        let rule = ExcludeIfPresentRule::new("/etc/nobackup");
        let path = rule.marker_path(Path::new("/home/user/docs"));
        assert_eq!(path, PathBuf::from("/etc/nobackup"));
    }

    #[test]
    fn exclude_if_present_rule_eq() {
        let a = ExcludeIfPresentRule::new(".nobackup");
        let b = ExcludeIfPresentRule::new(".nobackup");
        assert_eq!(a, b);
    }

    #[test]
    fn exclude_if_present_rule_ne() {
        let a = ExcludeIfPresentRule::new(".nobackup");
        let b = ExcludeIfPresentRule::new(".skip");
        assert_ne!(a, b);
    }

    #[test]
    fn directory_has_marker_empty_rules() {
        let rules: Vec<ExcludeIfPresentRule> = vec![];
        let result = directory_has_marker(&rules, Path::new("/nonexistent")).unwrap();
        assert!(!result);
    }
}
