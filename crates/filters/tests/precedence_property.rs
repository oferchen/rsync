//! Property-based tests that lock the FilterChain precedence and modifier
//! invariants needed for upstream-rsync compatibility.
//!
//! These tests complement `proptest_rule_evaluation.rs` (basic anchoring,
//! dir-only, wildcard semantics) and `proptest_fuzz.rs` (panic-freedom).
//! They focus on properties that a refactor of `FilterChain`/`FilterSet`
//! must preserve, with reference to upstream behaviour:
//!
//! - First-match-wins across arbitrary-length, mixed include/exclude
//!   sequences. Mirrors `exclude.c:check_filter()` (line 1043) which
//!   walks the rule list and returns on the first match.
//! - Negation (`!` modifier) inverts the match outcome. Mirrors
//!   `exclude.c:rule_matches()` line 906:
//!   `int ret_match = ex->rflags & FILTRULE_NEGATE ? 0 : 1;`
//! - Double negation is a no-op.
//! - Sender-only include (`show`) and sender-only exclude (`hide`)
//!   participate in the transfer-side decision used by [`FilterSet::allows`].
//!   Long forms map to action chars `S`/`H` per `exclude.c` lines 1151/1171.
//! - Protect/Risk decisions follow the same first-match-wins rule on a
//!   separate chain, per the second `check_filter()` call site.
//! - Anchoring and dir-only modifiers compose with both `include` and
//!   `exclude` actions consistently.

use std::path::Path;

use filters::{FilterRule, FilterSet};
use proptest::prelude::*;

/// Lowercase ASCII path segment (1-6 chars). Uses a small alphabet so generated
/// patterns and paths produce frequent matches and non-matches.
fn segment() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop::sample::select(vec!['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h']),
        1..7,
    )
    .prop_map(|v| v.into_iter().collect::<String>())
}

/// A relative path of 1-4 segments joined by `/`.
fn rel_path() -> impl Strategy<Value = String> {
    proptest::collection::vec(segment(), 1..5).prop_map(|s| s.join("/"))
}

/// A simple non-anchored, non-dir-only pattern that may include `*` for
/// extension-style matching. Kept literal-friendly so we can reason about
/// matches directly.
fn simple_pattern() -> impl Strategy<Value = String> {
    prop_oneof![
        // exact name: `foo`
        segment(),
        // extension wildcard: `*.EXT`
        segment().prop_map(|ext| format!("*.{ext}")),
    ]
}

// ---------------------------------------------------------------------------
// Property: first-match-wins across arbitrary-length include/exclude sequences
// ---------------------------------------------------------------------------
//
// upstream: exclude.c:1043 - check_filter() walks the rule list and returns
// the first matching include/exclude as the decision.

/// Reference oracle: walks `rules` in order, returning the first
/// include/exclude rule whose pattern matches `path`. Panics on rules whose
/// action is not Include/Exclude - those are filtered before calling.
fn oracle_first_match(rules: &[FilterRule], path: &Path, is_dir: bool) -> Option<bool> {
    for rule in rules {
        // Build a single-rule FilterSet to reuse the production matcher; this
        // isolates "did this rule match?" from "what is the chain's decision?".
        let probe = FilterSet::from_rules([rule.clone()]).ok()?;
        // A single-rule set's decision differs from default only when the
        // rule actually matched. For Include rules, `allows` stays true on a
        // match; for Exclude rules it flips to false. We detect the match by
        // observing a flip from the default.
        let default_allows = true;
        let observed = probe.allows(path, is_dir);
        if observed != default_allows {
            // Exclude rule matched - first match wins, decision is false.
            return Some(false);
        }
        // For Include rules, a match keeps `allows == true`, which is also
        // the default. To distinguish a match-include from no-match, place
        // an explicit catch-all exclude after it.
        let probe_with_catchall =
            FilterSet::from_rules([rule.clone(), FilterRule::exclude("**")]).ok()?;
        if probe_with_catchall.allows(path, is_dir) {
            // The include matched (otherwise the catch-all would have excluded).
            return Some(true);
        }
    }
    None
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// For an arbitrary-length sequence of include/exclude rules with simple
    /// patterns, `FilterSet::allows` must agree with the first-match-wins
    /// oracle (or with the default `true` when no rule matches).
    #[test]
    fn first_match_wins_arbitrary_sequence(
        actions in proptest::collection::vec(any::<bool>(), 1..8),
        patterns in proptest::collection::vec(simple_pattern(), 1..8),
        path in rel_path(),
        is_dir in any::<bool>(),
    ) {
        // Pair up the two vectors at the shorter length.
        let n = actions.len().min(patterns.len());
        let rules: Vec<FilterRule> = actions
            .iter()
            .zip(patterns.iter())
            .take(n)
            .map(|(is_include, pat)| {
                if *is_include {
                    FilterRule::include(pat)
                } else {
                    FilterRule::exclude(pat)
                }
            })
            .collect();

        let set = FilterSet::from_rules(rules.clone()).unwrap();
        let actual = set.allows(Path::new(&path), is_dir);

        let expected = oracle_first_match(&rules, Path::new(&path), is_dir).unwrap_or(true);
        prop_assert_eq!(
            actual, expected,
            "rules={:?} path={} is_dir={}", rules, path, is_dir
        );
    }
}

