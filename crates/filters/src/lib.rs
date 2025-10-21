#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_filters` provides ordered include/exclude pattern evaluation for the
//! Rust `oc-rsync` workspace. The implementation focuses on reproducing the
//! subset of rsync's filter grammar that governs `--include`/`--exclude`
//! handling for local filesystem transfers. Patterns honour anchored matches
//! (leading `/`), directory-only rules (trailing `/`), and recursive wildcards
//! using the same glob semantics exposed by upstream rsync. Rules are evaluated
//! sequentially with the last matching rule determining whether a path is
//! copied.
//!
//! # Design
//!
//! - [`FilterRule`] captures the user-supplied action (`Include`/`Exclude`) and
//!   pattern text. The rule itself is lightweight; heavy lifting happens when a
//!   [`FilterSet`] is constructed.
//! - [`FilterSet`] owns the compiled representation of each rule, expanding
//!   directory-only patterns into matchers that also cover their contents while
//!   deduplicating equivalent glob expressions.
//! - Matching occurs against relative paths using native [`Path`] semantics so
//!   callers can operate directly on `std::path::PathBuf` instances without
//!   additional conversions.
//!
//! # Invariants
//!
//! - Rules are applied in definition order. The last matching rule wins and
//!   defaults to `Include` when no rule matches.
//! - Trailing `/` marks a directory-only rule. The directory itself must match
//!   the rule to trigger exclusion; descendants are excluded automatically.
//! - Leading `/` anchors a rule to the transfer root. Patterns without a leading
//!   slash are matched at any depth by implicitly prefixing `**/`.
//!
//! # Errors
//!
//! [`FilterSet::from_rules`] reports [`FilterError`] when a rule expands to an
//! invalid glob expression. The error includes the offending pattern and the
//! underlying [`globset::Error`] for debugging.
//!
//! # Examples
//!
//! Build a filter list that excludes editor swap files while explicitly
//! re-including a tracked directory:
//!
//! ```
//! use rsync_filters::{FilterRule, FilterSet};
//! use std::path::Path;
//!
//! let rules = [
//!     FilterRule::exclude("*.swp"),
//!     FilterRule::exclude("*.tmp"),
//!     FilterRule::include("important/"),
//! ];
//! let filters = FilterSet::from_rules(rules).expect("filters compile");
//!
//! assert!(filters.allows(Path::new("notes.txt"), false));
//! assert!(filters.allows(Path::new("important/report.txt"), false));
//! assert!(!filters.allows(Path::new("scratch.swp"), false));
//! ```
//!
//! # See also
//!
//! - [`rsync_engine::local_copy`] integrates [`FilterSet`] to prune directory
//!   traversals during deterministic local copies.
//! - [`globset`] for the glob matching primitives used internally.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use globset::{GlobBuilder, GlobMatcher};

/// Action taken when a rule matches a path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterAction {
    /// Include the matching path.
    Include,
    /// Exclude the matching path.
    Exclude,
}

/// User-visible filter rule consisting of an action and pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterRule {
    action: FilterAction,
    pattern: String,
}

impl FilterRule {
    /// Creates an include rule for `pattern`.
    #[must_use]
    pub fn include(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Include,
            pattern: pattern.into(),
        }
    }

    /// Creates an exclude rule for `pattern`.
    #[must_use]
    pub fn exclude(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Exclude,
            pattern: pattern.into(),
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
}

/// Error produced when a rule cannot be compiled into a matcher.
#[derive(Debug)]
pub struct FilterError {
    pattern: String,
    source: globset::Error,
}

impl FilterError {
    /// Creates a new [`FilterError`] for the given pattern and source error.
    fn new(pattern: String, source: globset::Error) -> Self {
        Self { pattern, source }
    }

    /// Returns the offending pattern.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to compile filter pattern '{}': {}",
            self.pattern, self.source
        )
    }
}

impl std::error::Error for FilterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Ordered collection of filter rules.
#[derive(Clone, Debug, Default)]
pub struct FilterSet {
    inner: Arc<FilterSetInner>,
}

