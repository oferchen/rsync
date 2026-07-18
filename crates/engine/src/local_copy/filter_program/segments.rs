//! Compiled filter segments and stacks that evaluate include/exclude,
//! protect/risk, and exclude-if-present rules sequentially with first-match
//! semantics, mirroring upstream `exclude.c` rule traversal.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use filters::{FilterAction, FilterRule};
use globset::{GlobBuilder, GlobMatcher};
use logging::debug_log;

/// Compiled list of rules evaluated sequentially.
#[derive(Clone, Debug, Default)]
pub(crate) struct FilterSegment {
    include_exclude: Vec<CompiledRule>,
    protect_risk: Vec<CompiledRule>,
}

impl FilterSegment {
    pub(crate) fn push_rule(&mut self, rule: FilterRule) -> Result<(), super::FilterProgramError> {
        match rule.action() {
            FilterAction::Include | FilterAction::Exclude => {
                self.include_exclude.push(CompiledRule::new(rule)?);
            }
            FilterAction::Protect | FilterAction::Risk => {
                self.protect_risk.push(CompiledRule::new(rule)?);
            }
            FilterAction::Clear => {
                debug_assert!(
                    false,
                    "clear directives should be converted into FilterProgramEntry::Clear before compilation",
                );
            }
            FilterAction::Merge | FilterAction::DirMerge => {
                // Merge and DirMerge are handled separately during filter program
                // construction, not pushed as regular rules to segments.
            }
        }
        Ok(())
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.include_exclude.is_empty() && self.protect_risk.is_empty()
    }

    /// Checks whether a directory path is excluded by a non-directory-specific
    /// sender-side rule in this segment.
    ///
    /// Returns `Some(true)` if excluded by a non-dir-only rule, `Some(false)` if
    /// excluded by a dir-only rule or included, and `None` if no rule matched.
    pub(crate) fn excluded_dir_by_non_dir_rule(&self, path: &Path) -> Option<bool> {
        for rule in &self.include_exclude {
            if rule.applies_to_sender && rule.matches(path, true, false) {
                if matches!(rule.action, FilterAction::Exclude) {
                    return Some(!rule.directory_only);
                }
                return Some(false);
            }
        }
        None
    }

    pub(crate) fn apply(
        &self,
        path: &Path,
        is_dir: bool,
        outcome: &mut FilterOutcome,
        context: FilterContext,
    ) {
        // upstream: exclude.c:rule_matches() has NO descendant matching at all.
        // The sender's traversal prunes excluded directories so children are
        // never examined; the receiver's deletion scan only enters dirs that
        // exist in the file list. Skipping descendants here mirrors upstream's
        // literal name-match semantics and prevents anchored patterns like
        // `- /bar` from over-matching deep paths (UTS-V3-B exclude-lsh).
        let check_descendants = false;
        // upstream: an excluded directory protects its descendants from
        // deletion because the generator never descends into it. On the
        // deletion scan a directory-only unanchored wildcard exclude (`foo/*/`)
        // therefore keeps `foo/sub/file1` off the delete pass via its
        // deletion-only descendant matchers; the Transfer path must NOT see
        // those (it would re-trip the #6015 `foo/*/` over-exclusion).
        let for_deletion = matches!(context, FilterContext::Deletion);
        // upstream: exclude.c:1044 check_filter() only skips a perishable rule
        // when `ignore_perishable` is set, which happens exclusively while
        // deleting the contents of a directory being wholly removed
        // (delete.c:147). The top-level delete scan runs with it unset, so a
        // perishable rule still matches and protects a candidate here; the
        // wholly-deleted-directory case never reaches this evaluator because
        // the local-copy delete pass removes such directories en masse.
        let rule_matches = |rule: &CompiledRule, path: &Path, is_dir: bool| {
            if for_deletion {
                rule.matches_for_deletion(path, is_dir, check_descendants)
            } else {
                rule.matches(path, is_dir, check_descendants)
            }
        };
        for rule in &self.include_exclude {
            if outcome.transfer_decided() {
                break;
            }
            if rule_matches(rule, path, is_dir) {
                if matches!(context, FilterContext::Deletion) && rule.applies_to_receiver {
                    outcome.set_delete_excluded(matches!(rule.action, FilterAction::Exclude));
                }
                match context {
                    FilterContext::Transfer => {
                        if rule.applies_to_sender {
                            report_filter_result(rule, path, is_dir, "sender");
                            outcome
                                .set_transfer_allowed(matches!(rule.action, FilterAction::Include));
                            outcome.decide_transfer();
                        }
                    }
                    FilterContext::Deletion => {
                        if rule.applies_to_receiver {
                            report_filter_result(rule, path, is_dir, "generator");
                            outcome
                                .set_transfer_allowed(matches!(rule.action, FilterAction::Include));
                            outcome.decide_transfer();
                        }
                    }
                }
            }
        }

        for rule in &self.protect_risk {
            // upstream check_filter() (exclude.c:1058-1061) returns on the first
            // matching rule, so the first protect/risk decision wins. Stop once a
            // protect/risk rule has applied rather than letting a later rule
            // overwrite it (last-match-wins).
            if outcome.protection_decided() {
                break;
            }
            if rule_matches(rule, path, is_dir) {
                let applies = match context {
                    FilterContext::Transfer => rule.applies_to_sender,
                    FilterContext::Deletion => rule.applies_to_receiver,
                };
                if applies {
                    let who = match context {
                        FilterContext::Transfer => "sender",
                        FilterContext::Deletion => "generator",
                    };
                    report_filter_result(rule, path, is_dir, who);
                    match rule.action {
                        FilterAction::Protect => outcome.protect(),
                        FilterAction::Risk => outcome.unprotect(),
                        FilterAction::Include | FilterAction::Exclude => {}
                        FilterAction::Clear => debug_assert!(
                            false,
                            "clear directives should be converted into FilterProgramEntry::Clear before compilation",
                        ),
                        FilterAction::Merge | FilterAction::DirMerge => {
                            // Merge and DirMerge rules should never appear in compiled
                            // protect_risk rules; they're processed during construction.
                        }
                    }
                }
            }
        }
    }
}

