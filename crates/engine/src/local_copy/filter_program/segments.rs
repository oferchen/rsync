//! Compiled filter segments and stacks that evaluate include/exclude and
//! protect/risk rules sequentially with first-match
//! semantics, mirroring upstream `exclude.c` rule traversal.

use std::path::Path;

use filters::{FilterAction, FilterRule, RuleMatcher};
use logging::debug_log;

/// Compiled list of rules evaluated sequentially.
#[derive(Clone, Debug, Default)]
pub(crate) struct FilterSegment {
    include_exclude: Vec<CompiledRule>,
    protect_risk: Vec<CompiledRule>,
    /// Running source-definition counter stamped onto each compiled rule so the
    /// deletion decision can merge the two chains back into the single
    /// first-match-wins pass upstream `check_filter()` performs (exclude.c:1192-1212).
    next_order: usize,
}

impl FilterSegment {
    pub(crate) fn push_rule(&mut self, rule: FilterRule) -> Result<(), super::FilterProgramError> {
        match rule.action() {
            FilterAction::Include | FilterAction::Exclude => {
                let mut compiled = CompiledRule::new(rule)?;
                compiled.order = self.next_order;
                self.next_order += 1;
                self.include_exclude.push(compiled);
            }
            FilterAction::Protect | FilterAction::Risk => {
                let mut compiled = CompiledRule::new(rule)?;
                compiled.order = self.next_order;
                self.next_order += 1;
                self.protect_risk.push(compiled);
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
            if rule.applies_to_sender() && rule.matcher.matches(path, true) {
                if matches!(rule.action(), FilterAction::Exclude) {
                    return Some(!rule.matcher.is_directory_only());
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
        // exist in the file list. `RuleMatcher` skips descendants for that
        // reason, so anchored patterns like `- /bar` do not over-match deep
        // paths (UTS-V3-B exclude-lsh).
        // upstream: an excluded directory protects its descendants from
        // deletion because the generator never descends into it. On the
        // deletion scan a directory-only unanchored wildcard exclude (`foo/*/`)
        // therefore keeps `foo/sub/file1` off the delete pass via its
        // deletion-only descendant matchers; the Transfer path must NOT see
        // those (it would re-trip the `foo/*/` over-exclusion).
        let for_deletion = matches!(context, FilterContext::Deletion);
        // upstream: exclude.c:1193 check_filter() only skips a perishable rule
        // when `ignore_perishable` is set, which happens exclusively while
        // deleting the contents of a directory being wholly removed
        // (delete.c:205-211 delete_item()). The top-level delete scan runs with it unset, so a
        // perishable rule still matches and protects a candidate here; the
        // wholly-deleted-directory case never reaches this evaluator because
        // the local-copy delete pass removes such directories en masse.
        let rule_matches = |rule: &CompiledRule, path: &Path, is_dir: bool| {
            if for_deletion {
                rule.matcher.matches_for_deletion(path, is_dir)
            } else {
                rule.matcher.matches(path, is_dir)
            }
        };
        for rule in &self.include_exclude {
            if outcome.transfer_decided() {
                break;
            }
            if rule_matches(rule, path, is_dir) {
                match context {
                    FilterContext::Transfer => {
                        if rule.applies_to_sender() {
                            report_filter_result(rule, path, is_dir, "sender");
                            outcome.set_transfer_allowed(matches!(
                                rule.action(),
                                FilterAction::Include
                            ));
                            outcome.decide_transfer();
                        }
                    }
                    FilterContext::Deletion => {
                        if rule.applies_to_receiver() {
                            report_filter_result(rule, path, is_dir, "generator");
                            outcome.set_transfer_allowed(matches!(
                                rule.action(),
                                FilterAction::Include
                            ));
                            outcome.decide_transfer();
                        }
                    }
                }
            }
        }

        for rule in &self.protect_risk {
            // upstream check_filter() (exclude.c:1207-1210) returns on the first
            // matching rule, so the first protect/risk decision wins. Stop once a
            // protect/risk rule has applied rather than letting a later rule
            // overwrite it (last-match-wins).
            if outcome.protection_decided() {
                break;
            }
            if rule_matches(rule, path, is_dir) {
                let applies = match context {
                    FilterContext::Transfer => rule.applies_to_sender(),
                    FilterContext::Deletion => rule.applies_to_receiver(),
                };
                if applies {
                    let who = match context {
                        FilterContext::Transfer => "sender",
                        FilterContext::Deletion => "generator",
                    };
                    report_filter_result(rule, path, is_dir, who);
                    match rule.action() {
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

        // upstream: exclude.c:1192-1212 check_filter() walks ONE list first-match-wins
        // and returns `ent->rflags & FILTRULE_INCLUDE ? 1 : -1`. oc partitions
        // that list into the include/exclude and protect/risk chains, so the two
        // decisions above latch independently and would let a protect override an
        // earlier include (or vice versa) regardless of source order. Merge the
        // two receiver-side first-matches back into a single source-ordered pass:
        // the earlier-defined rule (by `order`) decides. Include/risk makes the
        // entry deletable, exclude/protect protects it. Latched globally so a
        // later segment cannot override the first match (upstream returns on it).
        if for_deletion && !outcome.deletion_decided() {
            let ie_hit = self
                .include_exclude
                .iter()
                .find(|rule| rule.applies_to_receiver() && rule_matches(rule, path, is_dir));
            let pr_hit = self
                .protect_risk
                .iter()
                .find(|rule| rule.applies_to_receiver() && rule_matches(rule, path, is_dir));
            let winner = match (ie_hit, pr_hit) {
                (Some(ie), Some(pr)) => Some(if ie.order <= pr.order { ie } else { pr }),
                (Some(ie), None) => Some(ie),
                (None, Some(pr)) => Some(pr),
                (None, None) => None,
            };
            if let Some(rule) = winner {
                outcome.decide_deletion(matches!(
                    rule.action(),
                    FilterAction::Include | FilterAction::Risk
                ));
            }
        }
    }
}

/// Emits a `--debug=FILTER` line for a rule that fired on `path`, naming the
/// file, its type, and the matching pattern.
///
/// upstream: exclude.c:1099 report_filter_result() - `[who] {action}ing {type}
/// {name} because of pattern {pattern}{/}`. `debug_log!` gates on the FILTER
/// debug level internally, so the message is only formatted when enabled.
///
/// BOTH interpolated halves are redacted for a file-sourced rule, exactly as
/// upstream does: the pattern through `rule_text()` and the trailing `/`
/// through `rule_detail()`. The slash is not decoration - it reports whether
/// the hidden rule was directory-only, which is a property OF the text being
/// withheld, so upstream drops it "along with the text it describes"
/// (exclude.c:127-131).
///
/// This trace runs at level 1 for a sender or generator (exclude.c:1087), so
/// plain `-vv` reaches it with no `--debug` at all - which is why an unredacted
/// pattern here echoes a peer-chosen merge file's contents to any client.
fn report_filter_result(rule: &CompiledRule, path: &Path, is_dir: bool, who: &str) {
    let verb = match rule.action() {
        FilterAction::Include => "including",
        FilterAction::Exclude => "excluding",
        FilterAction::Protect => "protecting",
        FilterAction::Risk => "risking",
        _ => return,
    };
    let kind = if is_dir { "directory" } else { "file" };
    let shown = match_trace_pattern(rule);
    debug_log!(
        Filter,
        1,
        "[{who}] {verb} {kind} {} because of pattern {shown}",
        path.display(),
    );
}

/// Renders the `{pattern}{/}` tail of the match trace, redacting BOTH halves
/// for a file-sourced rule.
///
/// Split out from [`report_filter_result`] so the redaction can be asserted
/// without capturing log output: the emitter decides *whether* to log, this
/// decides *what the text says*.
///
/// upstream: exclude.c:1099 passes the two halves through separate redactors -
/// `rule_text(ent, ent->pattern)` and `rule_detail(ent, ... ? "/" : "")`. The
/// slash reports whether the withheld rule was directory-only, which is a
/// property OF the hidden text, so upstream drops it too (exclude.c:127-131).
///
/// The trailing `/` is stripped from the pattern before the two halves are
/// composed. Upstream's `add_rule` consumes it at parse time - it sets
/// `FILTRULE_DIRECTORY` and stores a slash-free `ent->pattern`
/// (exclude.c:259-275) - so the slash reaches the trace exactly once, from
/// `rule_detail`. oc keeps the operator's raw spelling on the rule and derives
/// `directory_only` from it, so without this strip a directory-only rule
/// renders `sub//` where upstream renders `sub/` (measured against 3.5.0).
fn match_trace_pattern(rule: &CompiledRule) -> String {
    let written = String::from_utf8_lossy(rule.matcher.pattern());
    let pattern = written.strip_suffix('/').unwrap_or(&written);
    let source = rule.matcher.source().as_source();
    let shown = source.rule_text(pattern);
    let slash = source.rule_detail(if rule.matcher.is_directory_only() {
        "/"
    } else {
        ""
    });
    format!("{shown}{slash}")
}

#[derive(Clone, Debug)]
pub(crate) enum FilterInstruction {
    Segment(FilterSegment),
    DirMerge { index: usize },
}

pub(crate) type FilterSegmentLayers = Vec<Vec<FilterSegment>>;
pub(crate) type FilterSegmentStack = Vec<Vec<(usize, FilterSegment)>>;

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
    /// Unified deletion verdict: the include-flag of the single first-matching
    /// rule across BOTH chains in source order, mirroring upstream
    /// `check_filter()` (exclude.c:1209 `rflags & FILTRULE_INCLUDE ? 1 : -1`).
    /// Latched once decided so later segments cannot override it.
    deletion_deletable: bool,
    deletion_decided: bool,
}

impl FilterOutcome {
    const fn new() -> Self {
        Self {
            transfer_allowed: true,
            transfer_decided: false,
            protected: false,
            protection_decided: false,
            deletion_deletable: true,
            deletion_decided: false,
        }
    }

    pub(crate) const fn allows_transfer(self) -> bool {
        self.transfer_allowed
    }

    /// Whether the receiver may delete this entry.
    ///
    /// upstream `check_filter()` is a single first-match-wins pass over the
    /// whole rule list; oc splits it into two chains, so [`FilterSegment::apply`]
    /// merges their first receiver-side matches by source order into
    /// [`Self::deletion_deletable`]. When any rule matched, that unified verdict
    /// is authoritative; with no match at all we fall back to the split
    /// `transfer_allowed && !protected` (which is likewise deletable by default).
    pub(crate) const fn allows_deletion(self) -> bool {
        if self.deletion_decided {
            self.deletion_deletable
        } else {
            self.transfer_allowed && !self.protected
        }
    }

    pub(crate) const fn transfer_decided(self) -> bool {
        self.transfer_decided
    }

    const fn protection_decided(self) -> bool {
        self.protection_decided
    }

    const fn deletion_decided(self) -> bool {
        self.deletion_decided
    }

    /// Latches the unified deletion verdict from the first matching rule.
    const fn decide_deletion(&mut self, deletable: bool) {
        self.deletion_deletable = deletable;
        self.deletion_decided = true;
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
}

impl Default for FilterOutcome {
    fn default() -> Self {
        Self::new()
    }
}

/// A rule of a [`FilterSegment`]: the filters crate's matcher plus its place in
/// the segment's source order.
#[derive(Clone, Debug)]
struct CompiledRule {
    matcher: RuleMatcher,
    /// Source-definition order within the owning segment, assigned by
    /// [`FilterSegment::push_rule`]. Lets the deletion decision merge the
    /// include/exclude and protect/risk chains back into the single
    /// first-match-wins pass upstream `check_filter()` runs (exclude.c:1192-1212).
    order: usize,
}

impl CompiledRule {
    fn new(rule: FilterRule) -> Result<Self, super::FilterProgramError> {
        let pattern = String::from_utf8_lossy(rule.pattern()).into_owned();
        let matcher = RuleMatcher::new(rule)
            .map_err(|error| super::FilterProgramError::new(pattern, error))?;
        Ok(Self { matcher, order: 0 })
    }

    const fn action(&self) -> FilterAction {
        self.matcher.action()
    }

    const fn applies_to_sender(&self) -> bool {
        self.matcher.applies_to_sender()
    }

    const fn applies_to_receiver(&self) -> bool {
        self.matcher.applies_to_receiver()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filters::OwnedRuleSource;

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

    /// A rule whose text came out of a merge file must not have that text
    /// echoed by the match trace - and the trailing `/` goes with it, because
    /// it reports a property of the withheld text.
    ///
    /// The trace runs at level 1 for a sender or generator (exclude.c:1087),
    /// so plain `-vv` reaches it: an unredacted pattern here hands a
    /// peer-chosen per-directory merge file's contents to any client.
    #[test]
    fn match_trace_redacts_a_merge_file_rules_text_and_its_slash() {
        let rule = CompiledRule::new(
            FilterRule::exclude("sec[r]et-marker-9x/".to_owned())
                .with_source(OwnedRuleSource::FileReadEarlier),
        )
        .unwrap();

        assert_eq!(
            match_trace_pattern(&rule),
            "<rule from a file read earlier>"
        );
    }

    /// The non-vacuity companion: upstream shows an ARGUMENT rule's text
    /// verbatim "because it is the user's own and hiding it only makes typos
    /// harder to fix" (exclude.c:90-95), slash included. Without this row a
    /// blanket-redaction bug would satisfy the test above.
    #[test]
    fn match_trace_shows_an_argument_rules_text_and_its_slash_verbatim() {
        let rule =
            CompiledRule::new(FilterRule::exclude("sec[r]et-marker-9x/".to_owned())).unwrap();

        assert_eq!(match_trace_pattern(&rule), "sec[r]et-marker-9x/");
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
        assert!(rule.matcher.matches(Path::new("subdir"), true));
        // Files inside an included directory must still match a separate rule;
        // include never expands to descendants, not even on the deletion scan.
        assert!(!rule.matcher.matches(Path::new("file.txt"), false));
        assert!(
            !rule
                .matcher
                .matches_for_deletion(Path::new("subdir/debug.log"), false)
        );
        assert!(
            !rule
                .matcher
                .matches_for_deletion(Path::new("subdir/report.csv"), false)
        );
    }

    /// `--exclude '*/'` (a directory-only unanchored wildcard) routes its
    /// `*/**` descendant into the deletion-only set, mirroring upstream
    /// `exclude.c:rule_matches()`: the transfer path emits no `*/**` rule (the
    /// sender walk prunes the matched directory), while the receiver
    /// deletion scan must protect children of the excluded directory from
    /// over-deletion (`exclude` / `exclude-lsh` regression).
    #[test]
    fn exclude_directory_only_descendant_gated_by_context() {
        let rule = CompiledRule::new(FilterRule::exclude("*/".to_owned())).unwrap();
        // Directory entry matches directly in both contexts.
        assert!(rule.matcher.matches(Path::new("subdir"), true));
        // Transfer path never matches a child via a synthetic descendant.
        assert!(!rule.matcher.matches(Path::new("subdir/debug.log"), false));
        // Deletion path protects the child.
        assert!(
            rule.matcher
                .matches_for_deletion(Path::new("subdir/debug.log"), false)
        );
    }

    /// Verifies patterns with internal `/` but no leading `/` are NOT
    /// anchored: only a leading `/` anchors (exclude.c:1041-1044), and an
    /// internal one matches the trailing name elements (exclude.c:1046-1050).
    #[test]
    fn internal_slash_is_unanchored() {
        let rule = CompiledRule::new(FilterRule::exclude("src/lib/".to_owned())).unwrap();
        assert!(rule.matcher.is_directory_only());
        assert!(rule.matcher.matches(Path::new("a/src/lib"), true));
        assert!(!rule.matcher.matches(Path::new("a/src/lib"), false));
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
    }

    /// Regression for the `exclude` / `exclude-lsh` over-deletion: under
    /// `--delete-during` a directory-only unanchored wildcard exclude
    /// (`- foo/*/`) must protect the children of the matched directory from
    /// deletion. `FilterSegment::apply` calls with `check_descendants = false`,
    /// so the protection rides on the deletion-only descendant matchers. The
    /// transfer evaluation of the same child must stay "allowed" so the
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
        // descendant - the sender walk prunes the directory instead.
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
        assert!(rule.matcher.matches(Path::new("bar/down/to/foo/too"), true));
        assert!(rule.matcher.matches(Path::new("foo/too"), true));
        assert!(rule.matcher.matches(Path::new("fooxytoo"), false));
        assert!(!rule.matcher.matches(Path::new("foo/bar"), false));
    }

    #[test]
    fn filter_context_eq() {
        assert_eq!(FilterContext::Transfer, FilterContext::Transfer);
        assert_eq!(FilterContext::Deletion, FilterContext::Deletion);
        assert_ne!(FilterContext::Transfer, FilterContext::Deletion);
    }
}
