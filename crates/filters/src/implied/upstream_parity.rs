//! Differential parity of the implied-include rules against a line-by-line
//! model of rsync 3.5.2's `add_implied_include()`.
//!
//! oc builds the same rule set by a different route: it normalises by path
//! segment instead of upstream's byte loop, and its compiled rules choose
//! literal or wildcard matching from the pattern text instead of from the
//! arg's `saw_wild` flag. These tests hold both routes to upstream's
//! accept/refuse verdict for every candidate name, under every combination of
//! `--relative`, `--recursive`, `--dirs` and the daemon module strip.
//!
//! upstream: `exclude.c:409-447` `maybe_add_literal_brackets_rule()`,
//! `exclude.c:476-667` `add_implied_include()`, `exclude.c:1002-1069`
//! `rule_matches()` (anchored rules: `wildmatch` when `FILTRULE_WILD`,
//! `strcmp` otherwise; `FILTRULE_DIRECTORY` matches directories only).

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::{ImpliedIncludeOptions, ImpliedIncludes};
use crate::wildmatch;

/// One rule of upstream's `implied_filter_list`.
#[derive(Debug, Clone)]
struct ModelRule {
    pattern: Vec<u8>,
    wild: bool,
    directory: bool,
}

/// upstream: `exclude.c:409-447` `maybe_add_literal_brackets_rule()`.
fn model_literal_brackets(based_on: &ModelRule, list: &mut Vec<ModelRule>) {
    let arg = &based_on.pattern;
    let mut cnt = 0;
    let mut i = 0;
    while i < arg.len() {
        if arg[i] == b'\\' && i + 1 < arg.len() {
            i += 1;
        } else if arg[i] == b'[' {
            cnt += 1;
        }
        i += 1;
    }
    if cnt == 0 {
        return;
    }
    let mut pattern = Vec::with_capacity(arg.len() + cnt);
    let mut i = 0;
    while i < arg.len() {
        if arg[i] == b'\\' && i + 1 < arg.len() {
            pattern.push(arg[i]);
            i += 1;
        } else if arg[i] == b'[' {
            pattern.push(b'\\');
        }
        pattern.push(arg[i]);
        i += 1;
    }
    list.push(ModelRule {
        pattern,
        wild: based_on.wild,
        directory: based_on.directory,
    });
}