// ---------------------------------------------------------------------------
// Property: rule-order monotonicity for first-match-wins
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Prepending a matching include before any later rules guarantees the
    /// path is allowed. Prepending a matching exclude guarantees it is denied.
    /// This is the defining behaviour of first-match-wins.
    #[test]
    fn prepended_match_dominates_tail(
        ext in segment(),
        name in segment(),
        tail in proptest::collection::vec(simple_pattern(), 0..5),
    ) {
        let path = format!("{name}.{ext}");
        let target = Path::new(&path);

        // Prepend an include that definitely matches this file, followed by
        // an arbitrary tail. The decision must be: allowed.
        let mut inc_rules = vec![FilterRule::include(format!("*.{ext}"))];
        inc_rules.extend(tail.iter().cloned().map(FilterRule::exclude));
        let inc_set = FilterSet::from_rules(inc_rules).unwrap();
        prop_assert!(
            inc_set.allows(target, false),
            "prepended include *.{} must dominate tail for {}", ext, path
        );

        // Prepend an exclude that definitely matches this file, followed by
        // an arbitrary tail of includes. The decision must be: denied.
        let mut exc_rules = vec![FilterRule::exclude(format!("*.{ext}"))];
        exc_rules.extend(tail.iter().cloned().map(FilterRule::include));
        let exc_set = FilterSet::from_rules(exc_rules).unwrap();
        prop_assert!(
            !exc_set.allows(target, false),
            "prepended exclude *.{} must dominate tail for {}", ext, path
        );
    }
}

// ---------------------------------------------------------------------------
// Property: negation (`!` / FILTRULE_NEGATE) inverts the match outcome
// ---------------------------------------------------------------------------
//
// upstream: exclude.c:906
//   int ret_match = ex->rflags & FILTRULE_NEGATE ? 0 : 1;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// `! - PATTERN` (negated exclude) excludes everything EXCEPT files
    /// matching PATTERN. A negated exclude followed by a catch-all include
    /// means: only PATTERN passes through.
    #[test]
    fn negated_exclude_excludes_non_matches(
        ext in segment(),
        other_ext in segment(),
        name in segment(),
    ) {
        prop_assume!(ext != other_ext);

        let set = FilterSet::from_rules([
            FilterRule::exclude(format!("*.{ext}")).with_negate(true),
            FilterRule::include("**"),
        ])
        .unwrap();

        // Matching file: NOT excluded by the negated rule, then include
        // catch-all keeps it. -> allowed.
        let matching = format!("{name}.{ext}");
        prop_assert!(
            set.allows(Path::new(&matching), false),
            "negated exclude *.{} must allow matching {}", ext, matching
        );

        // Non-matching file: excluded by the negated rule (first-match-wins).
        let other = format!("{name}.{other_ext}");
        prop_assert!(
            !set.allows(Path::new(&other), false),
            "negated exclude *.{} must exclude non-match {}", ext, other
        );
    }

    /// Double negation is a no-op: `with_negate(true).with_negate(false)` is
    /// equivalent to never negating at all.
    #[test]
    fn double_negation_is_identity(
        ext in segment(),
        name in segment(),
    ) {
        let path = format!("{name}.{ext}");

        let plain = FilterSet::from_rules([FilterRule::exclude(format!("*.{ext}"))])
            .unwrap();
        let double = FilterSet::from_rules([
            FilterRule::exclude(format!("*.{ext}"))
                .with_negate(true)
                .with_negate(false),
        ])
        .unwrap();

        prop_assert_eq!(
            plain.allows(Path::new(&path), false),
            double.allows(Path::new(&path), false),
            "double negation must equal no negation for {}", path
        );
    }

    /// For any rule R and path P, the negated rule's match outcome must be
    /// the inverse of the non-negated rule's match outcome. We probe match
    /// outcomes by using exclude rules: a non-match leaves `allows == true`,
    /// a match flips it to `false`.
    #[test]
    fn negate_inverts_match_outcome(
        pat in simple_pattern(),
        path in rel_path(),
        is_dir in any::<bool>(),
    ) {
        let plain = FilterSet::from_rules([FilterRule::exclude(&pat)]).unwrap();
        let negated = FilterSet::from_rules([
            FilterRule::exclude(&pat).with_negate(true),
        ])
        .unwrap();

        let plain_allows = plain.allows(Path::new(&path), is_dir);
        let neg_allows = negated.allows(Path::new(&path), is_dir);

        // A path is excluded by the plain rule iff it is allowed by the
        // negated rule, and vice versa. (Default allow is `true`.)
        prop_assert_ne!(
            plain_allows, neg_allows,
            "negate must invert outcome for pat={} path={} is_dir={}",
            pat, path, is_dir
        );
    }
}

