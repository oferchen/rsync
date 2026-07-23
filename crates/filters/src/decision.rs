use std::path::Path;

use logging::debug_log;

use crate::{
    FilterAction,
    compiled::{CompiledRule, CompiledXattrRule},
};

/// Internal rule storage shared by [`FilterSet`](crate::FilterSet) instances.
///
/// Maintains three independent rule chains following the Chain of
/// Responsibility pattern. Each chain is evaluated with first-match-wins
/// semantics, mirroring upstream rsync's `check_filter()` in `exclude.c`.
///
/// - `include_exclude` / `protect_risk` govern path (file) decisions.
/// - `xattr` holds `x`-modifier rules that govern xattr-name decisions only,
///   kept separate because upstream `exclude.c:914` never matches an
///   `x`-modifier rule against a path nor an ordinary rule against an xattr
///   name.
#[derive(Debug, Default)]
pub(crate) struct FilterSetInner {
    pub(crate) include_exclude: Vec<CompiledRule>,
    pub(crate) protect_risk: Vec<CompiledRule>,
    pub(crate) xattr: Vec<CompiledXattrRule>,
}

impl FilterSetInner {
    /// Resolves whether an xattr `name` is allowed by the `x`-modifier rules.
    ///
    /// Evaluates the xattr chain first-match-wins and returns the matching
    /// rule's include/exclude verdict; with no match the name is included by
    /// default. Mirrors upstream `exclude.c:name_is_excluded(name,
    /// NAME_IS_XATTR, ALL_FILTERS)` consulted from `xattrs.c:250`.
    pub(crate) fn xattr_name_allowed(&self, name: &str) -> bool {
        for rule in &self.xattr {
            if rule.matches(name) {
                return matches!(rule.action(), FilterAction::Include);
            }
        }
        true
    }
}

impl FilterSetInner {
    /// Evaluates a path against both the include/exclude and protect/risk
    /// chains, returning a composite decision.
    ///
    /// The evaluation context determines which side's rules are consulted:
    /// - `Transfer` checks sender-side include/exclude rules.
    /// - `Deletion` checks receiver-side rules. Perishable rules are honoured
    ///   here because the top-level delete scan runs with `ignore_perishable`
    ///   unset (upstream delete.c:147); they are only skipped inside a
    ///   directory being wholly removed, which the receiver deletes en masse
    ///   without re-filtering.
    ///
    /// upstream: exclude.c:check_filter()
    pub(crate) fn decision(
        &self,
        path: &Path,
        is_dir: bool,
        context: DecisionContext,
    ) -> FilterDecision {
        self.decision_with_traversal(path, is_dir, context, false)
    }