/// upstream: `exclude.c:476-667` `add_implied_include()`, with
/// `trust_sender_args` off and no partial string pending.
fn model_add(list: &mut Vec<ModelRule>, arg: &[u8], opts: ImpliedIncludeOptions) {
    let mut arg = arg;
    if opts.skip_daemon_module {
        arg = arg
            .iter()
            .position(|&b| b == b'/')
            .map_or(&b""[..], |i| &arg[i + 1..]);
    }
    if opts.relative {
        if let Some(i) = arg.windows(3).position(|w| w == b"/./") {
            arg = &arg[i + 3..];
        }
    } else if let Some(i) = arg.iter().rposition(|&b| b == b'/') {
        arg = &arg[i + 1..];
    }
    if arg == b"." {
        arg = b"";
    }

    let mut saw_wild = false;
    let mut saw_live_open_brkt = false;
    let mut backslash_cnt = 0;
    let mut new_pat: Vec<u8> = Vec::new();

    if !arg.is_empty() {
        saw_wild = arg.iter().any(|b| b"*[?".contains(b));
        let at = |k: usize| arg.get(k).copied().unwrap_or(0);
        let mut slash_cnt: i32 = 1;
        new_pat.push(b'/');
        let mut cp = 0;
        while cp < arg.len() {
            match arg[cp] {
                b'\\' => {
                    if at(cp + 1) == b']' {
                        if !saw_wild {
                            cp += 1;
                        }
                    } else if at(cp + 1) == 0 || !b"*[?".contains(&at(cp + 1)) {
                        backslash_cnt += 1;
                        if saw_wild {
                            new_pat.push(b'\\');
                        }
                    }
                    new_pat.push(arg[cp]);
                    cp += 1;
                }
                b'/' => {
                    if new_pat.last() == Some(&b'/') {
                        cp += 1;
                        if cp == arg.len() {
                            slash_cnt -= 1;
                            new_pat.pop();
                        }
                    } else if at(cp + 1) == 0 {
                        cp += 1;
                    } else {
                        slash_cnt += 1;
                        new_pat.push(b'/');
                        cp += 1;
                    }
                }
                b'.' => {
                    if new_pat.last() == Some(&b'/') {
                        if at(cp + 1) == b'/' {
                            cp += 2;
                            if cp >= arg.len() {
                                slash_cnt -= 1;
                                new_pat.pop();
                            }
                        } else if at(cp + 1) == 0 {
                            cp += 1;
                            slash_cnt -= 1;
                            new_pat.pop();
                        } else {
                            new_pat.push(b'.');
                            cp += 1;
                        }
                    } else {
                        new_pat.push(b'.');
                        cp += 1;
                    }
                }
                b'[' => {
                    saw_live_open_brkt = true;
                    new_pat.push(b'[');
                    cp += 1;
                }
                other => {
                    new_pat.push(other);
                    cp += 1;
                }
            }
        }

        if !new_pat.is_empty() {
            let base_idx = list.len();
            let base = ModelRule {
                pattern: new_pat.clone(),
                wild: saw_wild,
                directory: false,
            };
            list.push(base.clone());
            if saw_live_open_brkt {
                model_literal_brackets(&base, list);
            }
            if opts.relative && slash_cnt > 0 {
                let mut parent = new_pat.clone();
                while let Some(pos) = parent.iter().rposition(|&b| b == b'/') {
                    if pos == 0 {
                        break;
                    }
                    parent.truncate(pos);
                    let found = list
                        .iter()
                        .enumerate()
                        .any(|(i, r)| i != base_idx && r.pattern == parent);
                    if found {
                        break;
                    }
                    let rule = ModelRule {
                        pattern: parent.clone(),
                        wild: saw_wild && parent.iter().any(|b| b"*[?\\".contains(b)),
                        directory: true,
                    };
                    list.push(rule.clone());
                    if saw_live_open_brkt {
                        model_literal_brackets(&rule, list);
                    }
                }
            }
        }
    }

    if opts.recurse || opts.dirs {
        let mut pattern = Vec::new();
        if !saw_wild && backslash_cnt > 0 {
            for &b in &new_pat {
                if b == b'\\' {
                    pattern.push(b'\\');
                }
                pattern.push(b);
            }
        } else {
            pattern.extend_from_slice(&new_pat);
        }
        pattern.extend_from_slice(if opts.recurse { b"/**" } else { b"/*" });
        let rule = ModelRule {
            pattern,
            wild: true,
            directory: false,
        };
        list.push(rule.clone());
        if saw_live_open_brkt {
            model_literal_brackets(&rule, list);
        }
    }
}

/// upstream: `exclude.c:1002-1069` `rule_matches()` for an anchored include
/// against a received (relative, non-empty) file-list name.
fn model_covers(list: &[ModelRule], name: &[u8], is_dir: bool) -> bool {
    !name.is_empty()
        && list.iter().any(|rule| {
            let body = &rule.pattern[1..];
            (is_dir || !rule.directory)
                && if rule.wild {
                    wildmatch(body, name)
                } else {
                    body == name
                }
        })
}

fn oc_covers(implied: &ImpliedIncludes, name: &[u8], is_dir: bool) -> bool {
    implied.covers(Path::new(OsStr::from_bytes(name)), is_dir)
}

fn all_option_sets() -> impl Iterator<Item = ImpliedIncludeOptions> {
    (0u8..16).map(|bits| ImpliedIncludeOptions {
        relative: bits & 1 != 0,
        recurse: bits & 2 != 0,
        dirs: bits & 4 != 0,
        skip_daemon_module: bits & 8 != 0,
    })
}