/// Short-form action prefix used in `add_rule()` debug lines, mirroring
/// upstream rsync's filter-rule syntax (`+`, `-`, `P`, `R`, `!`, `merge`, `dir-merge`).
fn filter_action_prefix(action: FilterAction) -> &'static str {
    match action {
        FilterAction::Include => "+",
        FilterAction::Exclude => "-",
        FilterAction::Protect => "P",
        FilterAction::Risk => "R",
        FilterAction::Clear => "!",
        FilterAction::Merge => "merge",
        FilterAction::DirMerge => "dir-merge",
    }
}

/// Emits a `--debug=FILTER` line for a rule that fired on `path`, naming the
/// file, its type, and the matching pattern.
///
/// upstream: exclude.c:report_filter_result() - `[who] {action}ing {type}
/// {name} because of pattern {pattern}{/}`. `debug_log!` gates on the FILTER
/// debug level internally, so the message is only formatted when enabled.
fn report_filter_result(rule: &CompiledRule, path: &Path, is_dir: bool, who: &str) {
    let verb = match rule.action {
        FilterAction::Include => "including",
        FilterAction::Exclude => "excluding",
        FilterAction::Protect => "protecting",
        FilterAction::Risk => "risking",
        _ => return,
    };
    let kind = if is_dir { "directory" } else { "file" };
    let slash = if rule.directory_only { "/" } else { "" };
    debug_log!(
        Filter,
        1,
        "[{who}] {verb} {kind} {} because of pattern {}{slash}",
        path.display(),
        rule.pattern,
    );
}

#[derive(Clone, Debug)]
pub(crate) enum FilterInstruction {
    Segment(FilterSegment),
    DirMerge { index: usize },
    ExcludeIfPresent { index: usize },
}

