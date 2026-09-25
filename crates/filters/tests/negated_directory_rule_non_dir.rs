//! A `!`-negated rule written with a trailing `/` matches EVERY non-directory.
//!
//! upstream: exclude.c:287-290 add_rule() peels a trailing `/` into
//! `FILTRULE_DIRECTORY`. rule_matches() then returns `!ret_match` for a
//! non-directory before any wildmatch (exclude.c:1037-1038), and `ret_match` is
//! 0 under `!` (exclude.c:1005). So `-! pat/` excludes every file it is asked
//! about, whatever `pat` is - the pattern only decides for directories.
//!
//! A trailing `/***` (`FILTRULE_WILD3_SUFFIX`) does not change that: the `/`
//! is peeled first, the `/***` stays in the pattern, and the directory check
//! still fires before the wildmatch. oc folds both suffixes into one
//! `directory_only` stem, and a negated WILD3 rule consults its descendant
//! matcher so that `-! /?*/***` keeps `II/0` (#7907). Without the separate
//! written-slash record, the same reach made `-!p /?*/***/` keep `II/0` too,
//! where upstream excludes it.
//!
//! The overnight `filter_differential` fuzzer surfaced this as
//! `path="II/0" is_dir=false oc=true upstream=false` for `-!p /?*/***/`. The
//! oracle rows below are measured against real rsync 3.5.0 (`rsync --dry-run
//! --recursive --out-format=I:%n --filter=<rule> src/ dst/`, one candidate
//! materialised per run, as `fuzz/fuzz_targets/filter_differential.rs` does).

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

/// `(leaf, is_dir)` candidates. `II/0` as a file is the fuzzer crash leaf; the
/// directory forms show the pattern still decides for directories.
const CANDIDATES: &[(&str, bool)] = &[
    ("II/0", false),
    ("II/0", true),
    ("II", true),
    ("top", false),
    ("top", true),
];

/// Each row is `(rule, [included; 5])` in `CANDIDATES` order, measured against
/// rsync 3.5.0.
const ORACLE: &[(&str, [bool; 5])] = &[
    // The crash rule. The file `II/0` is excluded even though `?*/***` would
    // wildmatch it: the directory check fires first.
    ("-!p /?*/***/", [false, true, true, false, true]),
    // Same rule without the trailing `/` (#7907): no FILTRULE_DIRECTORY, so the
    // file IS wildmatched, matches, and negate keeps it. The pair isolates the
    // written slash as the only difference.
    ("-!p /?*/***", [true, true, true, false, true]),
    // Unanchored and literal WILD3 stems behave the same way.
    ("-! ?*/***/", [false, true, true, false, true]),
    ("-! /II/***/", [false, true, true, false, false]),
    ("-! II/***/", [false, true, true, false, false]),
    ("-! /II/***", [true, true, true, false, false]),
    // Plain directory rules, anchored and unanchored, wildcard and literal:
    // every file is excluded, directories follow the pattern.
    ("-! /?*/", [false, false, true, false, true]),
    ("-! ?*/", [false, true, true, false, true]),
    ("-! */", [false, true, true, false, true]),
    ("-! II/", [false, false, true, false, false]),
    ("-! /II/", [false, false, true, false, false]),
    // A pattern naming the file itself still cannot keep it.
    ("-! 0/", [false, false, false, false, false]),
    ("-! /II/0/", [false, false, false, false, false]),
    // Negated include: every file is included, so nothing is dropped.
    ("+! II/", [true, true, true, true, true]),
    // Non-negated controls: the directory flag makes a file a non-match, so
    // only files under an excluded directory disappear.
    ("- /?*/***/", [false, false, false, true, false]),
    ("- II/", [false, false, false, true, true]),
];

#[test]
fn negated_directory_rule_excludes_every_non_dir_like_upstream() {
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

/// The receiver's deletion scan asks the same question of each candidate
/// without a walk (exclude.c:1037-1038 applies there too), so a negated
/// directory rule must protect every non-directory from `--delete`.
#[test]
fn negated_directory_rule_excludes_non_dir_on_the_deletion_path() {
    for rule in ["-! /?*/***/", "-! II/", "-! /II/0/"] {
        let set = set_for(rule);
        assert!(
            !set.allows_deletion(Path::new("II/0"), false),
            "{rule}: a negated directory rule must match the file II/0"
        );
    }
}

/// A run of more than three `*` after the last `/` is the same `/***` suffix.
///
/// upstream: exclude.c:340-345 sets FILTRULE_WILD3_SUFFIX for any pattern
/// ending in `***`, and lib/wildmatch.c treats a run of three or more `*` as
/// one slash-crossing wildcard. So `/?*/*****` matches the directory `cI` via
/// the appended `/` exactly like `/?*/***`. The fuzzer found this next
/// (`-!p /?*/*****` vs the directory `cI`: oc=false upstream=true). Rows are
/// `(rule, [included; 5])` over `cI` dir, `cI` file, `cI/0` file, `cI/0` dir,
/// `cIx` dir, measured against rsync 3.5.0.
#[test]
fn long_trailing_star_run_after_slash_matches_like_wild3() {
    const RUN_CANDIDATES: &[(&str, bool)] = &[
        ("cI", true),
        ("cI", false),
        ("cI/0", false),
        ("cI/0", true),
        ("cIx", true),
    ];
    const RUN_ORACLE: &[(&str, [bool; 5])] = &[
        ("-!p /?*/*****", [true, false, true, true, true]),
        ("-! /?*/****", [true, false, true, true, true]),
        ("- /?*/*****", [false, true, false, false, false]),
        ("-! cI/****", [true, false, true, true, false]),
        ("- /cI/****", [false, true, false, false, true]),
        // The written `/` still wins for a file (exclude.c:1037-1038).
        ("-! /?*/*****/", [true, false, false, true, true]),
    ];
    let mut failures = Vec::new();
    for (rule, expected) in RUN_ORACLE {
        let set = set_for(rule);
        for (idx, (leaf, is_dir)) in RUN_CANDIDATES.iter().enumerate() {
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