    /// Like [`Self::decision`] but lets callers signal that the query comes
    /// from a tree traversal that already prunes excluded subtrees.
    ///
    /// When `traversal` is `true`, synthetic descendant matchers (the
    /// `pattern/**` matcher pre-compiled for anchored excludes like
    /// `- /bar`) are skipped because the traversal itself handles descendant
    /// exclusion - this mirrors upstream's `exclude.c:rule_matches()` which
    /// has NO descendant matching at all. When `false`, descendants stay
    /// active so single-path API callers (e.g. `set.allows("build/x.bin")`
    /// after `- build/`) still see the expected exclusion without walking
    /// the tree.
    ///
    /// upstream: exclude.c:rule_matches()
    pub(crate) fn decision_with_traversal(
        &self,
        path: &Path,
        is_dir: bool,
        context: DecisionContext,
        traversal: bool,
    ) -> FilterDecision {
        let mut decision = FilterDecision::default();

        // upstream: exclude.c:rule_matches() has NO descendant matching.
        // Sender-side (Transfer) during a directory walk skips the synthetic
        // `pattern/**` descendant matchers because the traversal itself
        // implicitly handles them by not descending into excluded directories.
        // Single-path API queries (no traversal context) keep descendants
        // active so callers can still see "build/output.bin" as excluded
        // by a `- build/` rule without walking the tree themselves.
        // Receiver-side (Deletion) during a per-directory chain commit
        // suppresses descendants for the same reason: the chain has
        // already routed the path to the responsible scope via the
        // descendant-free `has_matching_rule` predicate, so re-enabling
        // `pattern/**` here would let a per-dir rule like `bar` in
        // `./foo/.cvsignore` fire against the sibling subtree `./bar/x`.
        let check_descendants = !traversal;

        let for_deletion = matches!(context, DecisionContext::Deletion);
        let transfer_rule = match context {
            // Single-path Transfer queries (`check_descendants == true`, no
            // traversal context) keep the per-path predicate but gate the
            // synthetic `{core}/**` descendant matchers so they honour
            // first-match-wins at the pruned ancestor. A directory-exclude only
            // prunes a descendant when the excluded directory is itself the
            // first match; an *earlier* include that keeps that directory means
            // upstream descends into it, so the descendant must not fire. The
            // sender's own traversal (`traversal == true`,
            // `check_descendants == false`) needs no gating - the walk prunes
            // excluded subtrees directly.
            DecisionContext::Transfer if check_descendants => transfer_rule_with_pruning(
                &self.include_exclude,
                path,
                is_dir,
                |rule| rule.applies_to_sender,
                true,
            ),
            DecisionContext::Transfer => first_matching_rule(
                &self.include_exclude,
                path,
                is_dir,
                |rule| rule.applies_to_sender,
                true,
                check_descendants,
                false,
            ),
            DecisionContext::Deletion => first_matching_rule(
                &self.include_exclude,
                path,
                is_dir,
                |rule| rule.applies_to_receiver,
                true,
                check_descendants,
                true,
            ),
        };

        if matches!(context, DecisionContext::Deletion)
            && let Some(rule) = first_matching_rule(
                &self.include_exclude,
                path,
                is_dir,
                |rule| rule.applies_to_receiver,
                true,
                check_descendants,
                for_deletion,
            )
        {
            decision.excluded_for_delete_excluded = matches!(rule.action, FilterAction::Exclude);
        }

        if let Some(rule) = transfer_rule {
            let allowed = matches!(rule.action, FilterAction::Include);
            decision.transfer_allowed = allowed;

            // upstream: exclude.c:report_filter_result() names the matched
            // pattern in the `--debug=FILTER` line.
            if allowed {
                debug_log!(
                    Filter,
                    1,
                    "including {:?} because of pattern {}",
                    path,
                    rule.pattern
                );
            } else {
                debug_log!(
                    Filter,
                    1,
                    "excluding {:?} because of pattern {}",
                    path,
                    rule.pattern
                );
            }
        }

        let protection_rule = match context {
            DecisionContext::Transfer => first_matching_rule(
                &self.protect_risk,
                path,
                is_dir,
                |rule| rule.applies_to_sender,
                true,
                check_descendants,
                false,
            ),
            DecisionContext::Deletion => first_matching_rule(
                &self.protect_risk,
                path,
                is_dir,
                |rule| rule.applies_to_receiver,
                true,
                check_descendants,
                true,
            ),
        };

        if let Some(rule) = protection_rule {
            match rule.action {
                FilterAction::Protect => decision.protect(),
                FilterAction::Risk => decision.unprotect(),
                FilterAction::Include
                | FilterAction::Exclude
                | FilterAction::Clear
                | FilterAction::Merge
                | FilterAction::DirMerge => {}
            }
        }

        // upstream: exclude.c:1038 check_filter() walks ONE list first-match-wins
        // and returns `ent->rflags & FILTRULE_INCLUDE ? 1 : -1`. oc partitions
        // that list into the include/exclude and protect/risk chains, so combine
        // their two receiver-side first-matches back into a single source-ordered
        // pass: the earlier-defined rule decides. An include/risk (`+`/`R`) makes
        // the entry deletable, an exclude/protect (`-`/`P`) protects it, and no
        // match at all leaves it deletable (check_filter returns 0). This is the
        // authoritative deletion verdict; the split `transfer_allowed && !protected`
        // fallback stays only for decisions produced outside the Deletion context.
        if for_deletion {
            decision.deletion_override = Some(deletion_first_match_deletable(
                transfer_rule,
                protection_rule,
            ));
        }

        decision
    }

