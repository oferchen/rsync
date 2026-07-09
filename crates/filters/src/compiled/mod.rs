//! Compiled filter rule representation and matching.
//!
//! Splits [`CompiledRule`] construction, pattern matching, and clear-rule
//! processing into focused submodules following single-responsibility:
//!
//! - `pattern` - pattern normalisation and glob compilation
//! - `rule` - the `CompiledRule` struct with matching and side-clearing logic
//! - `clear` - bulk clear-rule application over rule vectors

mod clear;
mod pattern;
mod rule;

use std::collections::HashSet;

use crate::{FilterAction, FilterError, FilterRule};

pub(crate) use clear::apply_clear_rule;
use pattern::{compile_patterns, normalise_pattern};
pub(crate) use rule::CompiledRule;

impl CompiledRule {
    /// Compiles a [`FilterRule`] into optimised glob matchers.
    ///
    /// The pattern is normalised (anchored/directory flags extracted), then
    /// expanded into direct matchers (for the pattern itself) and descendant
    /// matchers (for `pattern/**` to cover directory contents). Unanchored
    /// patterns additionally get `**/pattern` variants for matching at any
    /// depth.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] if the pattern cannot be compiled into a valid
    /// glob matcher.
    pub(crate) fn new(rule: FilterRule) -> Result<Self, FilterError> {
        let FilterRule {
            action,
            pattern,
            applies_to_sender,
            applies_to_receiver,
            perishable,
            xattr_only,
            negate,
            exclude_only: _,
            no_inherit: _,
            cvs_mode: _,
            abs_path: _,
            word_split: _,
            no_prefixes: _,
            no_prefixes_include: _,
        } = rule;
        debug_assert!(
            !xattr_only,
            "xattr-only rules should be filtered before compilation"
        );

        // upstream: exclude.c:add_rule() logs every parsed rule at
        // `DEBUG_GTE(FILTER, 2)` so the active rule set is observable.
        logging::debug_log!(
            Filter,
            2,
            "add_rule({} {})",
            match action {
                FilterAction::Include => "+",
                FilterAction::Exclude => "-",
                FilterAction::Protect => "P",
                FilterAction::Risk => "R",
                FilterAction::Clear => "!",
                FilterAction::Merge => "merge",
                FilterAction::DirMerge => "dir-merge",
            },
            pattern
        );

        let (anchored, directory_only, core_pattern) = normalise_pattern(&pattern);
        // upstream: exclude.c:903-960 rule_matches() - an unanchored pattern
        // that already begins with `**` is matched with slash_handling = -1
        // (try after every slash) by wildmatch_array (lib/wildmatch.c:316).
        // A leading `**` already carries the cross-depth anchor, so
        // prepending an extra `**/` would compound recursion and emit
        // matchers like `**/**/baz`. Skip the prefix only when
        // `core_pattern` already starts with `**`. Interior `**` (e.g.
        // `foo**too`, `foo/**/bar`) is NOT cross-depth on its own - the
        // pattern's leading literal still anchors it to the path root, so
        // the implicit `**/` prefix is required for upstream's
        // tail-matching semantics. Regression for UTS-DD-exclude.5 and the
        // double_star_interior_matches_across_path_segments guard.
        let has_double_star = core_pattern.starts_with("**");
        let mut direct_patterns = HashSet::new();
        direct_patterns.insert(core_pattern.to_string());
        if !anchored && !has_double_star {
            direct_patterns.insert(format!("**/{core_pattern}"));
        }

        let mut descendant_patterns = HashSet::new();
        // upstream: exclude.c:rule_matches() - excluding a directory excludes
        // its contents, but including a directory does NOT include its contents
        // (they must match their own rules).
        //
        // Anchored wildcard patterns (e.g., `/*`, `/*.txt`) must NOT generate
        // descendant matchers because `*/**` would incorrectly match nested
        // paths like `down/file.txt`. For these patterns, traversal control
        // (the sender/generator skips excluded directories) handles exclusion.
        //
        // Anchored literal patterns (e.g., `/build`, `/target/`) still need
        // `{core}/**` descendants so that paths like `build/output` are
        // excluded when checked individually (e.g., by the receiver).
        //
        // Directory-only unanchored wildcard patterns (e.g., `foo/*/`,
        // `**/node_modules/`) must ALSO suppress descendants. The wildcard's
        // match set is decided per-directory at walk time, so pre-baking
        // `foo/*/**` overreaches and excludes files inside subdirectories
        // that a later include rule like `+ foo/s?b/` should keep. Upstream
        // `exclude.c:rule_matches()` returns "no match" for a non-directory
        // entry when the rule carries FILTRULE_DIRECTORY (line 938-939), so
        // the file is included by default and the sender walk handles descent
        // pruning by never entering the excluded directory. Pre-baked
        // descendants would force an exclusion that upstream never produces
        // on EITHER the Transfer (sender) or Deletion (receiver single-path)
        // entry point. Regression for UTS-DD-exclude.3 (upstream `exclude` /
        // `exclude-lsh` testsuite) and `complex_nested_patterns`. The
        // dir-only suppression must fire for both contexts because
        // `CompiledRule` is shared across them and upstream's rule_matches()
        // returns "no match" for FILTRULE_DIRECTORY non-dir candidates
        // regardless of which call site dispatched the query.
        let has_glob_wildcard =
            core_pattern.contains('*') || core_pattern.contains('?') || core_pattern.contains('[');
        let slash_anchored = pattern.starts_with('/');
        // Directory-only unanchored wildcard gate: the user wrote `foo/*/`
        // (or any dir-only wildcard without a leading `/`). Upstream never
        // synthesises a descendant rule for it; the sender's traversal
        // pruning is the only exclusion mechanism and pre-baking
        // `{core}/**` would steal precedence from a later include like
        // `+ foo/s?b/`. Kept as an explicit predicate so the dir-only
        // unanchored case is greppable and pinned by unit tests.
        let is_directory_only_unanchored_wildcard =
            directory_only && !slash_anchored && has_glob_wildcard;
        let is_anchored_wildcard = slash_anchored && has_glob_wildcard;
        let suppress_descendants = is_directory_only_unanchored_wildcard || is_anchored_wildcard;
        let mut deletion_descendant_patterns = HashSet::new();
        if matches!(
            action,
            FilterAction::Exclude | FilterAction::Protect | FilterAction::Risk
        ) {
            if !suppress_descendants {
                descendant_patterns.insert(format!("{core_pattern}/**"));
                if !anchored && !has_double_star {
                    descendant_patterns.insert(format!("**/{core_pattern}/**"));
                }
            } else if is_directory_only_unanchored_wildcard {
                // upstream: exclude.c:rule_matches() emits no `foo/*/**`
                // transfer rule for a directory-only unanchored wildcard - the
                // sender walk prunes the matched directory instead (#6015).
                // The receiver's per-candidate deletion scan has no such
                // pruning, so route `{core}/**` into a deletion-only set that
                // keeps children of the excluded directory off the delete pass
                // without re-exposing `foo/*/**` to the Transfer path. The
                // anchored-wildcard case (`/*`) stays fully suppressed because
                // `*/**` would over-match nested paths even on deletion
                // (regression #5421).
                deletion_descendant_patterns.insert(format!("{core_pattern}/**"));
                if !anchored && !has_double_star {
                    deletion_descendant_patterns.insert(format!("**/{core_pattern}/**"));
                }
            }
        }

        // upstream FILTRULE_WILD2_PREFIX (exclude.c:241-242): set only when the
        // user's pattern starts with `**`, which implies it is unanchored.
        // Anchored patterns (`/**/*`) whose stem starts with `**` after the
        // leading-`/` strip are NOT WILD2_PREFIX and must not get the prepend.
        let wild2_prefix = !anchored && core_pattern.starts_with("**");
        let direct_matchers = compile_patterns(direct_patterns, &pattern, wild2_prefix)?;
        let descendant_matchers = compile_patterns(descendant_patterns, &pattern, wild2_prefix)?;
        let deletion_descendant_matchers =
            compile_patterns(deletion_descendant_patterns, &pattern, wild2_prefix)?;

        Ok(Self {
            action,
            pattern,
            directory_only,
            direct_matchers,
            descendant_matchers,
            deletion_descendant_matchers,
            applies_to_sender,
            applies_to_receiver,
            perishable,
            negate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_rule_new_simple_exclude() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.bak".to_owned(),
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
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Exclude);
        assert!(compiled.applies_to_sender);
        assert!(compiled.applies_to_receiver);
        assert!(!compiled.perishable);
    }

    #[test]
    fn compiled_rule_new_include() {
        let rule = FilterRule {
            action: FilterAction::Include,
            pattern: "*.rs".to_owned(),
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
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Include);
    }

    #[test]
    fn compiled_rule_perishable() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.log".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: true,
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
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.perishable);
    }

    /// Verifies that `--include '*/'` does NOT generate descendant matchers.
    ///
    /// upstream: Including a directory means "include the directory entry" -
    /// it does NOT mean "include everything inside it". Files inside must
    /// match their own rules. Only Exclude/Protect/Risk get descendants.
    #[test]
    fn include_directory_only_has_no_descendant_matchers() {
        let rule = FilterRule {
            action: FilterAction::Include,
            pattern: "*/".to_owned(),
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
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.directory_only);
        assert!(
            compiled.descendant_matchers.is_empty(),
            "include directory-only rules must not have descendant matchers"
        );
    }

    /// Verifies that `--exclude 'cache/'` DOES generate descendant matchers.
    ///
    /// upstream: Excluding a literal directory excludes all of its contents
    /// when the receiver checks them individually. Wildcard directory-only
    /// patterns are handled separately - see
    /// [`dir_only_wildcard_exclude_has_no_descendant_matchers`].
    #[test]
    fn exclude_directory_only_literal_has_descendant_matchers() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "cache/".to_owned(),
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
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.directory_only);
        assert!(
            !compiled.descendant_matchers.is_empty(),
            "exclude directory-only literal rules must have descendant matchers"
        );
    }

    /// Verifies that `--exclude '*/'` (and other directory-only wildcards)
    /// does NOT generate descendant matchers.
    ///
    /// upstream: `exclude.c:rule_matches()` line 938-939 returns "no match"
    /// when the rule carries `FILTRULE_DIRECTORY` and the candidate is not
    /// itself a directory. Pre-baking `*/**` (or `foo/*/**`) overreaches
    /// because the wildcard's per-directory match set is decided at walk
    /// time. The sender skips the directory subtree on a match; the
    /// receiver consults only the user-written rule and so should NOT see
    /// a synthetic descendant fire on a file under the matched directory.
    /// Regression for UTS-DD-exclude.3 and `complex_nested_patterns`.
    #[test]
    fn dir_only_wildcard_exclude_has_no_descendant_matchers() {
        for pattern in &[
            "*/",
            "foo/*/",
            "foo/s?b/",
            "bar/[a-z]*/",
            "**/node_modules/",
        ] {
            let rule = FilterRule {
                action: FilterAction::Exclude,
                pattern: pattern.to_string(),
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
            let compiled = CompiledRule::new(rule).unwrap();
            assert!(
                compiled.descendant_matchers.is_empty(),
                "directory-only wildcard pattern {pattern:?} must not have descendant matchers"
            );
        }
    }

    /// Anchored wildcard exclude patterns must NOT generate descendant
    /// matchers because `*/**` would incorrectly match nested paths like
    /// `down/file.txt`.
    ///
    /// upstream: exclude.c:rule_matches - for wildcard patterns like `/*`,
    /// traversal control handles exclusion of directory contents.
    #[test]
    fn anchored_wildcard_exclude_has_no_descendant_matchers() {
        for pattern in &["/*", "/*.txt", "/cache_?/"] {
            let rule = FilterRule {
                action: FilterAction::Exclude,
                pattern: pattern.to_string(),
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
            let compiled = CompiledRule::new(rule).unwrap();
            assert!(
                compiled.descendant_matchers.is_empty(),
                "anchored wildcard pattern {pattern:?} must not have descendant matchers"
            );
        }
    }

    /// Anchored literal exclude patterns still need descendant matchers so
    /// that paths like `build/output` are excluded when the receiver checks
    /// them individually (without traversal-skip control).
    #[test]
    fn anchored_literal_exclude_has_descendant_matchers() {
        for pattern in &["/build", "/build/", "/target/"] {
            let rule = FilterRule {
                action: FilterAction::Exclude,
                pattern: pattern.to_string(),
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
            let compiled = CompiledRule::new(rule).unwrap();
            assert!(
                !compiled.descendant_matchers.is_empty(),
                "anchored literal pattern {pattern:?} must have descendant matchers"
            );
        }
    }

    /// Unanchored exclude patterns still generate descendant matchers.
    #[test]
    fn unanchored_exclude_has_descendant_matchers() {
        for pattern in &["build", "*.bak", "cache/"] {
            let rule = FilterRule {
                action: FilterAction::Exclude,
                pattern: pattern.to_string(),
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
            let compiled = CompiledRule::new(rule).unwrap();
            assert!(
                !compiled.descendant_matchers.is_empty(),
                "unanchored pattern {pattern:?} must have descendant matchers"
            );
        }
    }

    /// `foo**too` must match `bar/down/to/foo/too` as a directory.
    ///
    /// upstream: `lib/wildmatch.c:dowild()` - `**` always matches across
    /// `/` boundaries regardless of surrounding characters. Without
    /// normalisation, globset's `literal_separator(true)` would treat the
    /// bare `**` as two single `*` wildcards (neither of which crosses
    /// `/`), so `foo**too` would only match `fooXYZtoo` within a single
    /// path segment. Regression test for the UTS-20 `exclude-lsh` followup.
    #[test]
    fn double_star_interior_matches_across_path_segments() {
        let rule = FilterRule {
            action: FilterAction::Include,
            pattern: "foo**too".to_owned(),
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
        let compiled = CompiledRule::new(rule).unwrap();

        // Cross-segment match: `bar/down/to/foo/too` ends in `foo/too` and
        // the `**` chews the intervening path.
        use std::path::Path;
        assert!(compiled.matches(Path::new("bar/down/to/foo/too"), true, true));
        // Basename-style: `foo/too` is the minimal cross-segment match.
        assert!(compiled.matches(Path::new("foo/too"), true, true));
        // In-segment form must still match - upstream `**` consumes zero
        // or more characters including `/`, so `fooxytoo` matches via the
        // empty-slice expansion.
        assert!(compiled.matches(Path::new("fooxytoo"), false, true));
        // Non-matching tail.
        assert!(!compiled.matches(Path::new("foo/bar"), false, true));
    }

    /// `**/bar` continues to match `bar` and `a/b/bar` after normalisation.
    /// Regression guard: leading `**/` must NOT be over-normalised.
    #[test]
    fn double_star_prefix_regression_guard() {
        let rule = FilterRule {
            action: FilterAction::Include,
            pattern: "**/bar".to_owned(),
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
        let compiled = CompiledRule::new(rule).unwrap();
        use std::path::Path;
        assert!(compiled.matches(Path::new("bar"), false, true));
        assert!(compiled.matches(Path::new("a/b/bar"), false, true));
        assert!(!compiled.matches(Path::new("baz"), false, true));
    }

    /// `bar/**` continues to match `bar/x/y` after normalisation.
    /// Regression guard: trailing `/**` must NOT be over-normalised.
    #[test]
    fn double_star_suffix_regression_guard() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "bar/**".to_owned(),
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
        let compiled = CompiledRule::new(rule).unwrap();
        use std::path::Path;
        assert!(compiled.matches(Path::new("bar/x"), false, true));
        assert!(compiled.matches(Path::new("bar/x/y"), false, true));
        // `bar` alone does NOT match `bar/**` - the `/` after `bar` is
        // mandatory in the pattern.
        assert!(!compiled.matches(Path::new("bar"), true, true));
    }

    /// Collects the source pattern strings of every direct matcher attached
    /// to `compiled` so wire-byte parity tests can assert exact matcher sets.
    fn direct_pattern_strings(compiled: &CompiledRule) -> Vec<String> {
        let mut out: Vec<String> = compiled
            .direct_matchers
            .iter()
            .map(|m| m.glob().to_string())
            .collect();
        out.sort();
        out
    }

    /// Collects the source pattern strings of every descendant matcher.
    fn descendant_pattern_strings(compiled: &CompiledRule) -> Vec<String> {
        let mut out: Vec<String> = compiled
            .descendant_matchers
            .iter()
            .map(|m| m.glob().to_string())
            .collect();
        out.sort();
        out
    }

    /// Collects the source pattern strings of every deletion-only descendant
    /// matcher (those that fire on the receiver delete pass but never on the
    /// transfer path).
    fn deletion_descendant_pattern_strings(compiled: &CompiledRule) -> Vec<String> {
        let mut out: Vec<String> = compiled
            .deletion_descendant_matchers
            .iter()
            .map(|m| m.glob().to_string())
            .collect();
        out.sort();
        out
    }

    fn make_exclude(pattern: &str) -> CompiledRule {
        CompiledRule::new(FilterRule {
            action: FilterAction::Exclude,
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
        })
        .unwrap()
    }

    /// UTS-DD-exclude.5 regression: an unanchored pattern that does NOT
    /// contain `**` must still get the implicit `**/` prefix variant so it
    /// matches at any depth, mirroring upstream's match-the-name handling
    /// for `!u.slash_cnt && !FILTRULE_WILD2` (exclude.c:917-922).
    #[test]
    fn implicit_double_star_prefix_added_for_plain_pattern() {
        let compiled = make_exclude("bar");
        assert_eq!(direct_pattern_strings(&compiled), vec!["**/bar", "bar"]);
        assert_eq!(
            descendant_pattern_strings(&compiled),
            vec!["**/bar/**", "bar/**"]
        );
    }

    /// An unanchored pattern with interior `**` (e.g. `foo/**/bar`) still
    /// needs the implicit `**/` prefix variant. The leading literal `foo`
    /// anchors the pattern to the path root absent the prefix, so without
    /// `**/foo/**/bar` the matcher cannot tail-match `xx/foo/yy/bar`.
    /// Upstream's `wildmatch_array(..., slash_handling=-1)`
    /// (lib/wildmatch.c:316, exclude.c:952-956) tries the pattern after
    /// every slash, which is wire-equivalent to the `**/` prefixed
    /// variant.
    #[test]
    fn implicit_double_star_prefix_added_for_interior_double_star_pattern() {
        let compiled = make_exclude("foo/**/bar");
        assert_eq!(
            direct_pattern_strings(&compiled),
            vec!["**/foo/**/bar", "foo/**/bar"]
        );
        assert_eq!(
            descendant_pattern_strings(&compiled),
            vec!["**/foo/**/bar/**", "foo/**/bar/**"]
        );
    }

    /// `**/baz` already has the recursive prefix; we must not double it
    /// up into `**/**/baz` (which globset would collapse but still pollutes
    /// the matcher set).
    #[test]
    fn implicit_double_star_prefix_skipped_for_leading_double_star() {
        let compiled = make_exclude("**/baz");
        assert_eq!(direct_pattern_strings(&compiled), vec!["**/baz"]);
        assert_eq!(descendant_pattern_strings(&compiled), vec!["**/baz/**"]);
    }

    /// Unanchored pattern with an interior slash but no `**` still gets the
    /// `**/` variant. Upstream's slash_cnt-based tail matching
    /// (exclude.c:947-951) is wire-equivalent to globset's `**/foo/bar`.
    #[test]
    fn implicit_double_star_prefix_added_for_unanchored_slash_pattern() {
        let compiled = make_exclude("foo/bar");
        assert_eq!(
            direct_pattern_strings(&compiled),
            vec!["**/foo/bar", "foo/bar"]
        );
    }

    /// Trailing `**` (e.g., `foo/**`) is unanchored: the leading literal
    /// `foo` still anchors the pattern to the path root absent the
    /// implicit `**/` prefix. The trailing `**` only covers descent under
    /// `foo`, not the placement of `foo` itself, so `**/foo/**` is
    /// required to tail-match nested directories like `xx/foo/yy`.
    #[test]
    fn implicit_double_star_prefix_added_for_trailing_double_star_pattern() {
        let compiled = make_exclude("foo/**");
        assert_eq!(
            direct_pattern_strings(&compiled),
            vec!["**/foo/**", "foo/**"]
        );
    }

    /// UTS-DD-exclude.3 wire-byte parity: `foo/*/` is directory-only,
    /// unanchored, and wildcard-bearing. The direct matcher set must
    /// be exactly `{foo/*, **/foo/*}` (tail-matching for upstream's
    /// `slash_handling = -1` `wildmatch_array` over the user-written
    /// pattern). The descendant matcher set MUST be empty - upstream
    /// emits no `foo/*/**` rule because `exclude.c:rule_matches()`
    /// returns "no match" for FILTRULE_DIRECTORY against a non-dir
    /// candidate, regardless of which decision context dispatched.
    #[test]
    fn dir_only_unanchored_wildcard_exact_matcher_set() {
        let compiled = make_exclude("foo/*/");
        assert_eq!(direct_pattern_strings(&compiled), vec!["**/foo/*", "foo/*"]);
        assert!(
            descendant_pattern_strings(&compiled).is_empty(),
            "`foo/*/` must not synthesise transfer descendant matchers (upstream FILTRULE_DIRECTORY semantic)"
        );
        // The `{core}/**` descendant is routed to the deletion-only set so the
        // receiver protects children of the excluded directory (exclude /
        // exclude-lsh over-deletion regression) without re-exposing `foo/*/**`
        // to the transfer path (#6015).
        assert_eq!(
            deletion_descendant_pattern_strings(&compiled),
            vec!["**/foo/*/**", "foo/*/**"],
        );
    }

    /// The deletion-only descendant matcher must fire only on the deletion
    /// path. `matches_for_deletion` sees `foo/sub/file1`; the transfer
    /// `matches` (any `check_descendants`) must not (#6015 guard).
    #[test]
    fn deletion_descendant_matches_only_for_deletion() {
        use std::path::Path;
        let compiled = make_exclude("foo/*/");
        let child = Path::new("foo/sub/file1");

        assert!(
            compiled.matches_for_deletion(child, false, false),
            "foo/*/ deletion path must protect foo/sub/file1",
        );
        assert!(
            !compiled.matches(child, false, true),
            "foo/*/ transfer path must NOT match foo/sub/file1 even with check_descendants",
        );
        assert!(!compiled.matches(child, false, false));
    }

    /// UTS-DD-exclude.3 wire-byte parity for `**/node_modules/`: leading
    /// `**` already carries the recursive prefix, so direct stays
    /// `{**/node_modules}` with no descendant `**/node_modules/**`.
    #[test]
    fn dir_only_unanchored_double_star_prefix_exact_matcher_set() {
        let compiled = make_exclude("**/node_modules/");
        assert_eq!(direct_pattern_strings(&compiled), vec!["**/node_modules"]);
        assert!(descendant_pattern_strings(&compiled).is_empty());
    }

    /// UTS-DD-exclude.3 negative guard: a literal directory-only
    /// unanchored pattern (`cache/`) is NOT covered by the dir-only
    /// unanchored gate (no wildcard component), so descendants are
    /// still synthesised for the receiver single-path API. Keeps the
    /// gate scoped to wildcard patterns.
    #[test]
    fn dir_only_unanchored_literal_still_gets_descendants() {
        let compiled = make_exclude("cache/");
        assert_eq!(
            descendant_pattern_strings(&compiled),
            vec!["**/cache/**", "cache/**"]
        );
    }

    /// Differential-fuzzer regression: a `*/***/` exclude must match a
    /// top-level directory, mirroring upstream's `FILTRULE_WILD3_SUFFIX` which
    /// appends `/` to a directory candidate (`exclude.c:936-937`). The
    /// trailing-dir-slash + `/***` combination must collapse to the
    /// directory-only stem `*`; before the `normalise_pattern` reorder oc-rsync
    /// kept `*/***` and failed to match a slashless directory name, so the dir
    /// was wrongly included where upstream excludes it.
    #[test]
    fn wild3_suffix_with_trailing_dir_slash_matches_top_level_dir() {
        use std::path::Path;
        let compiled = make_exclude("*/***/");
        // A top-level directory is excluded (upstream parity).
        assert!(compiled.matches(Path::new("0"), true, true));
        assert!(compiled.matches(Path::new("anydir"), true, true));
        // Its contents are excluded on the deletion path.
        assert!(compiled.matches_for_deletion(Path::new("0/child"), false, true));
        // A top-level *file* is not a directory, so the directory-only stem
        // does not match it.
        assert!(!compiled.matches(Path::new("0"), false, true));
    }

    /// Differential-fuzzer regression: an anchored `/**/*` must NOT match a
    /// top-level single-component entry. The stem `**/*` begins with `**` after
    /// the leading-`/` strip, but the rule is anchored, so it is NOT a
    /// `FILTRULE_WILD2_PREFIX` rule and the candidate is matched without a
    /// prepended `/`. `**/*` then needs a `/` in the path, which `1` lacks -
    /// verified against upstream `rsync -rn --filter='- /**/*'` which includes
    /// a top-level `1`. An unanchored `**/*` (WILD2_PREFIX) DOES match it.
    #[test]
    fn anchored_double_star_does_not_match_top_level_entry() {
        use std::path::Path;
        // Anchored `/**/*` does not match a top-level `1` (no WILD2_PREFIX).
        let anchored = make_exclude("/**/*");
        assert!(!anchored.matches(Path::new("1"), false, true));
        // Unanchored `**/*` (WILD2_PREFIX) does match the top-level `1`.
        let unanchored = make_exclude("**/*");
        assert!(unanchored.matches(Path::new("1"), false, true));
    }

    #[test]
    fn compiled_rule_negate_flag_preserved() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: true,
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
            abs_path: false,
            word_split: false,
            no_prefixes: false,
            no_prefixes_include: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.negate);

        let rule2 = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_owned(),
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
        let compiled2 = CompiledRule::new(rule2).unwrap();
        assert!(!compiled2.negate);
    }
}