pub(crate) type FilterSegmentLayers = Vec<Vec<FilterSegment>>;
pub(crate) type FilterSegmentStack = Vec<Vec<(usize, FilterSegment)>>;
pub(crate) type ExcludeIfPresentLayers = Vec<Vec<super::ExcludeIfPresentRule>>;
pub(crate) type ExcludeIfPresentStack = Vec<Vec<(usize, Vec<super::ExcludeIfPresentRule>)>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FilterContext {
    Transfer,
    Deletion,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FilterOutcome {
    transfer_allowed: bool,
    transfer_decided: bool,
    protected: bool,
    protection_decided: bool,
    excluded_for_delete_excluded: bool,
}

impl FilterOutcome {
    const fn new() -> Self {
        Self {
            transfer_allowed: true,
            transfer_decided: false,
            protected: false,
            protection_decided: false,
            excluded_for_delete_excluded: false,
        }
    }

    pub(crate) const fn allows_transfer(self) -> bool {
        self.transfer_allowed
    }

    pub(crate) const fn allows_deletion(self) -> bool {
        self.transfer_allowed && !self.protected
    }

    pub(crate) const fn allows_deletion_when_excluded_removed(self) -> bool {
        self.excluded_for_delete_excluded && !self.protected
    }

    pub(crate) const fn transfer_decided(self) -> bool {
        self.transfer_decided
    }

    const fn protection_decided(self) -> bool {
        self.protection_decided
    }

    const fn decide_transfer(&mut self) {
        self.transfer_decided = true;
    }

    const fn set_transfer_allowed(&mut self, allowed: bool) {
        self.transfer_allowed = allowed;
    }

    const fn protect(&mut self) {
        self.protected = true;
        self.protection_decided = true;
    }

    const fn unprotect(&mut self) {
        self.protected = false;
        self.protection_decided = true;
    }

    const fn set_delete_excluded(&mut self, excluded: bool) {
        self.excluded_for_delete_excluded = excluded;
    }
}

impl Default for FilterOutcome {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
struct CompiledRule {
    action: FilterAction,
    /// The source pattern, retained for `--debug=FILTER` reporting
    /// (upstream: exclude.c:report_filter_result() logs `ent->pattern`).
    pattern: String,
    directory_only: bool,
    direct_matchers: Vec<GlobMatcher>,
    descendant_matchers: Vec<GlobMatcher>,
    /// Descendant matchers (`{core}/**`) consulted ONLY on the deletion path.
    /// Populated for directory-only unanchored wildcard excludes (`foo/*/`) so
    /// the receiver keeps `foo/sub/file1` off the delete pass while the
    /// Transfer path never sees `foo/*/**` (mirrors the filters-crate
    /// `CompiledRule`; preserves the #6015 `foo/*/` over-exclusion fix).
    deletion_descendant_matchers: Vec<GlobMatcher>,
    applies_to_sender: bool,
    applies_to_receiver: bool,
    /// upstream: exclude.c:906 - `ret_match = ex->rflags & FILTRULE_NEGATE ? 0 : 1`.
    /// When set, the rule fires on paths that do NOT match the pattern.
    negate: bool,
}

impl CompiledRule {
    fn new(rule: FilterRule) -> Result<Self, super::FilterProgramError> {
        let action = rule.action();
        let applies_to_sender = rule.applies_to_sender();
        let applies_to_receiver = rule.applies_to_receiver();
        let negate = rule.is_negated();
        let pattern = rule.pattern().to_owned();

        // upstream: exclude.c:add_rule() logs every parsed rule at
        // `DEBUG_GTE(FILTER, 2)` so the active rule set is observable.
        debug_log!(
            Filter,
            2,
            "add_rule({} {})",
            filter_action_prefix(action),
            pattern
        );

        let (anchored, directory_only, core_pattern) = normalise_pattern(&pattern);

        let mut direct_patterns = HashSet::new();
        direct_patterns.insert(core_pattern.clone());
        if !anchored {
            direct_patterns.insert(format!("**/{core_pattern}"));
        }

        let mut descendant_patterns = HashSet::new();
        // upstream: exclude.c - excluding a directory excludes its contents,
        // but including a directory does NOT include its contents (they must
        // match their own rules). Only Exclude/Protect/Risk get descendants.
        //
        // Anchored wildcard patterns (e.g., `/*`, `/*.txt`) must NOT generate
        // descendant matchers because `*/**` would incorrectly match nested
        // paths like `down/file.txt`. Traversal control on the sender side
        // handles directory exclusion for those cases.
        let has_glob_wildcard =
            core_pattern.contains('*') || core_pattern.contains('?') || core_pattern.contains('[');
        let slash_anchored = pattern.starts_with('/');
        let is_directory_only_unanchored_wildcard =
            directory_only && !slash_anchored && has_glob_wildcard;
        let is_anchored_wildcard = slash_anchored && has_glob_wildcard;
        let mut deletion_descendant_patterns = HashSet::new();
        if matches!(
            action,
            FilterAction::Exclude | FilterAction::Protect | FilterAction::Risk
        ) {
            if !(is_anchored_wildcard || is_directory_only_unanchored_wildcard) {
                descendant_patterns.insert(format!("{core_pattern}/**"));
                if !anchored {
                    descendant_patterns.insert(format!("**/{core_pattern}/**"));
                }
            } else if is_directory_only_unanchored_wildcard {
                // upstream: a directory-only unanchored wildcard (`foo/*/`)
                // emits no `foo/*/**` transfer rule (#6015); the sender walk
                // prunes the matched directory. The receiver's per-candidate
                // deletion scan has no such pruning, so route `{core}/**` into
                // a deletion-only set so excluded-directory children stay off
                // the delete pass without re-exposing `foo/*/**` to transfer.
                deletion_descendant_patterns.insert(format!("{core_pattern}/**"));
                if !anchored {
                    deletion_descendant_patterns.insert(format!("**/{core_pattern}/**"));
                }
            }
        }

        Ok(Self {
            action,
            directory_only,
            direct_matchers: compile_patterns(direct_patterns, &pattern)?,
            descendant_matchers: compile_patterns(descendant_patterns, &pattern)?,
            deletion_descendant_matchers: compile_patterns(deletion_descendant_patterns, &pattern)?,
            applies_to_sender,
            applies_to_receiver,
            negate,
            pattern,
        })
    }

    fn matches(&self, path: &Path, is_dir: bool, check_descendants: bool) -> bool {
        let pattern_matched = self.pattern_matches(path, is_dir, check_descendants);
        // upstream: exclude.c:906 - `ret_match = ex->rflags & FILTRULE_NEGATE ? 0 : 1`.
        // A negated rule fires when the pattern does NOT match.
        if self.negate {
            !pattern_matched
        } else {
            pattern_matched
        }
    }

    /// Like [`Self::matches`] but additionally consults the deletion-only
    /// descendant matchers. An excluded directory protects its descendants
    /// from deletion regardless of `check_descendants`, because the receiver
    /// scan evaluates each candidate in isolation with no traversal-pruning
    /// side effect.
    ///
    /// upstream: exclude.c:rule_matches() / name_is_excluded() subtree pruning
    fn matches_for_deletion(&self, path: &Path, is_dir: bool, check_descendants: bool) -> bool {
        let pattern_matched = self.pattern_matches(path, is_dir, check_descendants)
            || self
                .deletion_descendant_matchers
                .iter()
                .any(|matcher| matcher.is_match(path));
        if self.negate {
            !pattern_matched
        } else {
            pattern_matched
        }
    }

    fn pattern_matches(&self, path: &Path, is_dir: bool, check_descendants: bool) -> bool {
        for matcher in &self.direct_matchers {
            if matcher.is_match(path) && (!self.directory_only || is_dir) {
                return true;
            }
        }

        // upstream: exclude.c:rule_matches() does not expand patterns into
        // descendant matchers. Descendants are only consulted when a caller
        // explicitly opts in.
        if check_descendants {
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
) -> Result<Vec<GlobMatcher>, super::FilterProgramError> {
    // upstream: lib/wildmatch.c:dowild() - bare `**` always matches across
    // `/`. globset's `literal_separator(true)` only treats `**` as recursive
    // when bounded by `/`, so each pattern expands into two variants: the
    // original (covers in-segment `*`-like behaviour) and a slash-bounded
    // rewrite (covers cross-segment matches). Both are added so either form
    // can satisfy upstream parity.
    let mut expanded: HashSet<String> = HashSet::with_capacity(patterns.len() * 2);
    for pattern in patterns {
        if let Cow::Owned(rewritten) = normalise_recursive_wildcards(&pattern) {
            expanded.insert(rewritten);
        }
        expanded.insert(pattern);
    }

    let mut unique: Vec<_> = expanded.into_iter().collect();
    unique.sort();

    let mut matchers = Vec::with_capacity(unique.len());
    for pattern in unique {
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| super::FilterProgramError::new(original.to_owned(), error))?;
        matchers.push(glob.compile_matcher());
    }

    Ok(matchers)
}

/// Rewrites bare interior `**` sequences into slash-delimited `/**/` so
/// globset treats them as recursive wildcards.
///
/// upstream: `lib/wildmatch.c:dowild()` - when `**` is encountered, the
/// `special` flag is set and the wildcard matches across `/` boundaries
/// regardless of surrounding characters. A pattern like `foo**too` must be
/// rewritten to `foo/**/too` so globset matches `bar/down/to/foo/too`.
///
/// Runs of three or more `*` characters collapse to `**` first, matching
/// upstream's `while (*++p == '*') {}` consumption.
fn normalise_recursive_wildcards(pattern: &str) -> Cow<'_, str> {
    if !pattern.contains("**") {
        return Cow::Borrowed(pattern);
    }

    let bytes = pattern.as_bytes();
    let mut out = String::with_capacity(bytes.len() + 4);
    let mut cut = 0;
    let mut i = 0;
    let mut changed = false;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let run_start = i;
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] == b'*' {
                j += 1;
            }

            out.push_str(&pattern[cut..run_start]);

            if j - run_start > 2 {
                changed = true;
            }
            let at_start = run_start == 0;
            let at_end = j == bytes.len();
            let prev_is_slash = run_start > 0 && bytes[run_start - 1] == b'/';
            let next_is_slash = j < bytes.len() && bytes[j] == b'/';

            let need_leading_slash = !at_start && !prev_is_slash;
            let need_trailing_slash = !at_end && !next_is_slash;

            if need_leading_slash {
                out.push('/');
                changed = true;
            }
            out.push_str("**");
            if need_trailing_slash {
                out.push('/');
                changed = true;
            }
            i = j;
            cut = j;
            continue;
        }
        i += 1;
    }

    if !changed {
        return Cow::Borrowed(pattern);
    }

    out.push_str(&pattern[cut..]);
    Cow::Owned(out)
}