    /// Returns `true` when any rule in this set (in the supplied direction)
    /// matches the path.
    ///
    /// Descendant matchers are skipped so the result reflects only real
    /// user-written rules, matching upstream `exclude.c:rule_matches()`
    /// which has no descendant matching at all. Both the include/exclude
    /// and protect/risk chains are consulted: a protect rule still counts
    /// as a scope match because it influences the deletion decision.
    ///
    /// This is the predicate the per-directory chain uses to detect
    /// whether a scope is silent on a path and fall through to outer
    /// scopes.
    pub(crate) fn has_matching_rule(
        &self,
        path: &Path,
        is_dir: bool,
        context: DecisionContext,
    ) -> bool {
        let applies: fn(&CompiledRule) -> bool = match context {
            DecisionContext::Transfer => |rule| rule.applies_to_sender,
            DecisionContext::Deletion => |rule| rule.applies_to_receiver,
        };
        // upstream: exclude.c:1044 check_filter() only skips perishable rules
        // when `ignore_perishable` is set, which happens exclusively inside a
        // directory being wholly deleted (delete.c:147). The receiver's
        // top-level delete scan runs with `ignore_perishable == 0`, so a
        // perishable rule still matches (and therefore protects) a candidate.
        let include_perishable = true;
        let for_deletion = matches!(context, DecisionContext::Deletion);
        if first_matching_rule(
            &self.include_exclude,
            path,
            is_dir,
            applies,
            include_perishable,
            false,
            for_deletion,
        )
        .is_some()
        {
            return true;
        }
        first_matching_rule(
            &self.protect_risk,
            path,
            is_dir,
            applies,
            include_perishable,
            false,
            for_deletion,
        )
        .is_some()
    }

    /// Checks whether a directory is excluded by a non-directory-specific rule.
    ///
    /// When `--prune-empty-dirs` is active, directories excluded by generic
    /// patterns (e.g., `exclude("*")`) should still be descended into so that
    /// file-level include rules can be evaluated. Only directory-specific
    /// exclude patterns (trailing `/`) should prevent traversal outright.
    ///
    /// Returns `true` when the first matching sender-side include/exclude rule
    /// is an exclude rule whose pattern is NOT directory-only.
    pub(crate) fn excluded_dir_by_non_dir_rule(&self, path: &Path) -> bool {
        if let Some(rule) = first_matching_rule(
            &self.include_exclude,
            path,
            true,
            |rule| rule.applies_to_sender,
            true,
            false,
            false,
        ) {
            matches!(rule.action, FilterAction::Exclude) && !rule.is_directory_only()
        } else {
            false
        }
    }
}

/// Finds the first matching rule in the list (first-match-wins semantics).
///
/// This matches upstream rsync's `check_filter()` in exclude.c which iterates
/// from the head of the filter list and returns on the first match.
///
/// # Arguments
///
/// * `rules` - Compiled rules to search, evaluated in order
/// * `path` - File path to match against rule patterns
/// * `is_dir` - Whether the path is a directory (affects trailing-slash patterns)
/// * `applies` - Predicate filtering which rules are considered (e.g., sender-only rules)
/// * `include_perishable` - Whether to consider perishable rules (marked with `p` modifier)
/// * `check_descendants` - Whether to consult the synthetic `pattern/**` descendant matchers
/// * `for_deletion` - When `true`, also consult the deletion-only descendant
///   matchers so an excluded directory protects its children from deletion
///   (upstream subtree-pruning semantic). Kept off the transfer path so a
///   directory-only unanchored wildcard like `foo/*/` does not over-exclude
///   files inside subdirectories (#6015).
///
/// # Returns
///
/// The first rule where all conditions are met:
/// 1. `include_perishable` is true OR the rule is not perishable
/// 2. `applies(rule)` returns true
/// 3. The rule's pattern matches `path` considering `is_dir`
fn first_matching_rule<'a, F>(
    rules: &'a [CompiledRule],
    path: &Path,
    is_dir: bool,
    mut applies: F,
    include_perishable: bool,
    check_descendants: bool,
    for_deletion: bool,
) -> Option<&'a CompiledRule>
where
    F: FnMut(&CompiledRule) -> bool,
{
    rules.iter().find(|rule| {
        (include_perishable || !rule.perishable)
            && applies(rule)
            && if for_deletion {
                rule.matches_for_deletion(path, is_dir, check_descendants)
            } else {
                rule.matches(path, is_dir, check_descendants)
            }
    })
}

