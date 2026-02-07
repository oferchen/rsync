use std::path::Path;
use std::sync::Arc;

use crate::{
    FilterAction, FilterError, FilterRule, MergeFileError,
    compiled::{CompiledRule, apply_clear_rule},
    cvs::default_patterns as cvs_default_patterns,
    decision::{DecisionContext, FilterSetInner},
    merge::read_rules_recursive,
};

/// Compiled, immutable collection of filter rules for fast path matching.
///
/// A `FilterSet` is built from a sequence of [`FilterRule`]s via
/// [`from_rules`](Self::from_rules) (or one of its variants). During
/// construction each rule is compiled into optimised glob matchers and
/// partitioned into two independent lists:
///
/// - **Include/Exclude** -- governs whether a path is transferred.
/// - **Protect/Risk** -- governs whether a path may be deleted on the
///   receiver when `--delete` is active.
///
/// Both lists use first-match-wins evaluation. If no include/exclude rule
/// matches, the path is included by default.
///
/// `FilterSet` is cheaply cloneable (the inner state is behind an [`Arc`]).
///
/// # Examples
///
/// ```
/// use filters::{FilterRule, FilterSet};
/// use std::path::Path;
///
/// let set = FilterSet::from_rules([
///     FilterRule::exclude("*.o"),
///     FilterRule::include("important.o"),
/// ]).unwrap();
///
/// // first-match-wins: "*.o" matches first
/// assert!(!set.allows(Path::new("main.o"), false));
/// // non-matching paths are included by default
/// assert!(set.allows(Path::new("README.md"), false));
/// ```
#[derive(Clone, Debug, Default)]
pub struct FilterSet {
    inner: Arc<FilterSetInner>,
}

impl FilterSet {
    /// Builds a [`FilterSet`] from the supplied rules.
    ///
    /// Rules are compiled in iteration order. Include/Exclude rules are placed
    /// in the transfer list while Protect/Risk rules go into the deletion list.
    /// A [`Clear`](FilterAction::Clear) rule removes all prior rules that
    /// match its side flags.
    ///
    /// # Merge and DirMerge Rules
    ///
    /// Merge rules (`. FILE`) and dir-merge rules (`: FILE`) are skipped when
    /// building the filter set. To automatically expand merge files, use
    /// [`from_rules_with_merge_expansion`](Self::from_rules_with_merge_expansion)
    /// instead.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] if any rule's pattern cannot be compiled into a
    /// valid glob matcher.
    pub fn from_rules<I>(rules: I) -> Result<Self, FilterError>
    where
        I: IntoIterator<Item = FilterRule>,
    {
        let mut include_exclude = Vec::new();
        let mut protect_risk = Vec::new();

        for rule in rules.into_iter() {
            if rule.is_xattr_only() {
                continue;
            }
            match rule.action {
                FilterAction::Include | FilterAction::Exclude => {
                    include_exclude.push(CompiledRule::new(rule)?);
                }
                FilterAction::Protect | FilterAction::Risk => {
                    protect_risk.push(CompiledRule::new(rule)?);
                }
                FilterAction::Clear => {
                    apply_clear_rule(
                        &mut include_exclude,
                        rule.applies_to_sender,
                        rule.applies_to_receiver,
                    );
                    apply_clear_rule(
                        &mut protect_risk,
                        rule.applies_to_sender,
                        rule.applies_to_receiver,
                    );
                }
                FilterAction::Merge | FilterAction::DirMerge => {
                    // Merge rules are processed during expansion, not compilation.
                    // DirMerge rules are processed per-directory during traversal.
                    // Both are skipped here - use from_rules_with_merge_expansion
                    // for automatic merge file processing.
                }
            }
        }

        Ok(Self {
            inner: Arc::new(FilterSetInner {
                include_exclude,
                protect_risk,
            }),
        })
    }

