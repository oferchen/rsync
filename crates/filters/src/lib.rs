#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_filters` provides ordered include/exclude/protect pattern evaluation for the
//! Rust `rsync` workspace. The implementation focuses on reproducing the
//! subset of rsync's filter grammar that governs `--include`/`--exclude`
//! handling for local filesystem transfers. Patterns honour anchored matches
//! (leading `/`), directory-only rules (trailing `/`), and recursive wildcards
//! using the same glob semantics exposed by upstream rsync. Rules are evaluated
//! sequentially with the last matching include/exclude directive determining
//! whether a path is copied. `protect` directives accumulate alongside these
//! rules to prevent matching destination paths from being removed during
//! `--delete` sweeps.
//!
//! # Design
//!
//! - [`FilterRule`] captures the user-supplied action (`Include`/`Exclude`/
//!   `Protect`) and pattern text. The rule itself is lightweight; heavy lifting
//!   pattern text. The rule itself is lightweight; heavy lifting happens when a
//!   [`FilterSet`] is constructed.
//! - [`FilterSet`] owns the compiled representation of each rule, expanding
//!   directory-only patterns into matchers that also cover their contents while
//!   deduplicating equivalent glob expressions. Protect rules are tracked in a
//!   dedicated list so deletion checks can honour them without affecting copy
//!   decisions.
//! - Matching occurs against relative paths using native [`Path`] semantics so
//!   callers can operate directly on `std::path::PathBuf` instances without
//!   additional conversions.
//!
//! # Invariants
//!
//! - Include/exclude rules are applied in definition order. The last matching
//!   rule wins and defaults to `Include` when no rule matches.
//! - Trailing `/` marks a directory-only rule. The directory itself must match
//!   the rule to trigger exclusion; descendants are excluded automatically.
//! - Leading `/` anchors a rule to the transfer root. Patterns without a leading
//!   slash are matched at any depth by implicitly prefixing `**/`.
//! - Protect rules accumulate independently of include/exclude decisions and
//!   prevent matching destination paths from being removed when `--delete` is
//!   active.
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
//! - `rsync_engine::local_copy` integrates [`FilterSet`] to prune directory
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
    /// Protect the matching path from deletion while leaving transfer decisions unchanged.
    Protect,
    /// Remove previously applied protection, allowing deletion when matched.
    Risk,
    /// Clear previously defined filter rules for the affected transfer sides.
    Clear,
}

/// User-visible filter rule consisting of an action and pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterRule {
    action: FilterAction,
    pattern: String,
    applies_to_sender: bool,
    applies_to_receiver: bool,
}

impl FilterRule {
    /// Creates an include rule for `pattern`.
    #[must_use]
    pub fn include(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Include,
            pattern: pattern.into(),
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates an exclude rule for `pattern`.
    #[must_use]
    pub fn exclude(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Exclude,
            pattern: pattern.into(),
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates a protect rule for `pattern`.
    #[must_use]
    pub fn protect(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Protect,
            pattern: pattern.into(),
            applies_to_sender: false,
            applies_to_receiver: true,
        }
    }

    /// Creates a risk rule for `pattern`.
    #[must_use]
    pub fn risk(pattern: impl Into<String>) -> Self {
        Self {
            action: FilterAction::Risk,
            pattern: pattern.into(),
            applies_to_sender: false,
            applies_to_receiver: true,
        }
    }

    /// Clears all previously configured rules for the applicable transfer sides.
    #[must_use]
    #[doc(alias = "!")]
    pub fn clear() -> Self {
        Self {
            action: FilterAction::Clear,
            pattern: String::new(),
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates a sender-only include rule equivalent to `show PATTERN`.
    ///
    /// # Examples
    /// ```
    /// use rsync_filters::FilterRule;
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
        }
    }

    /// Creates a sender-only exclude rule equivalent to `hide PATTERN`.
    ///
    /// # Examples
    /// ```
    /// use rsync_filters::FilterRule;
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

    /// Anchors the pattern to the root of the transfer if it is not already.
    #[must_use]
    pub fn anchor_to_root(mut self) -> Self {
        if !self.pattern.starts_with('/') {
            self.pattern.insert(0, '/');
        }
        self
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
        let mut include_exclude = Vec::new();
        let mut protect_risk = Vec::new();

        for rule in rules.into_iter() {
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
            }
        }

        Ok(Self {
            inner: Arc::new(FilterSetInner {
                include_exclude,
                protect_risk,
            }),
        })
    }

    /// Reports whether the set contains any rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.include_exclude.is_empty() && self.inner.protect_risk.is_empty()
    }

    /// Determines whether the provided path is allowed.
    #[must_use]
    pub fn allows(&self, path: &Path, is_dir: bool) -> bool {
        self.inner
            .decision(path, is_dir, DecisionContext::Transfer)
            .allows_transfer()
    }

    /// Determines whether deleting the provided path is permitted.
    ///
    /// Protect directives prevent deletion regardless of the include/exclude
    /// decision, matching upstream `--filter 'protect …'` semantics.
    #[must_use]
    pub fn allows_deletion(&self, path: &Path, is_dir: bool) -> bool {
        self.inner
            .decision(path, is_dir, DecisionContext::Deletion)
            .allows_deletion()
    }

    /// Determines whether the path may be removed when excluded entries are purged.
    #[must_use]
    pub fn allows_deletion_when_excluded_removed(&self, path: &Path, is_dir: bool) -> bool {
        self.inner
            .decision(path, is_dir, DecisionContext::Deletion)
            .allows_deletion_when_excluded_removed()
    }
}

