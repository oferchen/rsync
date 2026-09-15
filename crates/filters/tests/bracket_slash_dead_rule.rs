//! A `/` inside a `[...]` bracket expression counts toward upstream rsync's
//! `slash_cnt` (exclude.c:292-294 `add_rule()`), even though a bracket can never
//! match a `/` (lib/wildmatch.c:247 `dowild()`). For an unanchored, non-`**`
//! rule upstream then requires the candidate to have `slash_cnt + 1` path
//! components (exclude.c:1046-1050 `rule_matches()` picks
//! `slash_handling = slash_cnt + 1`, and lib/wildmatch.c `trailing_N_elements`
//! returns NULL when the name has fewer components). Because the bracket cannot
//! consume the extra separator, such a rule can never match anything - it is a
//! dead rule that keeps every candidate.
//!
//! The overnight `filter_rules_vs_upstream` differential fuzzer surfaced this
//! as `path="U" oc=false upstream=true` for the rule `- [/U]`: oc excluded the
//! top-level file `U` where upstream keeps it. The oracle rows below are
//! measured against real rsync 3.5.0
//! (`rsync --dry-run --recursive --list-only --no-h --filter=<rule> src/`).

use std::path::Path;

use filters::{FilterSet, parse_rules};

/// Mirror the differential fuzzer's traversal-driven verdict: a leaf is listed
/// only when it and every ancestor directory survive the filter chain
/// (`fuzz/fuzz_targets/filter_rules_vs_upstream.rs::oc_walk_allows`).
fn oc_walk_allows(set: &FilterSet, rel_path: &str, is_dir: bool) -> bool {
    let components: Vec<&str> = rel_path.split('/').filter(|s| !s.is_empty()).collect();
    let mut prefix = String::new();
    for (idx, component) in components.iter().enumerate() {
        if idx > 0 {
            prefix.push('/');
        }
        prefix.push_str(component);
        let is_leaf = idx + 1 == components.len();
        let component_is_dir = if is_leaf { is_dir } else { true };
        if !set.allows(Path::new(&prefix), component_is_dir) {
            return false;
        }
    }
    true
}

fn set_for(rule_line: &str) -> FilterSet {
    let parsed = parse_rules(rule_line, Path::new("<test>")).expect("rule parses");
    FilterSet::from_rules(parsed).expect("rule compiles")
}

/// `(leaf, is_dir)` candidates matching the fuzzer fixture tree.
const CANDIDATES: &[(&str, bool)] = &[
    ("U", false),
    ("UU", false),
    ("x", true),
    ("x/U", false),
    ("a", true),
    ("a/b", true),
    ("a/b/U", false),
];

/// Each row is `(rule, [included; 7])` in `CANDIDATES` order, measured against
/// rsync 3.5.0.
const ORACLE: &[(&str, [bool; 7])] = &[
    // Unanchored, non-`**` rules whose only extra slash is inside the bracket:
    // dead rules upstream - every candidate survives.
    ("- [/U]", [true, true, true, true, true, true, true]),
    ("- [/U]/", [true, true, true, true, true, true, true]),
    ("- [/U]*", [true, true, true, true, true, true, true]),
    ("- a[/b]", [true, true, true, true, true, true, true]),
    ("- foo/[/U]", [true, true, true, true, true, true, true]),
    ("- U[/a]b", [true, true, true, true, true, true, true]),
    // Anchored form is NOT dead: slash_handling = 0, so it excludes the root
    // `U` only (exclude.c:1041-1044 strips the leading `/`).
    ("- /[/U]", [false, true, true, true, true, true, true]),
    // A `**` (WILD2) rule uses slash_handling = -1/0, not slash_cnt + 1, so the
    // bracket slash does not make it dead.
    ("- **[/U]", [false, false, true, false, true, true, false]),
    ("- [/U]**", [false, false, true, false, true, true, false]),
    // Control: the same class without the bracket slash is an ordinary
    // basename exclude.
    ("- [U]", [false, true, true, false, true, true, false]),
];

#[test]
fn bracket_internal_slash_matches_upstream() {
    let mut failures = Vec::new();
    for (rule, expected) in ORACLE {
        let set = set_for(rule);
        for (idx, (leaf, is_dir)) in CANDIDATES.iter().enumerate() {
            let got = oc_walk_allows(&set, leaf, *is_dir);
            if got != expected[idx] {
                failures.push(format!(
                    "rule {rule:?} path {leaf:?} is_dir={is_dir}: oc={got} upstream={}",
                    expected[idx]
                ));
            }
        }
    }
    assert!(failures.is_empty(), "divergences:\n{}", failures.join("\n"));
}