/// Merges the include/exclude and protect/risk first-matches into the single
/// source-ordered verdict upstream's `check_filter()` produces for deletion.
///
/// Upstream keeps every rule in one list and returns on the first match with
/// `rflags & FILTRULE_INCLUDE ? 1 : -1` (exclude.c:1060). oc splits the list
/// into two chains, so the earlier-defined of the two first-matches (by
/// [`CompiledRule::order`]) is the one upstream would have hit first. A rule
/// carrying the include flag (`+` include or `R` risk) makes the entry
/// deletable; an exclude (`-`) or protect (`P`) protects it. When neither chain
/// matches, `check_filter` returns 0 and the entry is deletable by default.
///
/// upstream: exclude.c:check_filter()
fn deletion_first_match_deletable(
    include_exclude: Option<&CompiledRule>,
    protect_risk: Option<&CompiledRule>,
) -> bool {
    let winner = match (include_exclude, protect_risk) {
        (Some(ie), Some(pr)) => Some(if ie.order <= pr.order { ie } else { pr }),
        (Some(ie), None) => Some(ie),
        (None, Some(pr)) => Some(pr),
        (None, None) => None,
    };
    match winner {
        None => true,
        Some(rule) => matches!(rule.action, FilterAction::Include | FilterAction::Risk),
    }
}

/// Resolves the winning include/exclude rule for a single-path Transfer query,
/// gating the synthetic descendant matchers by upstream's subtree-pruning rule.
///
/// Genuine (direct) matches keep first-match-wins semantics, matching upstream
/// `exclude.c:rule_matches()`. The synthetic `{core}/**` descendant matcher an
/// exclude carries is only allowed to fire when the excluded directory is
/// actually pruned under first-match-wins: upstream never descends into an
/// excluded directory (`flist.c:send_directory`), but an *earlier* include that
/// keeps that directory means the walk *does* descend, so its contents must not
/// be pruned. Without this gate a rule pair like `+ foo` / `- foo/` would let
/// the `foo/**` descendant exclude `foo/bar` even though `foo` itself is kept.
///
/// upstream: exclude.c:check_filter() first-match-wins plus the send_directory
/// subtree pruning that the descendant matchers emulate for single-path queries.
fn transfer_rule_with_pruning<'a, F>(
    rules: &'a [CompiledRule],
    path: &Path,
    is_dir: bool,
    mut applies: F,
    include_perishable: bool,
) -> Option<&'a CompiledRule>
where
    F: FnMut(&CompiledRule) -> bool,
{
    for rule in rules {
        if (!include_perishable && rule.perishable) || !applies(rule) {
            continue;
        }
        // A genuine per-path match wins immediately (upstream rule_matches).
        if rule.matches(path, is_dir, false) {
            return Some(rule);
        }
        // A descendant-only exclude prunes `path` only when the directory it
        // excludes is itself the first match under first-match-wins.
        if matches!(rule.action, FilterAction::Exclude)
            && rule.matches(path, is_dir, true)
            && ancestor_pruned_by_exclude(rules, path, &mut applies, include_perishable)
        {
            return Some(rule);
        }
    }
    None
}

/// Returns `true` when a proper ancestor directory of `path` is excluded under
/// first-match-wins, reproducing upstream's per-directory descent: the walk
/// stops at the shallowest ancestor whose first matching rule is an exclude and
/// never reaches `path`; an include keeps the directory and the walk descends.
fn ancestor_pruned_by_exclude<F>(
    rules: &[CompiledRule],
    path: &Path,
    mut applies: F,
    include_perishable: bool,
) -> bool
where
    F: FnMut(&CompiledRule) -> bool,
{
    let components: Vec<_> = path.components().collect();
    let mut ancestor = std::path::PathBuf::new();
    for comp in &components[..components.len().saturating_sub(1)] {
        ancestor.push(comp);
        if let Some(rule) = first_matching_rule(
            rules,
            &ancestor,
            true,
            &mut applies,
            include_perishable,
            false,
            false,
        ) && matches!(rule.action, FilterAction::Exclude)
        {
            return true;
        }
    }
    false
}