// ---------------------------------------------------------------------------
// Property: show / hide are sender-side include / exclude
// ---------------------------------------------------------------------------
//
// upstream: exclude.c:1151 'hide' -> 'H', 1171 'show' -> 'S'. Both set
// FILTRULE_SENDER_SIDE. The sender-side transfer decision (used by
// `FilterSet::allows`) sees them; the receiver-side deletion decision does
// not.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// `hide PATTERN` excludes from the sender-side transfer, identically
    /// to a sender-only `exclude`.
    #[test]
    fn hide_excludes_from_transfer(
        ext in segment(),
        name in segment(),
    ) {
        let path = format!("{name}.{ext}");
        let set = FilterSet::from_rules([FilterRule::hide(format!("*.{ext}"))]).unwrap();
        prop_assert!(
            !set.allows(Path::new(&path), false),
            "hide *.{} must exclude {} from transfer", ext, path
        );
    }

    /// `show PATTERN` placed before a catch-all hide allows the matching
    /// file through on the sender side.
    #[test]
    fn show_before_hide_catch_all_allows_match(
        ext in segment(),
        other_ext in segment(),
        name in segment(),
    ) {
        prop_assume!(ext != other_ext);

        let set = FilterSet::from_rules([
            FilterRule::show(format!("*.{ext}")),
            FilterRule::hide("*"),
        ])
        .unwrap();

        let matching = format!("{name}.{ext}");
        prop_assert!(
            set.allows(Path::new(&matching), false),
            "show *.{} before hide * must allow {}", ext, matching
        );

        let other = format!("{name}.{other_ext}");
        prop_assert!(
            !set.allows(Path::new(&other), false),
            "hide * must exclude non-shown {}", other
        );
    }

    /// `hide` does NOT participate in receiver-side deletion decisions;
    /// `allows_deletion` ignores sender-only rules. With only a hide rule,
    /// every path remains deletable (no protect, receiver-side allows by
    /// default).
    #[test]
    fn hide_does_not_block_deletion(
        ext in segment(),
        name in segment(),
        is_dir in any::<bool>(),
    ) {
        let set = FilterSet::from_rules([FilterRule::hide(format!("*.{ext}"))]).unwrap();
        let path = format!("{name}.{ext}");
        prop_assert!(
            set.allows_deletion(Path::new(&path), is_dir),
            "hide is sender-only and must not block deletion of {}", path
        );
    }
}

// ---------------------------------------------------------------------------
// Property: protect / risk first-match-wins on a separate chain
// ---------------------------------------------------------------------------
//
// upstream: exclude.c - protect_filter_list is a distinct rule chain; the
// same first-match-wins evaluation in check_filter() applies independently.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Among `protect`/`risk` rules, the first matching wins. A `risk`
    /// placed before a `protect` for the same pattern keeps deletion
    /// allowed; reversing the order blocks deletion.
    #[test]
    fn protect_risk_first_match_wins(
        name in segment(),
    ) {
        let path = Path::new(&name);
        let pat = format!("/{name}");

        // risk before protect -> deletion allowed.
        let risk_first = FilterSet::from_rules([
            FilterRule::risk(&pat),
            FilterRule::protect(&pat),
        ])
        .unwrap();
        prop_assert!(
            risk_first.allows_deletion(path, false),
            "risk before protect for {} must allow deletion", pat
        );

        // protect before risk -> deletion blocked.
        let protect_first = FilterSet::from_rules([
            FilterRule::protect(&pat),
            FilterRule::risk(&pat),
        ])
        .unwrap();
        prop_assert!(
            !protect_first.allows_deletion(path, false),
            "protect before risk for {} must block deletion", pat
        );
    }

    /// Multiple unrelated protect rules don't interfere: a path that
    /// matches none of them remains deletable.
    #[test]
    fn unrelated_protects_do_not_block(
        protected in segment(),
        unprotected in segment(),
    ) {
        prop_assume!(protected != unprotected);

        let set = FilterSet::from_rules([
            FilterRule::protect(format!("/{protected}")),
        ])
        .unwrap();

        prop_assert!(
            set.allows_deletion(Path::new(&unprotected), false),
            "protect /{} must not block deletion of unrelated {}",
            protected, unprotected
        );
    }
}