impl FilterSet {
    /// Builds a [`FilterSet`] from the supplied rules.
    pub fn from_rules<I>(rules: I) -> Result<Self, FilterError>
    where
        I: IntoIterator<Item = FilterRule>,
    {
        let compiled = rules
            .into_iter()
            .map(|rule| CompiledRule::new(rule.action, rule.pattern))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            inner: Arc::new(FilterSetInner { rules: compiled }),
        })
    }

    /// Reports whether the set contains any rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.rules.is_empty()
    }

    /// Determines whether the provided path is allowed.
    #[must_use]
    pub fn allows(&self, path: &Path, is_dir: bool) -> bool {
        self.inner.allows(path, is_dir)
    }
}

#[derive(Debug, Default)]
struct FilterSetInner {
    rules: Vec<CompiledRule>,
}

impl FilterSetInner {
    fn allows(&self, path: &Path, is_dir: bool) -> bool {
        let mut decision = true;
        for rule in &self.rules {
            if rule.matches(path, is_dir) {
                decision = matches!(rule.action, FilterAction::Include);
            }
        }
        decision
    }
}

#[derive(Debug)]
struct CompiledRule {
    action: FilterAction,
    directory_only: bool,
    direct_matchers: Vec<GlobMatcher>,
    descendant_matchers: Vec<GlobMatcher>,
}

impl CompiledRule {
    fn new(action: FilterAction, pattern: String) -> Result<Self, FilterError> {
        let (anchored, directory_only, core_pattern) = normalise_pattern(&pattern);
        let mut direct_patterns = HashSet::new();
        direct_patterns.insert(core_pattern.clone());
        if !anchored {
            direct_patterns.insert(format!("**/{}", core_pattern));
        }

        let mut descendant_patterns = HashSet::new();
        if directory_only || matches!(action, FilterAction::Exclude) {
            descendant_patterns.insert(format!("{}/**", core_pattern));
            if !anchored {
                descendant_patterns.insert(format!("**/{}/**", core_pattern));
            }
        }

        let direct_matchers = compile_patterns(direct_patterns, &pattern)?;
        let descendant_matchers = compile_patterns(descendant_patterns, &pattern)?;

        Ok(Self {
            action,
            directory_only,
            direct_matchers,
            descendant_matchers,
        })
    }

    fn matches(&self, path: &Path, is_dir: bool) -> bool {
        for matcher in &self.direct_matchers {
            if matcher.is_match(path) {
                if !self.directory_only || is_dir {
                    return true;
                }
            }
        }

        if !self.descendant_matchers.is_empty() {
            for matcher in &self.descendant_matchers {
                if matcher.is_match(path) {
                    return true;
                }
            }
        }

        false
    }
}

fn compile_patterns(
    patterns: HashSet<String>,
    original: &str,
) -> Result<Vec<GlobMatcher>, FilterError> {
    let mut unique: Vec<_> = patterns.into_iter().collect();
    unique.sort();

    let mut matchers = Vec::with_capacity(unique.len());
    for pattern in unique {
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| FilterError::new(original.to_string(), error))?;
        matchers.push(glob.compile_matcher());
    }
    Ok(matchers)
}