/// Normalizes a pattern by stripping leading `/` (anchored) and trailing `/`
/// or `/***` (directory-only).
///
/// upstream: `exclude.c:rule_matches()` - `FILTRULE_ABS_PATH` is set only for
/// patterns starting with `/`. A pattern with internal slashes but no leading
/// `/` is NOT anchored; upstream tail-matches it against the last N+1 path
/// components. The glob equivalent is `**/pattern`, which the caller adds for
/// unanchored patterns.
fn normalise_pattern(pattern: &str) -> (bool, bool, String) {
    let starts_with_slash = pattern.starts_with('/');

    // upstream: exclude.c:243-248 - a trailing `/***` (SLASH_WILD3_SUFFIX)
    // means "match both the directory and everything inside it". Normalize
    // by stripping `/***` and treating the result as directory-only.
    let (stripped, directory_only) = if pattern.ends_with("/***") && pattern.len() > 4 {
        (&pattern[..pattern.len() - 4], true)
    } else if pattern.ends_with('/') && pattern.len() > 1 {
        (&pattern[..pattern.len() - 1], true)
    } else if pattern == "/" {
        (pattern, true)
    } else {
        (pattern, false)
    };

    let core_pattern = if starts_with_slash {
        stripped.strip_prefix('/').unwrap_or(stripped)
    } else {
        stripped
    };

    (starts_with_slash, directory_only, core_pattern.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_pattern_basic() {
        let (anchored, directory_only, core) = normalise_pattern("*.txt");
        assert!(!anchored);
        assert!(!directory_only);
        assert_eq!(core, "*.txt");
    }

    #[test]
    fn normalise_pattern_anchored() {
        let (anchored, directory_only, core) = normalise_pattern("/foo");
        assert!(anchored);
        assert!(!directory_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_directory_only() {
        let (anchored, directory_only, core) = normalise_pattern("bar/");
        assert!(!anchored);
        assert!(directory_only);
        assert_eq!(core, "bar");
    }

    #[test]
    fn normalise_pattern_anchored_and_directory() {
        let (anchored, directory_only, core) = normalise_pattern("/baz/");
        assert!(anchored);
        assert!(directory_only);
        assert_eq!(core, "baz");
    }

    #[test]
    fn filter_outcome_default() {
        let outcome = FilterOutcome::default();
        assert!(outcome.allows_transfer());
        assert!(outcome.allows_deletion());
    }

    #[test]
    fn filter_outcome_transfer_not_allowed() {
        let mut outcome = FilterOutcome::default();
        outcome.set_transfer_allowed(false);
        assert!(!outcome.allows_transfer());
        assert!(!outcome.allows_deletion());
    }

    #[test]
    fn filter_outcome_protected() {
        let mut outcome = FilterOutcome::default();
        outcome.protect();
        assert!(outcome.allows_transfer());
        assert!(!outcome.allows_deletion());
    }

    #[test]
    fn filter_outcome_unprotect() {
        let mut outcome = FilterOutcome::default();
        outcome.protect();
        outcome.unprotect();
        assert!(outcome.allows_transfer());
        assert!(outcome.allows_deletion());
    }

    #[test]
    fn filter_segment_is_empty() {
        let segment = FilterSegment::default();
        assert!(segment.is_empty());
    }

    #[test]
    fn filter_segment_push_include() {
        let mut segment = FilterSegment::default();
        segment
            .push_rule(FilterRule::include("*.txt".to_owned()))
            .unwrap();
        assert!(!segment.is_empty());
    }

    #[test]
    fn filter_segment_push_exclude() {
        let mut segment = FilterSegment::default();
        segment
            .push_rule(FilterRule::exclude("*.bak".to_owned()))
            .unwrap();
        assert!(!segment.is_empty());
    }

    #[test]
    fn filter_segment_push_protect() {
        let mut segment = FilterSegment::default();
        segment
            .push_rule(FilterRule::protect("important/".to_owned()))
            .unwrap();
        assert!(!segment.is_empty());
    }

    /// Verifies `--include '*/'` does not match files inside directories.
    ///
    /// upstream: Including a directory means "include the directory entry" -
    /// it does NOT mean "include everything inside it". Files inside must
    /// match their own rules. Only Exclude/Protect/Risk get descendants.
    #[test]
    fn include_directory_only_no_descendant_match() {
        let rule = CompiledRule::new(FilterRule::include("*/".to_owned())).unwrap();
        assert!(rule.matches(Path::new("subdir"), true, true));
        // Files inside an included directory must still match a separate rule;
        // include never expands to descendants. See `CompiledRule::new`.
        assert!(!rule.matches(Path::new("file.txt"), false, true));
        assert!(!rule.matches(Path::new("subdir/debug.log"), false, true));
        assert!(!rule.matches(Path::new("subdir/report.csv"), false, true));
    }

    /// `--exclude '*/'` (a directory-only unanchored wildcard) routes its
    /// `*/**` descendant into the deletion-only set, mirroring upstream
    /// `exclude.c:rule_matches()`: the transfer path emits no `*/**` rule (the
    /// sender walk prunes the matched directory, #6015), while the receiver
    /// deletion scan must protect children of the excluded directory from
    /// over-deletion (`exclude` / `exclude-lsh` regression).
    #[test]
    fn exclude_directory_only_descendant_gated_by_context() {
        let rule = CompiledRule::new(FilterRule::exclude("*/".to_owned())).unwrap();
        // Directory entry matches directly in both contexts.
        assert!(rule.matches(Path::new("subdir"), true, false));
        // Transfer path never matches a child via a synthetic descendant, even
        // with check_descendants requested (#6015 guard).
        assert!(!rule.matches(Path::new("subdir/debug.log"), false, true));
        assert!(!rule.matches(Path::new("subdir/debug.log"), false, false));
        // Deletion path protects the child regardless of check_descendants.
        assert!(rule.matches_for_deletion(Path::new("subdir/debug.log"), false, false));
        assert!(rule.matches_for_deletion(Path::new("subdir/debug.log"), false, true));
    }

    /// Verifies patterns with internal `/` but no leading `/` are NOT
    /// anchored - upstream sets `FILTRULE_ABS_PATH` only for a leading `/`.
    /// Internal slashes drive tail-matching via the `**/pattern` direct
    /// variant added in `CompiledRule::new`.
    #[test]
    fn normalise_pattern_internal_slash_is_unanchored() {
        let (anchored, directory_only, core) = normalise_pattern("src/lib/");
        assert!(!anchored);
        assert!(directory_only);
        assert_eq!(core, "src/lib");
    }

    /// `- /bar` is anchored and must NOT match `bar/down` (or deeper paths)
    /// through `FilterSegment::apply`. The sender's traversal handles the
    /// directory-exclusion side-effect; the per-entry filter path mirrors
    /// upstream `rule_matches()` literal-only matching. Regression coverage
    /// for the UTS-V3-B exclude-lsh failure.
    #[test]
    fn anchored_exclude_does_not_overmatch_children_via_apply() {
        let mut segment = FilterSegment::default();
        segment
            .push_rule(FilterRule::exclude("/bar".to_owned()))
            .unwrap();

        let mut bar_outcome = FilterOutcome::default();
        segment.apply(
            Path::new("bar"),
            true,
            &mut bar_outcome,
            FilterContext::Transfer,
        );
        assert!(!bar_outcome.allows_transfer(), "the dir `bar` is excluded");

        let mut child_outcome = FilterOutcome::default();
        segment.apply(
            Path::new("bar/down"),
            true,
            &mut child_outcome,
            FilterContext::Transfer,
        );
        assert!(
            child_outcome.allows_transfer(),
            "`bar/down` must not be over-matched via descendant expansion"
        );

        let mut deep_outcome = FilterOutcome::default();
        segment.apply(
            Path::new("bar/down/to/foo/file1"),
            false,
            &mut deep_outcome,
            FilterContext::Transfer,
        );
        assert!(
            deep_outcome.allows_transfer(),
            "deep paths under `bar/` must not be over-matched"
        );
    }

    /// `- foo/*/` is unanchored despite the internal slash. After the
    /// anchoring fix it gains a `**/foo/*` direct matcher so `mid/for/foo/and`
    /// matches via tail anchor and the receiver's `--delete-excluded` path
    /// removes the dest entry. Regression coverage for UTS-V3-B exclude-lsh.
    #[test]
    fn unanchored_internal_slash_matches_via_tail() {
        let mut segment = FilterSegment::default();
        segment
            .push_rule(FilterRule::exclude("foo/*/".to_owned()))
            .unwrap();

        let mut outcome = FilterOutcome::default();
        segment.apply(
            Path::new("mid/for/foo/and"),
            true,
            &mut outcome,
            FilterContext::Deletion,
        );
        assert!(
            !outcome.allows_transfer(),
            "`mid/for/foo/and` must match `- foo/*/` via tail anchor `**/foo/*`"
        );
        assert!(
            outcome.allows_deletion_when_excluded_removed(),
            "matching exclude rule must enable --delete-excluded removal"
        );
    }

    /// Regression for the `exclude` / `exclude-lsh` over-deletion: under
    /// `--delete-during` a directory-only unanchored wildcard exclude
    /// (`- foo/*/`) must protect the children of the matched directory from
    /// deletion. `FilterSegment::apply` calls with `check_descendants = false`,
    /// so the protection rides on the deletion-only descendant matchers. The
    /// transfer evaluation of the same child must stay "allowed" so #6015's
    /// `foo/*/` over-exclusion does not return.
    #[test]
    fn deletion_dir_only_wildcard_protects_children() {
        let mut segment = FilterSegment::default();
        segment
            .push_rule(FilterRule::exclude("foo/*/".to_owned()))
            .unwrap();

        let child = Path::new("foo/sub/file1");

        let mut del = FilterOutcome::default();
        segment.apply(child, false, &mut del, FilterContext::Deletion);
        assert!(
            !del.allows_deletion(),
            "`foo/sub/file1` must be protected from deletion by `- foo/*/`",
        );

        // The transfer path must NOT exclude the child via a synthetic
        // descendant - the sender walk prunes the directory instead (#6015).
        let mut xfer = FilterOutcome::default();
        segment.apply(child, false, &mut xfer, FilterContext::Transfer);
        assert!(
            xfer.allows_transfer(),
            "`foo/sub/file1` must not be excluded on the transfer path (#6015)",
        );
    }

    /// `+ foo**too` must match cross-segment paths after recursive wildcard
    /// normalisation, preserving UTS-20 parity inside the filter program
    /// compilation path.
    #[test]
    fn bare_double_star_matches_across_segments() {
        let rule = CompiledRule::new(FilterRule::include("foo**too".to_owned())).unwrap();
        assert!(rule.matches(Path::new("bar/down/to/foo/too"), true, false));
        assert!(rule.matches(Path::new("foo/too"), true, false));
        assert!(rule.matches(Path::new("fooxytoo"), false, false));
        assert!(!rule.matches(Path::new("foo/bar"), false, false));
    }

    #[test]
    fn filter_context_eq() {
        assert_eq!(FilterContext::Transfer, FilterContext::Transfer);
        assert_eq!(FilterContext::Deletion, FilterContext::Deletion);
        assert_ne!(FilterContext::Transfer, FilterContext::Deletion);
    }
}