// ---------------------------------------------------------------------------
// Property: anchoring and dir-only modifiers compose with both actions
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// An anchored exclude `/NAME` blocks the root entry; the same anchored
    /// pattern on an include rule, placed before a catch-all exclude, lets
    /// only that one root entry through. Both demonstrate that the leading
    /// `/` anchors regardless of action.
    #[test]
    fn anchoring_modifier_acts_on_both_actions(
        name in segment(),
        sibling in segment(),
        parent in segment(),
    ) {
        prop_assume!(name != sibling);
        prop_assume!(parent != name);

        // Anchored exclude excludes only the root entry.
        let exc = FilterSet::from_rules([
            FilterRule::exclude(format!("/{name}")),
        ])
        .unwrap();
        prop_assert!(
            !exc.allows(Path::new(&name), false),
            "anchored exclude /{} must block root {}", name, name
        );
        let nested = format!("{parent}/{name}");
        prop_assert!(
            exc.allows(Path::new(&nested), false),
            "anchored exclude /{} must NOT block nested {}", name, nested
        );

        // Anchored include before catch-all exclude allows only the root entry.
        let inc = FilterSet::from_rules([
            FilterRule::include(format!("/{name}")),
            FilterRule::exclude("**"),
        ])
        .unwrap();
        prop_assert!(
            inc.allows(Path::new(&name), false),
            "anchored include /{} must allow root {}", name, name
        );
        prop_assert!(
            !inc.allows(Path::new(&sibling), false),
            "catch-all exclude must block sibling {}", sibling
        );
    }

    /// Trailing `/` (dir-only) composes the same way for both include and
    /// exclude: only directories with the matching name are affected; files
    /// with the same name are not.
    #[test]
    fn dir_only_modifier_acts_on_both_actions(
        name in segment(),
    ) {
        // Dir-only exclude blocks the directory but not a file with the
        // same name.
        let exc = FilterSet::from_rules([
            FilterRule::exclude(format!("{name}/")),
        ])
        .unwrap();
        prop_assert!(
            !exc.allows(Path::new(&name), true),
            "{}/ as exclude must block directory", name
        );
        prop_assert!(
            exc.allows(Path::new(&name), false),
            "{}/ as exclude must NOT block file with same name", name
        );

        // Dir-only include before catch-all exclude lets the directory
        // through but not a file with the same name.
        let inc = FilterSet::from_rules([
            FilterRule::include(format!("{name}/")),
            FilterRule::exclude("**"),
        ])
        .unwrap();
        prop_assert!(
            inc.allows(Path::new(&name), true),
            "{}/ as include must allow directory", name
        );
        prop_assert!(
            !inc.allows(Path::new(&name), false),
            "{}/ as include must NOT allow file with same name", name
        );
    }
}

// ---------------------------------------------------------------------------
// Property: clear (`!`) wipes prior rules; subsequent rules act fresh
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// A clear rule between two contradictory rules keeps the second one's
    /// outcome regardless of what the first said. This is stronger than a
    /// single existing test - it exercises clear with arbitrary preceding
    /// rules.
    #[test]
    fn clear_isolates_subsequent_rules(
        prefix_rules in proptest::collection::vec(simple_pattern(), 0..5),
        ext in segment(),
        name in segment(),
    ) {
        let path = format!("{name}.{ext}");
        let target = Path::new(&path);

        // Build: <arbitrary excludes> CLEAR exclude(*.ext)
        let mut rules: Vec<FilterRule> = prefix_rules
            .iter()
            .cloned()
            .map(FilterRule::exclude)
            .collect();
        rules.push(FilterRule::clear());
        rules.push(FilterRule::exclude(format!("*.{ext}")));

        let set = FilterSet::from_rules(rules).unwrap();
        prop_assert!(
            !set.allows(target, false),
            "clear must isolate; trailing exclude *.{} must block {}", ext, path
        );

        // Same prefix but trailing rule is include over a catch-all exclude.
        let mut rules2: Vec<FilterRule> = prefix_rules
            .iter()
            .cloned()
            .map(FilterRule::exclude)
            .collect();
        rules2.push(FilterRule::clear());
        rules2.push(FilterRule::include(format!("*.{ext}")));
        rules2.push(FilterRule::exclude("**"));

        let set2 = FilterSet::from_rules(rules2).unwrap();
        prop_assert!(
            set2.allows(target, false),
            "clear must isolate; trailing include *.{} must allow {}", ext, path
        );
    }
}