    /// Returns `true` if the set contains no compiled rules of any kind.
    ///
    /// An empty filter set allows all paths and all deletions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.include_exclude.is_empty() && self.inner.protect_risk.is_empty()
    }

    /// Returns `true` if the path should be included in the transfer.
    ///
    /// Evaluates sender-side include/exclude rules in definition order using
    /// first-match-wins semantics. Paths that match no rule are included by
    /// default. Perishable rules are considered during transfer checks.
    ///
    /// `is_dir` should be `true` when the path refers to a directory, which
    /// affects directory-only rules (patterns with a trailing `/`).
    #[must_use]
    pub fn allows(&self, path: &Path, is_dir: bool) -> bool {
        self.inner
            .decision(path, is_dir, DecisionContext::Transfer)
            .allows_transfer()
    }

    /// Returns `true` if deleting the path on the receiver is permitted.
    ///
    /// A path may be deleted when:
    /// 1. It is *included* by receiver-side include/exclude rules (perishable
    ///    rules are ignored for deletion), **and**
    /// 2. No protect rule matches the path.
    ///
    /// This matches upstream rsync's `--delete` semantics combined with
    /// `--filter 'protect ...'`.
    #[must_use]
    pub fn allows_deletion(&self, path: &Path, is_dir: bool) -> bool {
        self.inner
            .decision(path, is_dir, DecisionContext::Deletion)
            .allows_deletion()
    }

    /// Returns `true` if the path may be removed during `--delete-excluded`
    /// processing.
    ///
    /// Unlike [`allows_deletion`](Self::allows_deletion), this method checks
    /// whether the path is *excluded* (rather than included) by receiver-side
    /// rules while still honouring protect directives.  It is used to implement
    /// rsync's `--delete-excluded` flag, which removes destination files that
    /// the filter list would have excluded from transfer.
    #[must_use]
    pub fn allows_deletion_when_excluded_removed(&self, path: &Path, is_dir: bool) -> bool {
        self.inner
            .decision(path, is_dir, DecisionContext::Deletion)
            .allows_deletion_when_excluded_removed()
    }

    /// Builds a [`FilterSet`] from the supplied rules with CVS exclusions appended.
    ///
    /// CVS exclusions are added at the end of the rule list, giving them lower
    /// priority than explicitly specified rules. This matches upstream rsync's
    /// `--cvs-exclude` (`-C`) behavior.
    ///
    /// The CVS exclusion patterns include common version control directories
    /// (`.git/`, `.svn/`, `CVS/`, etc.), build artifacts (`*.o`, `*.so`), and
    /// editor backup files (`*~`, `*.bak`).
    ///
    /// # Perishable Rules
    ///
    /// When `perishable` is `true`, the CVS patterns are marked as perishable,
    /// meaning they can be overridden by explicit include rules. This matches
    /// rsync protocol version 30+ behavior.
    ///
    /// # Examples
    ///
    /// ```
    /// use filters::{FilterRule, FilterSet};
    /// use std::path::Path;
    ///
    /// let rules = [FilterRule::include("important.o")];
    /// let set = FilterSet::from_rules_with_cvs(rules, true).unwrap();
    ///
    /// // CVS patterns exclude .o files
    /// assert!(!set.allows(Path::new("main.o"), false));
    /// // But explicit includes still work (if perishable)
    /// ```
    pub fn from_rules_with_cvs<I>(rules: I, perishable: bool) -> Result<Self, FilterError>
    where
        I: IntoIterator<Item = FilterRule>,
    {
        let mut all_rules: Vec<FilterRule> = rules.into_iter().collect();
        all_rules.extend(cvs_exclusion_rules(perishable));
        Self::from_rules(all_rules)
    }

    /// Builds a [`FilterSet`] from rules, expanding merge file references.
    ///
    /// When a merge rule (`. FILE`) is encountered, the referenced file is
    /// read and its rules are inlined at that position. This process is
    /// recursive up to `max_depth` levels to prevent infinite loops.
    ///
    /// Dir-merge rules (`, FILE`) are skipped since they're processed
    /// per-directory during traversal, not at compilation time.
    ///
    /// # Arguments
    ///
    /// * `rules` - Initial filter rules, may contain merge directives
    /// * `max_depth` - Maximum merge file nesting depth (typically 10)
    ///
    /// # Errors
    ///
    /// Returns [`FilterSetError`] if a merge file cannot be read or parsed,
    /// or if the maximum depth is exceeded.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use filters::{FilterRule, FilterSet};
    ///
    /// let rules = [
    ///     FilterRule::include("*.txt"),
    ///     FilterRule::merge("/etc/rsync/global.rules"),
    ///     FilterRule::exclude("*.tmp"),
    /// ];
    /// let set = FilterSet::from_rules_with_merge_expansion(rules, 10).unwrap();
    /// ```
    pub fn from_rules_with_merge_expansion<I>(
        rules: I,
        max_depth: usize,
    ) -> Result<Self, FilterSetError>
    where
        I: IntoIterator<Item = FilterRule>,
    {
        let expanded = expand_merge_rules(rules.into_iter().collect(), max_depth, 0)?;
        Self::from_rules(expanded).map_err(FilterSetError::Filter)
    }
}