fn normalise_pattern(pattern: &str) -> (bool, bool, String) {
    let anchored = pattern.starts_with('/');
    let directory_only = pattern.ends_with('/');
    let mut core = pattern;
    if anchored {
        core = &core[1..];
    }
    if directory_only && !core.is_empty() {
        core = &core[..core.len() - 1];
    }
    (anchored, directory_only, core.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn empty_rules_allow_everything() {
        let set = FilterSet::from_rules(Vec::new()).expect("empty set");
        assert!(set.allows(Path::new("foo"), false));
    }

    #[test]
    fn include_rule_allows_path() {
        let set = FilterSet::from_rules([FilterRule::include("foo")]).expect("compiled");
        assert!(set.allows(Path::new("foo"), false));
    }

    #[test]
    fn exclude_rule_blocks_match() {
        let set = FilterSet::from_rules([FilterRule::exclude("foo")]).expect("compiled");
        assert!(!set.allows(Path::new("foo"), false));
        assert!(!set.allows(Path::new("bar/foo"), false));
    }

    #[test]
    fn include_after_exclude_reinstates_path() {
        let rules = [
            FilterRule::exclude("foo"),
            FilterRule::include("foo/bar.txt"),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(set.allows(Path::new("foo/bar.txt"), false));
        assert!(!set.allows(Path::new("foo/baz.txt"), false));
    }

    #[test]
    fn anchored_pattern_matches_only_at_root() {
        let set = FilterSet::from_rules([FilterRule::exclude("/foo/bar")]).expect("compiled");
        assert!(!set.allows(Path::new("foo/bar"), false));
        assert!(set.allows(Path::new("a/foo/bar"), false));
    }

    #[test]
    fn directory_rule_excludes_children() {
        let set = FilterSet::from_rules([FilterRule::exclude("build/")]).expect("compiled");
        assert!(!set.allows(Path::new("build"), true));
        assert!(!set.allows(Path::new("build/output.bin"), false));
        assert!(!set.allows(Path::new("dir/build/log.txt"), false));
    }

    #[test]
    fn wildcard_patterns_match_expected_paths() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).expect("compiled");
        assert!(!set.allows(Path::new("note.tmp"), false));
        assert!(!set.allows(Path::new("dir/note.tmp"), false));
        assert!(set.allows(Path::new("note.txt"), false));
    }

    #[test]
    fn invalid_pattern_reports_error() {
        let error = FilterSet::from_rules([FilterRule::exclude("[")]).expect_err("invalid");
        assert_eq!(error.pattern(), "[");
    }

    #[test]
    fn glob_escape_sequences_supported() {
        let set = FilterSet::from_rules([FilterRule::exclude("foo\\?bar")]).expect("compiled");
        assert!(!set.allows(Path::new("foo?bar"), false));
        assert!(set.allows(Path::new("fooXbar"), false));
    }

    #[test]
    fn ordering_respected() {
        let rules = [
            FilterRule::exclude("*.tmp"),
            FilterRule::include("special.tmp"),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(set.allows(Path::new("special.tmp"), false));
        assert!(!set.allows(Path::new("other.tmp"), false));
    }

    #[test]
    fn directory_rule_requires_directory_for_exact_match() {
        let set = FilterSet::from_rules([FilterRule::exclude("foo/")]).expect("compiled");
        assert!(set.allows(Path::new("foo"), false));
    }

    #[test]
    fn directory_rule_still_excludes_nested_files() {
        let set = FilterSet::from_rules([FilterRule::exclude("foo/")]).expect("compiled");
        assert!(!set.allows(Path::new("foo/bar/baz.txt"), false));
    }

    #[test]
    fn deep_paths_match_unanchored_pattern() {
        let set = FilterSet::from_rules([FilterRule::exclude("foo/bar")]).expect("compiled");
        assert!(!set.allows(Path::new("foo/bar"), false));
        assert!(!set.allows(Path::new("a/foo/bar"), false));
    }

    #[test]
    fn trailing_slash_is_optional_for_directory_includes() {
        let set = FilterSet::from_rules([FilterRule::exclude("foo/"), FilterRule::include("foo")])
            .expect("compiled");
        assert!(set.allows(Path::new("foo"), true));
        assert!(!set.allows(Path::new("foo/bar"), false));
    }

    #[test]
    fn multiple_rules_compile_without_duplicates() {
        let rules = [FilterRule::exclude("foo/"), FilterRule::exclude("foo/")];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(!set.allows(Path::new("foo/bar"), false));
    }

    #[test]
    fn allows_checks_respect_directory_flag() {
        let set = FilterSet::from_rules([FilterRule::exclude("foo/")]).expect("compiled");
        assert!(!set.allows(Path::new("foo"), true));
        assert!(set.allows(Path::new("foo"), false));
    }

    #[test]
    fn include_rule_for_directory_restores_descendants() {
        let rules = [
            FilterRule::exclude("cache/"),
            FilterRule::include("cache/preserved/**"),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(set.allows(Path::new("cache/preserved/data"), false));
        assert!(!set.allows(Path::new("cache/tmp"), false));
    }

    #[test]
    fn relative_path_conversion_handles_dot_components() {
        let set = FilterSet::from_rules([FilterRule::exclude("foo/bar")]).expect("compiled");
        let mut path = PathBuf::from("foo");
        path.push("..");
        path.push("foo");
        path.push("bar");
        assert!(!set.allows(&path, false));
    }
}