/// Spellings a sender could offer for an arg: every path prefix, a child
/// below it, and each with the backslash, bracket and wildcard bytes the
/// escaping rules treat differently rewritten one way or the other.
fn candidates(args: &[&[u8]]) -> BTreeSet<Vec<u8>> {
    let mut out = BTreeSet::new();
    for arg in args {
        let segments: Vec<&[u8]> = arg.split(|&b| b == b'/').collect();
        let mut stems = Vec::new();
        for start in 0..segments.len() {
            for end in start + 1..=segments.len() {
                stems.push(segments[start..end].join(&b'/'));
            }
        }
        for stem in stems {
            for spelling in rewrites(&stem) {
                for suffix in [&b""[..], b"/f", b"/d/f", b"\\"] {
                    let name = [spelling.as_slice(), suffix].concat();
                    let name = name.strip_prefix(b"/").unwrap_or(&name).to_vec();
                    if is_clean_received_name(&name) {
                        out.insert(name);
                    }
                }
            }
        }
    }
    for bait in [&b"evil"[..], b"f", b"d", b"d/f", b"\\", b"]", b"[x]", b"x"] {
        out.insert(bait.to_vec());
    }
    out
}

/// A received file-list name reaches the implied check (flist.c:1368) only
/// after `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)` (flist.c:1081), so it
/// never holds an empty, `.` or `..` segment.
fn is_clean_received_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .split(|&b| b == b'/')
            .all(|seg| !seg.is_empty() && seg != b"." && seg != b"..")
}

fn rewrites(stem: &[u8]) -> Vec<Vec<u8>> {
    let replace = |from: &[u8], to: &[u8]| -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < stem.len() {
            if stem[i..].starts_with(from) {
                out.extend_from_slice(to);
                i += from.len();
            } else {
                out.push(stem[i]);
                i += 1;
            }
        }
        out
    };
    vec![
        stem.to_vec(),
        replace(b"\\", b""),
        replace(b"\\", b"\\\\"),
        replace(b"\\]", b"]"),
        replace(b"*", b"z"),
        replace(b"?", b"z"),
        replace(b"[", b""),
        replace(b"]", b""),
        replace(b"/./", b"/"),
        replace(b"//", b"/"),
    ]
}

/// What one rule matches, independent of how it is spelled.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Meaning {
    /// Matches exactly this name (strcmp, or a wild pattern with no live
    /// metacharacter).
    Literal(Vec<u8>),
    /// Matches by `wildmatch` against this pattern.
    Wild(Vec<u8>),
}

/// Reduces a wildmatch pattern with no live `*`, `?` or `[` to the literal
/// name it matches (`\x` matches `x`); otherwise keeps it as a pattern.
fn meaning(body: &[u8], wild: bool) -> Meaning {
    if !wild {
        return Meaning::Literal(body.to_vec());
    }
    let mut literal = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'\\' if i + 1 < body.len() => {
                literal.push(body[i + 1]);
                i += 2;
            }
            b'*' | b'?' | b'[' => return Meaning::Wild(body.to_vec()),
            b => {
                literal.push(b);
                i += 1;
            }
        }
    }
    Meaning::Literal(literal)
}

/// The rule set as (meaning, directory-only) pairs. A directory-only rule is
/// dropped when a plain rule with the same meaning exists, since the plain one
/// already accepts the directory; upstream's parent walk stops at such a rule
/// (exclude.c:594-608) where oc adds the parent anyway.
fn rule_set(rules: impl Iterator<Item = (Meaning, bool)>) -> BTreeSet<(Meaning, bool)> {
    let all: BTreeSet<(Meaning, bool)> = rules.collect();
    all.iter()
        .filter(|(m, dir)| !*dir || !all.contains(&(m.clone(), false)))
        .cloned()
        .collect()
}

fn model_rule_set(model: &[ModelRule]) -> BTreeSet<(Meaning, bool)> {
    rule_set(
        model
            .iter()
            .map(|r| (meaning(&r.pattern[1..], r.wild), r.directory)),
    )
}