/// Error returned by [`FilterSet::from_rules_with_merge_expansion`].
///
/// Wraps either a merge-file I/O or parse error, or a glob compilation error
/// from one of the expanded rules.
#[derive(Debug)]
pub enum FilterSetError {
    /// A merge file could not be read, parsed, or exceeded the depth limit.
    Merge(MergeFileError),
    /// A filter rule's pattern could not be compiled into a glob matcher.
    Filter(FilterError),
}

impl std::fmt::Display for FilterSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Merge(e) => write!(f, "merge file error: {e}"),
            Self::Filter(e) => write!(f, "filter error: {e}"),
        }
    }
}

impl std::error::Error for FilterSetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Merge(e) => Some(e),
            Self::Filter(e) => Some(e),
        }
    }
}

impl From<MergeFileError> for FilterSetError {
    fn from(e: MergeFileError) -> Self {
        Self::Merge(e)
    }
}

impl From<FilterError> for FilterSetError {
    fn from(e: FilterError) -> Self {
        Self::Filter(e)
    }
}

/// Recursively expands merge rules by reading and inlining their contents.
fn expand_merge_rules(
    rules: Vec<FilterRule>,
    max_depth: usize,
    current_depth: usize,
) -> Result<Vec<FilterRule>, MergeFileError> {
    if current_depth > max_depth {
        return Err(MergeFileError {
            path: "<expansion>".to_string(),
            line: None,
            message: format!("maximum merge depth ({max_depth}) exceeded"),
        });
    }

    let mut expanded = Vec::with_capacity(rules.len());

    for rule in rules {
        match rule.action() {
            FilterAction::Merge => {
                let merge_path = Path::new(rule.pattern());
                let nested =
                    read_rules_recursive(merge_path, max_depth.saturating_sub(current_depth))?;
                let nested_expanded = expand_merge_rules(nested, max_depth, current_depth + 1)?;
                expanded.extend(nested_expanded);
            }
            FilterAction::DirMerge => {
                // DirMerge rules are processed per-directory during traversal,
                // not expanded at compile time. We skip them here.
            }
            _ => {
                expanded.push(rule);
            }
        }
    }

    Ok(expanded)
}

