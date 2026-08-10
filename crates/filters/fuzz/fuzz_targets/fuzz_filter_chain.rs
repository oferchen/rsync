#![no_main]

//! Fuzz target for filter chain evaluation.
//!
//! Builds a `FilterSet` from arbitrary rules and evaluates arbitrary paths
//! against it. This exercises the compiled glob matching, first-match-wins
//! evaluation, and the protect/risk chain simultaneously.
//!
//! Uses `arbitrary::Arbitrary` to generate structured (rules, paths) pairs
//! rather than raw bytes, giving the fuzzer better coverage of meaningful
//! rule combinations.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::path::Path;

/// Structured fuzzer input: a set of rules and paths to evaluate.
#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// Raw rule lines in merge-file format.
    rule_text: String,
    /// Paths to evaluate against the compiled filter set.
    paths: Vec<PathEntry>,
}

/// A path with its directory flag for filter evaluation.
#[derive(Arbitrary, Debug)]
struct PathEntry {
    path: String,
    is_dir: bool,
}

fuzz_target!(|input: FuzzInput| {
    let source = Path::new("<fuzz>");

    // Parse rules from arbitrary text - must not panic.
    let rules = match filters::parse_rules(&input.rule_text, source) {
        Ok(r) => r,
        Err(_) => return,
    };

    // Compile into a FilterSet - glob errors are acceptable, panics are not.
    let set = match filters::FilterSet::from_rules(rules) {
        Ok(s) => s,
        Err(_) => return,
    };

    // Evaluate each path against all decision methods - must not panic.
    for entry in &input.paths {
        let path = Path::new(&entry.path);
        let _ = set.allows(path, entry.is_dir);
        let _ = set.allows_deletion(path, entry.is_dir);
        let _ = set.allows_deletion_when_excluded_removed(path, entry.is_dir);
        let _ = set.excluded_dir_by_non_dir_rule(path);
    }

    // Also test is_empty for completeness.
    let _ = set.is_empty();
});