/// oc's stored patterns: anchored, a trailing `/` marks directory-only, and
/// the compiled rule wildmatches exactly when the body holds `*`, `?` or `[`
/// (compiled/mod.rs, `has_glob_wildcard`).
fn oc_rule_set(implied: &ImpliedIncludes) -> BTreeSet<(Meaning, bool)> {
    rule_set(implied.seen.iter().map(|pattern| {
        let directory = pattern.len() > 1 && pattern.ends_with(b"/");
        let body = &pattern[1..pattern.len() - usize::from(directory)];
        let wild = body.iter().any(|b| b"*?[".contains(b));
        (meaning(body, wild), directory)
    }))
}

/// Asserts oc builds the same rule set as the upstream model and gives the
/// same verdict for every candidate name (plus `extra` probes), as a file and
/// as a directory, under every option set.
fn assert_parity(args: &[&[u8]]) {
    assert_parity_with_probes(args, &[]);
}

fn assert_parity_with_probes(args: &[&[u8]], extra: &[Vec<u8>]) {
    let mut names = candidates(args);
    names.extend(extra.iter().filter(|n| is_clean_received_name(n)).cloned());
    for opts in all_option_sets() {
        let mut model = Vec::new();
        for arg in args {
            model_add(&mut model, arg, opts);
        }
        let implied = ImpliedIncludes::from_args(opts, args.iter().copied())
            .expect("implied rules compile");
        assert_eq!(
            implied.is_empty(),
            model.is_empty(),
            "active/inactive differs for {args:?} under {opts:?}: model {model:?}"
        );
        assert_eq!(
            oc_rule_set(&implied),
            model_rule_set(&model),
            "rule set differs for {:?} under {opts:?}",
            args.iter().map(|a| String::from_utf8_lossy(a)).collect::<Vec<_>>(),
        );
        for name in &names {
            for is_dir in [false, true] {
                assert_eq!(
                    oc_covers(&implied, name, is_dir),
                    model_covers(&model, name, is_dir),
                    "verdict differs for name {:?} (dir={is_dir}) from args {:?} under {opts:?}; \
                     upstream rules {model:?}",
                    String::from_utf8_lossy(name),
                    args.iter().map(|a| String::from_utf8_lossy(a)).collect::<Vec<_>>(),
                );
            }
        }
    }
}

/// The CVE-2026-70461 shape and its neighbours: a trailing backslash, a
/// backslash before each wildcard byte, and `\]`, each with and without a
/// wildcard elsewhere in the arg (which switches upstream's escaping mode).
#[test]
fn backslash_escaping_matches_upstream() {
    for arg in [
        &b"a\\"[..],
        b"a\\b\\",
        b"\\",
        b"\\\\",
        b"a\\b",
        b"*a\\",
        b"a*\\b\\",
        b"a\\*b",
        b"a\\[b",
        b"a\\?b",
        b"a\\*b\\",
        b"a\\]b",
        b"a\\]b*",
        b"a\\]",
        b"a\\\\*",
        b"a\\/c",
        b"a\\/c*",
        b"x\\]/y*",
        b"d/a\\b\\",
    ] {
        assert_parity(&[arg]);
    }
}

/// Upstream's byte loop drops `//`, `/./`, a trailing `/` or `/.`, and a bare
/// `.`; oc drops empty and `.` segments. Both must reach the same rule.
#[test]
fn path_normalisation_matches_upstream() {
    for arg in [
        &b"a//b"[..],
        b"a/./b",
        b"a/.",
        b"a/",
        b"a//",
        b"a/./",
        b"a/.//",
        b".",
        b"./",
        b".//",
        b".//a",
        b"/a",
        b"//a",
        b"a/..",
        b"../a",
        b"a/./b/./c",
        b"a/.b",
        b"a/b/",
    ] {
        assert_parity(&[arg]);
    }
}