/// Whether a filter evaluation is for the transfer or deletion phase.
///
/// Transfer checks use sender-side rules; Deletion checks use receiver-side
/// rules. Both honour perishable rules: the top-level delete scan runs with
/// upstream's `ignore_perishable` unset (delete.c:147), so a perishable rule
/// still protects a matching destination entry from `--delete`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecisionContext {
    Transfer,
    Deletion,
}

/// Outcome of evaluating a path against the compiled filter rules.
///
/// Captures both the transfer decision (include or exclude) and the deletion
/// protection state. The default allows both transfer and deletion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FilterDecision {
    transfer_allowed: bool,
    protected: bool,
    excluded_for_delete_excluded: bool,
    /// Unified deletion verdict for decisions produced in the
    /// [`Deletion`](DecisionContext::Deletion) context, computed as a single
    /// source-ordered first-match over both rule chains (upstream
    /// `check_filter()`). `None` for Transfer-context decisions, where deletion
    /// is not queried and `allows_deletion` falls back to the split formula.
    deletion_override: Option<bool>,
}

impl FilterDecision {
    /// Returns `true` if the path should be included in the transfer.
    pub(crate) const fn allows_transfer(self) -> bool {
        self.transfer_allowed
    }

    /// Returns `true` if the path may be deleted on the receiver.
    ///
    /// For decisions produced in the Deletion context this is the unified
    /// source-ordered first-match verdict ([`deletion_override`]), matching
    /// upstream `check_filter()`. The split `transfer_allowed && !protected`
    /// formula remains only as the fallback for decisions built in other
    /// contexts, where `deletion_override` is unset.
    ///
    /// [`deletion_override`]: FilterDecision::deletion_override
    pub(crate) const fn allows_deletion(self) -> bool {
        match self.deletion_override {
            Some(deletable) => deletable,
            None => self.transfer_allowed && !self.protected,
        }
    }

    /// Returns `true` if the path may be removed during `--delete-excluded`.
    ///
    /// Unlike `allows_deletion`, this checks whether the path is excluded
    /// rather than included, supporting the `--delete-excluded` flag.
    pub(crate) const fn allows_deletion_when_excluded_removed(self) -> bool {
        self.excluded_for_delete_excluded && !self.protected
    }

    /// Marks this path as protected from deletion.
    pub(crate) const fn protect(&mut self) {
        self.protected = true;
    }

    /// Removes deletion protection from this path.
    pub(crate) const fn unprotect(&mut self) {
        self.protected = false;
    }
}