/// Creates filter rules for the default CVS exclusion patterns.
///
/// These rules exclude common version control directories, build artifacts,
/// and editor backup files. The patterns match upstream rsync's `--cvs-exclude`
/// (`-C`) option.
///
/// # Arguments
///
/// * `perishable` - If `true`, marks rules as perishable (can be overridden).
///   This should be `true` for rsync protocol version 30+.
///
/// # Examples
///
/// ```
/// use filters::cvs_exclusion_rules;
///
/// let rules: Vec<_> = cvs_exclusion_rules(false).collect();
/// assert!(!rules.is_empty());
/// ```
pub fn cvs_exclusion_rules(perishable: bool) -> impl Iterator<Item = FilterRule> {
    cvs_default_patterns().map(move |pattern| {
        let mut rule = FilterRule::exclude(pattern.to_owned());
        if perishable {
            rule = rule.with_perishable(true);
        }
        rule
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_set_default_is_empty() {
        let set = FilterSet::default();
        assert!(set.is_empty());
    }

    #[test]
    fn filter_set_from_empty_rules() {
        let set = FilterSet::from_rules(vec![]).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn filter_set_with_include_not_empty() {
        let set = FilterSet::from_rules(vec![FilterRule::include("*.txt".to_owned())]).unwrap();
        assert!(!set.is_empty());
    }

    #[test]
    fn filter_set_with_exclude_not_empty() {
        let set = FilterSet::from_rules(vec![FilterRule::exclude("*.bak".to_owned())]).unwrap();
        assert!(!set.is_empty());
    }

    #[test]
    fn filter_set_allows_by_default() {
        let set = FilterSet::default();
        assert!(set.allows(Path::new("file.txt"), false));
    }

    #[test]
    fn filter_set_allows_deletion_by_default() {
        let set = FilterSet::default();
        assert!(set.allows_deletion(Path::new("file.txt"), false));
    }

    #[test]
    fn filter_set_exclude_blocks() {
        let set = FilterSet::from_rules(vec![FilterRule::exclude("*.bak".to_owned())]).unwrap();
        assert!(!set.allows(Path::new("file.bak"), false));
    }

    #[test]
    fn filter_set_exclude_allows_non_matching() {
        let set = FilterSet::from_rules(vec![FilterRule::exclude("*.bak".to_owned())]).unwrap();
        assert!(set.allows(Path::new("file.txt"), false));
    }

    #[test]
    fn filter_set_include_allows() {
        // rsync uses first-match-wins: include must come before exclude for exceptions
        let rules = vec![
            FilterRule::include("*.txt".to_owned()),
            FilterRule::exclude("*".to_owned()),
        ];
        let set = FilterSet::from_rules(rules).unwrap();
        assert!(set.allows(Path::new("file.txt"), false));
    }

    #[test]
    fn filter_set_protect_blocks_deletion() {
        let set =
            FilterSet::from_rules(vec![FilterRule::protect("/important".to_owned())]).unwrap();
        assert!(!set.allows_deletion(Path::new("important"), false));
    }

    // CVS exclusion tests

    #[test]
    fn cvs_exclusion_rules_not_empty() {
        let rules: Vec<_> = cvs_exclusion_rules(false).collect();
        assert!(!rules.is_empty());
    }

    #[test]
    fn cvs_exclusion_rules_perishable_flag() {
        let perishable_rules: Vec<_> = cvs_exclusion_rules(true).collect();
        let non_perishable_rules: Vec<_> = cvs_exclusion_rules(false).collect();

        for rule in &perishable_rules {
            assert!(rule.is_perishable());
        }
        for rule in &non_perishable_rules {
            assert!(!rule.is_perishable());
        }
    }

    #[test]
    fn from_rules_with_cvs_excludes_git() {
        let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();
        assert!(!set.allows(Path::new(".git"), true));
    }

    #[test]
    fn from_rules_with_cvs_excludes_svn() {
        let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();
        assert!(!set.allows(Path::new(".svn"), true));
    }

    #[test]
    fn from_rules_with_cvs_excludes_object_files() {
        let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();
        assert!(!set.allows(Path::new("main.o"), false));
        assert!(!set.allows(Path::new("lib.so"), false));
        assert!(!set.allows(Path::new("program.exe"), false));
    }

    #[test]
    fn from_rules_with_cvs_excludes_backup_files() {
        let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();
        assert!(!set.allows(Path::new("file.bak"), false));
        assert!(!set.allows(Path::new("file.BAK"), false));
        assert!(!set.allows(Path::new("file~"), false));
        assert!(!set.allows(Path::new("file.orig"), false));
    }

    #[test]
    fn from_rules_with_cvs_allows_normal_files() {
        let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();
        assert!(set.allows(Path::new("main.c"), false));
        assert!(set.allows(Path::new("README.md"), false));
        assert!(set.allows(Path::new("Cargo.toml"), false));
    }

    #[test]
    fn from_rules_with_cvs_explicit_rules_higher_priority() {
        // Explicit include rule should take precedence over CVS exclusions
        // rsync uses first-match-wins: explicit rules come first, so they have priority
        let rules = vec![FilterRule::include("*.o")];
        let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();
        // With first-match-wins, explicit include matches before CVS exclude
        assert!(set.allows(Path::new("main.o"), false));
    }

    #[test]
    fn from_rules_with_cvs_explicit_exclude_still_works() {
        let rules = vec![FilterRule::exclude("*.txt")];
        let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();
        // Both explicit and CVS exclusions should work
        assert!(!set.allows(Path::new("notes.txt"), false));
        assert!(!set.allows(Path::new("main.o"), false));
    }
}