/// `--relative` implies every parent directory; upstream stops at the first
/// parent already in the list and flags a parent wild only for a wild arg
/// whose sub-path holds `*[?\`.
#[test]
fn relative_parent_rules_match_upstream() {
    for args in [
        &[&b"a/b/c"[..]][..],
        &[b"a/b/c*"],
        &[b"a\\b/c*"],
        &[b"a\\]b/c*"],
        &[b"a*/b/c"],
        &[b"a\\*/b/c?"],
        &[b"a/b", b"a/b/c"],
        &[b"a", b"a/b/c"],
        &[b"a/*", b"a/b"],
        &[b"a/b/c", b"a/b"],
        &[b"x/./a/b"],
    ] {
        assert_parity(args);
    }
}

/// A live `[` also implies the literal bracketed name; an escaped `\[` does
/// not.
#[test]
fn literal_bracket_companion_matches_upstream() {
    for args in [
        &[&b"[x]"[..]][..],
        &[b"\\[x]"],
        &[b"\\\\[x]"],
        &[b"a[bc]d"],
        &[b"[x]/[y]"],
        &[b"[x]/y"],
        &[b"a/[b"],
    ] {
        assert_parity(args);
    }
}

/// The daemon module name is stripped up to the first `/` before any rule is
/// built.
#[test]
fn daemon_module_strip_matches_upstream() {
    for args in [
        &[&b"mod/a"[..]][..],
        &[b"mod"],
        &[b"mod/"],
        &[b"mod/./a/b"],
        &[b"mod/a\\"],
        &[b"mod/a", b"mod/b"],
    ] {
        assert_parity(args);
    }
}

/// The model reproduces the rules fixed 3.5.2 builds for the testsuite entry:
/// the trailing backslash is counted (`!cp[1]`, exclude.c:527), so the `/**`
/// rule doubles both backslashes and the name rule stays literal.
#[test]
fn model_builds_the_fixed_upstream_rules_for_a_trailing_backslash() {
    let opts = ImpliedIncludeOptions {
        recurse: true,
        ..Default::default()
    };
    let mut model = Vec::new();
    model_add(&mut model, b"a\\b\\", opts);
    let rules: Vec<(&[u8], bool, bool)> = model
        .iter()
        .map(|r| (r.pattern.as_slice(), r.wild, r.directory))
        .collect();
    assert_eq!(
        rules,
        [
            (&b"/a\\b\\"[..], false, false),
            (&b"/a\\\\b\\\\/**"[..], true, false),
        ]
    );
}

/// The rsync 3.5.2 testsuite fixture (`exclude-implied-trailing-backslash`):
/// a `-r --files-from` entry `a\b\`. The entry itself and its subtree are
/// requested; spellings that differ only in a backslash are not.
#[test]
fn testsuite_trailing_backslash_entry_is_requested_exactly() {
    let opts = ImpliedIncludeOptions {
        recurse: true,
        ..Default::default()
    };
    let implied = ImpliedIncludes::from_args(opts, [&b"a\\b\\"[..]]).unwrap();
    assert!(oc_covers(&implied, b"a\\b\\", true));
    assert!(oc_covers(&implied, b"a\\b\\/f", false));
    for bait in [&b"a\\b"[..], b"ab", b"a\\b\\\\", b"a\\\\b\\", b"a\\b/f", b"ab/f"] {
        assert!(
            !oc_covers(&implied, bait, true) && !oc_covers(&implied, bait, false),
            "{:?} must be refused",
            String::from_utf8_lossy(bait)
        );
    }
}

mod generated {
    use super::assert_parity_with_probes;
    use proptest::prelude::*;

    /// Bytes upstream's loop treats specially, plus two plain name bytes.
    fn bytes(max: usize) -> impl Strategy<Value = Vec<u8>> {
        proptest::collection::vec(prop::sample::select(&b"ab\\*[]?/."[..]), 1..max)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Generated source args, one to three at a time: oc builds the
        /// upstream model's rule set and gives its verdict for every derived
        /// candidate and every generated probe name, under every option set.
        #[test]
        fn generated_args_match_upstream(
            args in proptest::collection::vec(bytes(9), 1..4),
            probes in proptest::collection::vec(bytes(9), 0..16),
        ) {
            let refs: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
            assert_parity_with_probes(&refs, &probes);
        }
    }
}