impl Default for FilterDecision {
    fn default() -> Self {
        Self {
            transfer_allowed: true,
            protected: false,
            excluded_for_delete_excluded: false,
            deletion_override: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_decision_default() {
        let decision = FilterDecision::default();
        assert!(decision.allows_transfer());
        assert!(decision.allows_deletion());
        assert!(!decision.allows_deletion_when_excluded_removed());
    }

    #[test]
    fn filter_decision_protect() {
        let mut decision = FilterDecision::default();
        decision.protect();
        assert!(decision.allows_transfer());
        assert!(!decision.allows_deletion());
    }

    #[test]
    fn filter_decision_unprotect() {
        let mut decision = FilterDecision::default();
        decision.protect();
        decision.unprotect();
        assert!(decision.allows_transfer());
        assert!(decision.allows_deletion());
    }

    #[test]
    fn filter_decision_transfer_not_allowed() {
        let decision = FilterDecision {
            transfer_allowed: false,
            protected: false,
            excluded_for_delete_excluded: false,
            deletion_override: None,
        };
        assert!(!decision.allows_transfer());
        assert!(!decision.allows_deletion());
    }

    #[test]
    fn filter_decision_excluded_for_delete_excluded() {
        let decision = FilterDecision {
            transfer_allowed: false,
            protected: false,
            excluded_for_delete_excluded: true,
            deletion_override: None,
        };
        assert!(decision.allows_deletion_when_excluded_removed());
    }

    #[test]
    fn filter_decision_protected_blocks_excluded_removal() {
        let decision = FilterDecision {
            transfer_allowed: false,
            protected: true,
            excluded_for_delete_excluded: true,
            deletion_override: None,
        };
        assert!(!decision.allows_deletion_when_excluded_removed());
    }

    #[test]
    fn decision_context_eq() {
        assert_eq!(DecisionContext::Transfer, DecisionContext::Transfer);
        assert_eq!(DecisionContext::Deletion, DecisionContext::Deletion);
        assert_ne!(DecisionContext::Transfer, DecisionContext::Deletion);
    }

    #[test]
    fn filter_set_inner_default_is_empty() {
        let inner = FilterSetInner::default();
        assert!(inner.include_exclude.is_empty());
        assert!(inner.protect_risk.is_empty());
    }

    use crate::FilterRule;

    fn push_rule(inner: &mut FilterSetInner, action: FilterAction, pattern: &str) {
        let rule = FilterRule {
            action,
            pattern: pattern.to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
        };
        inner
            .include_exclude
            .push(CompiledRule::new(rule).expect("compile"));
    }

    /// UTS-V3.B regression: `+ /bar/` under Deletion against `./bar/.filt`
    /// must match upstream behaviour. The include rule is anchored and
    /// directory-only; the file `bar/.filt` is not the included directory
    /// and is not pulled in by it. With no excluding rule in the chain the
    /// default decision is "allow deletion" - upstream's
    /// `exclude.c::rule_matches()` returns no match for the include rule
    /// (FILTRULE_DIRECTORY on a non-dir) and falls through.
    ///
    /// Both the Recursive-traversal call (`allows_deletion_during_traversal`)
    /// and the single-path call (`allows_deletion`) converge on the same
    /// upstream outcome.
    ///
    /// upstream: exclude.c::rule_matches()
    #[test]
    fn deletion_include_bar_dir_does_not_force_include_bar_filt() {
        let mut inner = FilterSetInner::default();
        push_rule(&mut inner, FilterAction::Include, "/bar/");

        let path = Path::new("bar/.filt");
        let recursive = inner.decision_with_traversal(path, false, DecisionContext::Deletion, true);
        assert!(
            recursive.allows_deletion(),
            "Deletion+Recursive: + /bar/ must not force-include bar/.filt",
        );

        let single = inner.decision_with_traversal(path, false, DecisionContext::Deletion, false);
        assert!(
            single.allows_deletion(),
            "Deletion single-path: + /bar/ must not force-include bar/.filt",
        );
    }

    /// UTS-V3.B regression: `+ /foo/s?b/` under Deletion against
    /// `./foo/sub/file1` must match upstream behaviour. The include rule
    /// is anchored, directory-only, and wildcard-bearing - it matches the
    /// directory `foo/sub` but does not pull files inside it into the
    /// transfer (per upstream `exclude.c::rule_matches()` FILTRULE_DIRECTORY
    /// semantic). With no other matching rule the default decision is
    /// "allow deletion" on both the Recursive-traversal and single-path
    /// entry points.
    ///
    /// upstream: exclude.c::rule_matches()
    #[test]
    fn deletion_include_foo_sub_dir_does_not_force_include_file1() {
        let mut inner = FilterSetInner::default();
        push_rule(&mut inner, FilterAction::Include, "/foo/s?b/");

        let path = Path::new("foo/sub/file1");
        let recursive = inner.decision_with_traversal(path, false, DecisionContext::Deletion, true);
        assert!(
            recursive.allows_deletion(),
            "Deletion+Recursive: + /foo/s?b/ must not force-include foo/sub/file1",
        );

        let single = inner.decision_with_traversal(path, false, DecisionContext::Deletion, false);
        assert!(
            single.allows_deletion(),
            "Deletion single-path: + /foo/s?b/ must not force-include foo/sub/file1",
        );
    }

    /// UTS-V3.B regression: `- /bar` synthesises `bar/**` descendants.
    /// Under Deletion single-path the descendant fires on `bar/.filt` so
    /// the receiver excludes it from the delete pass; under
    /// Deletion+Recursive the runtime `check_descendants = !traversal`
    /// gate suppresses descendants because the walk itself handles
    /// descent control, matching upstream `exclude.c::rule_matches()`.
    ///
    /// upstream: exclude.c::rule_matches()
    #[test]
    fn deletion_anchored_literal_exclude_descends_only_off_traversal() {
        let mut inner = FilterSetInner::default();
        push_rule(&mut inner, FilterAction::Exclude, "/bar");

        let path = Path::new("bar/.filt");

        let single = inner.decision_with_traversal(path, false, DecisionContext::Deletion, false);
        assert!(
            !single.allows_deletion(),
            "Deletion single-path: - /bar must exclude bar/.filt via descendant matcher",
        );

        let recursive = inner.decision_with_traversal(path, false, DecisionContext::Deletion, true);
        assert!(
            recursive.allows_deletion(),
            "Deletion+Recursive: descendant matchers are suppressed because the walk handles descent",
        );
    }

    /// Regression for the `exclude` / `exclude-lsh` over-deletion: a
    /// directory-only unanchored wildcard exclude (`foo/*/`) must protect the
    /// children of the matched directory from deletion on the receiver. After
    /// #6015 suppressed the synthesised `foo/*/**` descendant for BOTH the
    /// transfer and deletion paths, `foo/sub/file1` fell through to the
    /// default "allow deletion" and was over-deleted under `--delete-during`.
    /// The deletion-only descendant matcher restores child protection while
    /// keeping the descendant invisible to the transfer path (see
    /// [`deletion_dir_only_wildcard_does_not_affect_transfer`]).
    ///
    /// upstream: an excluded directory protects its descendants from deletion
    /// because the generator never descends into it (exclude.c subtree
    /// pruning); the receiver scan must reproduce that per-candidate.
    #[test]
    fn deletion_dir_only_wildcard_protects_children() {
        let mut inner = FilterSetInner::default();
        push_rule(&mut inner, FilterAction::Exclude, "foo/*/");

        let child = Path::new("foo/sub/file1");
        // Both the single-path API and the per-directory traversal commit must
        // treat the child of the excluded directory as NOT deletable; the
        // deletion-only descendants fire independently of the traversal gate.
        let single = inner.decision_with_traversal(child, false, DecisionContext::Deletion, false);
        assert!(
            !single.allows_deletion(),
            "Deletion single-path: foo/*/ must protect foo/sub/file1 from deletion",
        );
        let recursive =
            inner.decision_with_traversal(child, false, DecisionContext::Deletion, true);
        assert!(
            !recursive.allows_deletion(),
            "Deletion+traversal: foo/*/ must protect foo/sub/file1 from deletion",
        );

        // A nested grandchild is equally protected.
        let deep = Path::new("foo/sub/inner/file2");
        assert!(
            !inner
                .decision_with_traversal(deep, false, DecisionContext::Deletion, false)
                .allows_deletion(),
            "Deletion: foo/*/ must protect foo/sub/inner/file2 from deletion",
        );
    }

    /// #6015 guard: the deletion-only descendant for a directory-only
    /// unanchored wildcard exclude (`foo/*/`) must NOT leak into the transfer
    /// path. Upstream emits no `foo/*/**` transfer rule, so `foo/sub/file1`
    /// stays included for transfer (it must match its own rules, and there is
    /// none). Re-exposing the descendant here would re-trip the over-exclusion
    /// #6015 fixed (`complex_nested_patterns`).
    #[test]
    fn deletion_dir_only_wildcard_does_not_affect_transfer() {
        let mut inner = FilterSetInner::default();
        push_rule(&mut inner, FilterAction::Exclude, "foo/*/");

        let child = Path::new("foo/sub/file1");
        // The directory itself is excluded for transfer (direct matcher).
        assert!(
            !inner
                .decision(Path::new("foo/sub"), true, DecisionContext::Transfer)
                .allows_transfer(),
            "Transfer: foo/*/ excludes the directory foo/sub",
        );
        // But a file inside is NOT excluded by the directory-only wildcard on
        // the transfer path - it would only be skipped because the sender walk
        // never descends into the pruned directory.
        assert!(
            inner
                .decision(child, false, DecisionContext::Transfer)
                .allows_transfer(),
            "Transfer: foo/*/ must NOT exclude foo/sub/file1 via a synthetic descendant (#6015)",
        );
        assert!(
            inner
                .decision_with_traversal(child, false, DecisionContext::Transfer, true)
                .allows_transfer(),
            "Transfer+traversal: foo/*/ must NOT exclude foo/sub/file1 (#6015)",
        );
    }
}