#[derive(Debug, Default)]
struct FilterSetInner {
    include_exclude: Vec<CompiledRule>,
    protect_risk: Vec<CompiledRule>,
}

fn last_matching_rule<'a, F>(
    rules: &'a [CompiledRule],
    path: &Path,
    is_dir: bool,
    mut applies: F,
) -> Option<&'a CompiledRule>
where
    F: FnMut(&CompiledRule) -> bool,
{
    rules
        .iter()
        .rev()
        .find(|rule| applies(rule) && rule.matches(path, is_dir))
}

impl FilterSetInner {
    fn decision(&self, path: &Path, is_dir: bool, context: DecisionContext) -> FilterDecision {
        let mut decision = FilterDecision::default();

        let transfer_rule = match context {
            DecisionContext::Transfer => {
                last_matching_rule(&self.include_exclude, path, is_dir, |rule| {
                    rule.applies_to_sender
                })
            }
            DecisionContext::Deletion => {
                last_matching_rule(&self.include_exclude, path, is_dir, |rule| {
                    rule.applies_to_receiver
                })
            }
        };

        if let Some(rule) = transfer_rule {
            decision.transfer_allowed = matches!(rule.action, FilterAction::Include);
        }

        let protection_rule = match context {
            DecisionContext::Transfer => {
                last_matching_rule(&self.protect_risk, path, is_dir, |rule| {
                    rule.applies_to_sender
                })
            }
            DecisionContext::Deletion => {
                last_matching_rule(&self.protect_risk, path, is_dir, |rule| {
                    rule.applies_to_receiver
                })
            }
        };

        if let Some(rule) = protection_rule {
            match rule.action {
                FilterAction::Protect => decision.protect(),
                FilterAction::Risk => decision.unprotect(),
                FilterAction::Include | FilterAction::Exclude | FilterAction::Clear => {}
            }
        }

        decision
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DecisionContext {
    Transfer,
    Deletion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FilterDecision {
    transfer_allowed: bool,
    protected: bool,
}

impl FilterDecision {
    const fn allows_transfer(self) -> bool {
        self.transfer_allowed
    }

    const fn allows_deletion(self) -> bool {
        self.transfer_allowed && !self.protected
    }

    const fn allows_deletion_when_excluded_removed(self) -> bool {
        !self.transfer_allowed && !self.protected
    }

    fn protect(&mut self) {
        self.protected = true;
    }

    fn unprotect(&mut self) {
        self.protected = false;
    }
}

impl Default for FilterDecision {
    fn default() -> Self {
        Self {
            transfer_allowed: true,
            protected: false,
        }
    }
}

#[derive(Debug)]
struct CompiledRule {
    action: FilterAction,
    directory_only: bool,
    direct_matchers: Vec<GlobMatcher>,
    descendant_matchers: Vec<GlobMatcher>,
    applies_to_sender: bool,
    applies_to_receiver: bool,
}

impl CompiledRule {
    fn new(rule: FilterRule) -> Result<Self, FilterError> {
        let FilterRule {
            action,
            pattern,
            applies_to_sender,
            applies_to_receiver,
        } = rule;
        let (anchored, directory_only, core_pattern) = normalise_pattern(&pattern);
        let mut direct_patterns = HashSet::new();
        direct_patterns.insert(core_pattern.clone());
        if !anchored {
            direct_patterns.insert(format!("**/{}", core_pattern));
        }

        let mut descendant_patterns = HashSet::new();
        if directory_only
            || matches!(
                action,
                FilterAction::Exclude | FilterAction::Protect | FilterAction::Risk
            )
        {
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
            applies_to_sender,
            applies_to_receiver,
        })
    }

    fn matches(&self, path: &Path, is_dir: bool) -> bool {
        for matcher in &self.direct_matchers {
            if matcher.is_match(path) && (!self.directory_only || is_dir) {
                return true;
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

    fn clear_sides(&mut self, sender: bool, receiver: bool) -> bool {
        if sender {
            self.applies_to_sender = false;
        }
        if receiver {
            self.applies_to_receiver = false;
        }
        self.applies_to_sender || self.applies_to_receiver
    }
}

fn apply_clear_rule(rules: &mut Vec<CompiledRule>, sender: bool, receiver: bool) {
    if !sender && !receiver {
        return;
    }

    rules.retain_mut(|rule| rule.clear_sides(sender, receiver));
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
        assert!(set.allows_deletion(Path::new("foo"), false));
    }

    #[test]
    fn include_rule_allows_path() {
        let set = FilterSet::from_rules([FilterRule::include("foo")]).expect("compiled");
        assert!(set.allows(Path::new("foo"), false));
        assert!(set.allows_deletion(Path::new("foo"), false));
    }

    #[test]
    fn exclude_rule_blocks_match() {
        let set = FilterSet::from_rules([FilterRule::exclude("foo")]).expect("compiled");
        assert!(!set.allows(Path::new("foo"), false));
        assert!(!set.allows(Path::new("bar/foo"), false));
        assert!(!set.allows_deletion(Path::new("foo"), false));
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
        assert!(set.allows_deletion(Path::new("foo/bar.txt"), false));
    }

    #[test]
    fn clear_rule_removes_previous_rules() {
        let rules = [
            FilterRule::exclude("*.tmp"),
            FilterRule::protect("secrets/"),
            FilterRule::clear(),
            FilterRule::include("*.tmp"),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(set.allows(Path::new("scratch.tmp"), false));
        assert!(set.allows_deletion(Path::new("scratch.tmp"), false));
        assert!(set.allows_deletion(Path::new("secrets/data"), false));
    }

    #[test]
    fn clear_rule_respects_side_flags() {
        let rules = [
            FilterRule::exclude("sender.txt").with_sides(true, false),
            FilterRule::exclude("receiver.txt").with_sides(false, true),
            FilterRule::clear().with_sides(true, false),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");

        // Sender-side rule cleared, so transfers allow the path again.
        assert!(set.allows(Path::new("sender.txt"), false));

        // Receiver-side rule remains active and prevents deletion.
        assert!(!set.allows_deletion(Path::new("receiver.txt"), false));
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
        assert!(!set.allows_deletion(Path::new("build/output.bin"), false));
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

    #[test]
    fn protect_rule_blocks_deletion_without_affecting_transfer() {
        let set = FilterSet::from_rules([FilterRule::protect("*.bak")]).expect("compiled");
        assert!(set.allows(Path::new("keep.bak"), false));
        assert!(!set.allows_deletion(Path::new("keep.bak"), false));
    }

    #[test]
    fn protect_rule_applies_to_directory_descendants() {
        let set = FilterSet::from_rules([FilterRule::protect("secrets/")]).expect("compiled");
        assert!(set.allows(Path::new("secrets/data.txt"), false));
        assert!(!set.allows_deletion(Path::new("secrets/data.txt"), false));
        assert!(!set.allows_deletion(Path::new("dir/secrets/data.txt"), false));
    }

    #[test]
    fn risk_rule_allows_deletion_after_protection() {
        let rules = [
            FilterRule::protect("archive/"),
            FilterRule::risk("archive/"),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(set.allows_deletion(Path::new("archive/file.bin"), false));
    }

    #[test]
    fn risk_rule_applies_to_descendants() {
        let rules = [FilterRule::protect("backup/"), FilterRule::risk("backup/")];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(set.allows_deletion(Path::new("backup/snap/info"), false));
        assert!(set.allows_deletion(Path::new("sub/backup/snap"), true));
    }

    #[test]
    fn delete_excluded_only_removes_excluded_matches() {
        let rules = [FilterRule::include("keep/**"), FilterRule::exclude("*.tmp")];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(set.allows_deletion_when_excluded_removed(Path::new("skip.tmp"), false));
        assert!(!set.allows_deletion_when_excluded_removed(Path::new("keep/file.txt"), false));
    }

    #[test]
    fn sender_only_rule_does_not_prevent_deletion() {
        let rules = [FilterRule::exclude("skip.txt").with_sides(true, false)];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(!set.allows(Path::new("skip.txt"), false));
        assert!(set.allows_deletion(Path::new("skip.txt"), false));
    }

    #[test]
    fn receiver_only_rule_blocks_deletion_without_hiding() {
        let rules = [FilterRule::exclude("keep.txt").with_sides(false, true)];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(set.allows(Path::new("keep.txt"), false));
        assert!(!set.allows_deletion(Path::new("keep.txt"), false));
    }

    #[test]
    fn show_rule_applies_only_to_sender() {
        let set = FilterSet::from_rules([FilterRule::show("visible/**")]).expect("compiled");
        assert!(set.allows(Path::new("visible/file.txt"), false));
        assert!(set.allows_deletion(Path::new("visible/file.txt"), false));
    }

    #[test]
    fn hide_rule_applies_only_to_sender() {
        let set = FilterSet::from_rules([FilterRule::hide("hidden/**")]).expect("compiled");
        assert!(!set.allows(Path::new("hidden/file.txt"), false));
        assert!(set.allows_deletion(Path::new("hidden/file.txt"), false));
    }

    #[test]
    fn receiver_context_skips_sender_only_tail_rule() {
        let rules = [
            FilterRule::exclude("*.tmp").with_sides(false, true),
            FilterRule::include("*.tmp").with_sides(true, false),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(!set.allows_deletion(Path::new("note.tmp"), false));
    }

    #[test]
    fn sender_only_risk_does_not_clear_receiver_protection() {
        let rules = [
            FilterRule::protect("keep/"),
            FilterRule::risk("keep/").with_sides(true, false),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");
        assert!(!set.allows_deletion(Path::new("keep/item.txt"), false));
    }
}
