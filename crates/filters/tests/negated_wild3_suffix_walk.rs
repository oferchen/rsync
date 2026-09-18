//! A `!`-negated rule inverts ONE wildmatch of its whole pattern
//! (exclude.c:1005 `ret_match = rflags & FILTRULE_NEGATE ? 0 : 1`, returned on
//! a match and negated on a miss). When the pattern ends in `/***`
//! (`FILTRULE_WILD3_SUFFIX`, exclude.c:340-345) that single wildmatch spans TWO
//! things: the directory itself - `rule_matches()` appends `/` to a directory
//! name so `dir/***` matches `dir/` (exclude.c:1033-1036) - and every
//! descendant, because the trailing `***` crosses `/` in `lib/wildmatch.c`.
//!
//! oc peels the `/***` into a directory-only stem plus a synthetic `stem/**`
//! descendant matcher. The descendant set is otherwise a subtree-pruning
//! emulation that single-path/deletion queries consult and the sender walk does
//! not; for a `!`-negated WILD3 rule there is no subtree to prune (its action
//! fires on NON-match), so the descendant reach must be part of the value that
//! negate inverts - otherwise oc inverts against the directory stem alone and
//! over-excludes a file the pattern actually matches.
//!
//! The overnight `filter_differential` fuzzer surfaced this as
//! `path="I/0" is_dir=false oc=false upstream=true` for the rule `-!p /?*/***`:
//! oc excluded the file `I/0` where upstream keeps it. The oracle rows below are
//! measured against real rsync 3.5.0 (`rsync --dry-run --recursive --verbose
//! --out-format=I:%n --filter=<rule> src/ dst/`, one candidate materialised per
//! run, exactly as `fuzz/fuzz_targets/filter_differential.rs` drives it).

use std::path::Path;

use filters::{FilterSet, parse_rules};

/// Mirror the differential fuzzer's traversal-driven verdict: a leaf is listed
/// only when it and every ancestor directory survive the filter chain
/// (`fuzz/fuzz_targets/filter_differential.rs::oc_walk_allows`).
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

/// `(leaf, is_dir)` candidates. `I/0` is the fuzzer crash leaf; the rest
/// discriminate the mechanism (a single-segment name the pattern cannot match,
/// a deep descendant, and the directory forms).
const CANDIDATES: &[(&str, bool)] = &[
    ("I", false),
    ("I/0", false),
    ("I/a/b", false),
    ("ab/cd", false),
    ("f", false),
    ("x", true),
    ("I", true),
];

/// Each row is `(rule, [included; 7])` in `CANDIDATES` order, measured against
/// rsync 3.5.0.
const ORACLE: &[(&str, [bool; 7])] = &[
    // The crash rule. Negated exclude + WILD3: the directory (`I`, `x`) matches
    // `?*/***` via the appended `/`, so negate keeps it and the walk descends;
    // a file two-plus segments deep also matches, so it survives too. Only a
    // single-segment name the pattern can never reach (`I` as a file, `f`) is
    // inverted into an exclude.
    ("-!p /?*/***", [false, true, true, true, false, true, true]),
    // Same rule without the perishable modifier - `p` never changes a plain
    // transfer verdict, so the row is identical. Guards the fix against keying
    // on the modifier text rather than the WILD3 suffix.
    ("-! /?*/***", [false, true, true, true, false, true, true]),
    // Non-negated control: the WILD3 rule excludes the directory and every
    // descendant, keeping only the names it cannot match.
    ("- /?*/***", [true, false, false, false, true, false, false]),
    // `/?*/**` is NOT a WILD3 rule (it ends in `**`, not `***`), so the
    // directory name is matched WITHOUT the trailing `/`; `?*/**` cannot match a
    // bare directory, so the negated rule excludes it and prunes everything.
    // This is the discriminating control that separates WILD3 from plain `**`.
    (
        "-!p /?*/**",
        [false, false, false, false, false, false, false],
    ),
    // Negated include + WILD3 keeps everything: names the pattern matches are
    // included outright, and names it misses are inverted into an include.
    ("+!p /?*/***", [true, true, true, true, true, true, true]),
];

#[test]
fn negated_wild3_suffix_matches_upstream() {
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
