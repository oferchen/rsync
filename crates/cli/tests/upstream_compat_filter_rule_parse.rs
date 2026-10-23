//! Oracle cells: oc-rsync and upstream rsync 3.5.1 must parse filter rules alike.
//!
//! Every cell runs the SAME local transfer through both binaries over identical
//! fresh trees and compares what an operator can observe: the exit code, the
//! diagnostic text, and the files that reached the destination. The rule
//! parser is `parse_rule_tok` / `parse_filter_str` in upstream `exclude.c`, so a
//! divergence here is a divergence in which rules a run accepts or what they
//! select.
//!
//! # Gating
//!
//! Self-skips unless `OC_RSYNC_UPSTREAM_COMPAT=1` and an upstream 3.5.1 binary
//! resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_1`, or the interop install). Once both
//! are present every cell must run and match. Unix only: the pull cells run
//! the server through a `/bin/sh` remote shell.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use test_support::{
    UpstreamVersion, oc_rsync_bin, require_upstream_rsync, upstream_compat_enabled,
};

/// What one run of a binary over the fixture produced.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    exit: i32,
    /// The first diagnostic, stripped of its framing. Upstream prints the text
    /// on one line and a separate `rsync error: ... (code N) at FILE(LINE)`
    /// trailer; oc folds both into `oc-rsync error: TEXT (code N) at ...`. The
    /// source location and version tag legitimately differ, and the exit code
    /// is compared on its own, so only TEXT is kept.
    diagnostic: String,
    /// Sorted relative paths of every file and directory in the destination.
    transferred: Vec<String>,
}

/// Lays out `src/` from `files` (path, contents) plus an empty `dst/`.
fn build_tree(root: &Path, files: &[(&str, &str)]) {
    fs::create_dir_all(root.join("src")).expect("source root");
    fs::create_dir_all(root.join("dst")).expect("destination root");
    for (path, contents) in files {
        let target = root.join("src").join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("source subdir");
        }
        fs::write(target, contents).expect("source file");
    }
}

fn listing(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("read destination") {
        let entry = entry.expect("dir entry");
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type().expect("file type").is_dir() {
            listing(&entry.path(), &format!("{name}/"), out);
        }
        out.push(name);
    }
}

/// Reduces the first stderr line to the diagnostic text both binaries share.
fn diagnostic(stderr: &str) -> String {
    let line = stderr.lines().next().unwrap_or_default();
    let line = line.strip_prefix("oc-rsync error: ").unwrap_or(line);
    match line.rfind(" (code ") {
        Some(end) if line.contains(") at ") => line[..end].to_owned(),
        _ => line.to_owned(),
    }
}

/// How the client reaches the source tree.
#[derive(Clone, Copy)]
enum Source {
    /// A local `src/`.
    Local,
    /// `localhost:<tree>/src/` through a remote shell that runs upstream 3.5.1
    /// in place as the server, so the client's rules cross the wire to it.
    Pull,
    /// `src/` to `localhost:<tree>/dst/` through the same remote shell, so the
    /// client is the sender and its own per-directory merge parser runs.
    Push,
}

/// Writes a remote shell that drops the host argument and runs the rest here.
fn local_rsh(dir: &Path) -> PathBuf {
    let path = dir.join("lrsh");
    fs::write(&path, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    path
}

/// Runs `BINARY -r ARGS SOURCE DEST` from inside a fresh tree; `server` is the
/// remote binary a [`Source::Pull`] or [`Source::Push`] starts.
fn run(
    binary: &Path,
    server: &Path,
    files: &[(&str, &str)],
    args: &[&str],
    source: Source,
) -> Outcome {
    let tmp = test_support::create_tempdir();
    build_tree(tmp.path(), files);
    let mut command = Command::new(binary);
    command.current_dir(tmp.path()).arg("-r").args(args);
    if !matches!(source, Source::Local) {
        command
            .arg("-e")
            .arg(local_rsh(tmp.path()))
            .arg(format!("--rsync-path={}", server.display()));
    }
    let remote = |dir: &str| format!("localhost:{}/{dir}/", tmp.path().display());
    let (from, to) = match source {
        Source::Local => ("src/".to_owned(), "dst/".to_owned()),
        Source::Pull => (remote("src"), "dst/".to_owned()),
        Source::Push => ("src/".to_owned(), remote("dst")),
    };
    let output = command.arg(from).arg(to).output().expect("run rsync");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut transferred = Vec::new();
    listing(&tmp.path().join("dst"), "", &mut transferred);
    transferred.sort();
    Outcome {
        exit: output.status.code().expect("exit code"),
        diagnostic: diagnostic(&stderr),
        transferred,
    }
}

/// The resolved upstream 3.5.1 binary, or `None` when the cell must skip.
fn upstream() -> Option<test_support::UpstreamRsync> {
    if !upstream_compat_enabled() {
        return None;
    }
    require_upstream_rsync(UpstreamVersion::V3_5_1)
}

/// Asserts an oc client and an upstream client agree on `args` over `files`,
/// returning the shared outcome so a cell can also pin WHAT they agree on.
fn assert_parity_from(files: &[(&str, &str)], args: &[&str], source: Source) -> Option<Outcome> {
    let upstream = upstream()?;
    let server = upstream.binary();
    let expected = run(server, server, files, args, source);
    let actual = run(&oc_rsync_bin(), server, files, args, source);
    assert_eq!(actual, expected, "oc vs upstream 3.5.1 for {args:?}");
    Some(actual)
}

/// [`assert_parity_from`] for a local copy.
fn assert_parity(files: &[(&str, &str)], args: &[&str]) -> Option<Outcome> {
    assert_parity_from(files, args, Source::Local)
}

/// #457: upstream 3.5.1 has no `exclude-if-present` rule. `parse_rule_tok`
/// takes `case 'e':`, `RULE_STRCMP(s, "exclude")` rejects the `-` after the
/// keyword, and `filter_rule_err("Unknown filter rule")` exits RERR_SYNTAX
/// (exclude.c:1297-1299, 1362-1363).
#[test]
fn exclude_if_present_is_an_unknown_rule() {
    let files = [("a", "a"), ("sub/.git", "marker"), ("sub/b", "b")];
    for rule in ["exclude-if-present .git", "exclude-if-present=.git"] {
        if let Some(outcome) = assert_parity(&files, &[&format!("--filter={rule}")]) {
            assert_eq!(outcome.exit, 1, "{rule}");
            assert_eq!(outcome.diagnostic, format!("Unknown filter rule: {rule}"));
        }
    }
}

/// #212: a `!` empties the rule list while it is being parsed and never reaches
/// the wire. `parse_filter_str` pops the list and `continue`s on
/// FILTRULE_CLEAR_LIST (exclude.c:1541-1551), so there is nothing to send and
/// nothing for a pre-29 peer, which cannot encode a clear, to refuse.
#[test]
fn clear_rule_is_dropped_at_parse_time_on_a_protocol_28_pull() {
    let files = [("a.tmp", "a"), ("keep", "k")];
    for clear in ["--exclude=!", "--filter=!"] {
        let args = ["--protocol=28", "--exclude=*.tmp", clear, "--exclude=keep"];
        if let Some(outcome) = assert_parity_from(&files, &args, Source::Pull) {
            assert_eq!(outcome.exit, 0, "{clear}");
            assert_eq!(outcome.transferred, ["a.tmp"], "{clear}");
        }
    }
}

/// #225: once a merge rule's `-`/`+`/`C` set FILTRULE_NO_PREFIXES, a second one
/// reaches `invalid` (exclude.c:1381-1391, 1402-1404).
#[test]
fn merge_rule_refuses_a_second_prefix_modifier() {
    let files = [("a", "a")];
    for rule in [":-C", ":C-", ":--", ":++", ":+-", ".-C", "dir-merge,-C .f"] {
        if let Some(outcome) = assert_parity(&files, &[&format!("--filter={rule}")]) {
            assert_eq!(outcome.exit, 1, "{rule}");
            assert!(outcome.diagnostic.starts_with("invalid modifier"), "{rule}");
        }
    }
}

/// #223: upstream accepts `C` on a non-merge rule (exclude.c:1402-1409), lets
/// it have no pattern (:1474), never matches its own pattern (:1201-1206) and
/// builds the CVS list from its flags (:1595-1597). A side-binding prefix or a
/// second `C` refuses it.
#[test]
fn c_modifier_on_a_non_merge_rule() {
    let files = [("a.o", "o"), ("b.c", "c"), ("foo", "f")];
    for args in [
        &["--filter=-C"][..],
        &["--filter=+C", "--filter=- *.o"],
        &["--filter=-C", "--filter=+ *.o"],
        &["--filter=-sC"],
        &["--filter=-rC"],
        &["--filter=-pC"],
        &["--filter=-Cs"],
        &["--filter=-!C"],
        &["--filter=-xC"],
        &["--filter=-C foo"],
        &["--filter=exclude,C"],
        &["--filter=PC"],
        &["--filter=-CC"],
    ] {
        assert_parity(&files, args);
    }
}

/// #223: the same `C` rules inside a per-directory merge file, through the
/// local-copy parser and, on a push, the sender's per-directory parser.
///
/// A refused rule is left to the `--filter` cell above: upstream names the
/// file by its absolute path in `<rule from ...>`, oc by the path it opened,
/// which is a separate divergence from the one this cell pins.
#[test]
fn c_modifier_inside_a_per_directory_merge_file() {
    for rules in ["-C\n", "+C\n- *.o\n", "-sC foo\n", "-rC\n"] {
        let files = [("a.o", "o"), ("b.c", "c"), (".rsync-filter", rules)];
        for source in [Source::Local, Source::Push] {
            assert_parity_from(&files, &["-F"], source);
        }
    }
}

/// #224: `e` adds an exclude of the merge file's basename, once and with none
/// of the rule's modifiers, AHEAD of the merged rules (exclude.c:1558-1572), so
/// a `+` naming the file inside it, or after a dir-merge, cannot re-include it.
#[test]
fn exclude_self_precedes_the_merged_rules() {
    let files = [
        ("a", "a"),
        ("b", "b"),
        ("rules", "+ rules\n- b\n"),
        (".f", "+ .f\n:e .g\n+ .g\n"),
        ("sub/.f", "- a\n"),
        ("sub/.g", "- b\n"),
        ("sub/a", "a"),
        ("sub/b", "b"),
    ];
    for args in [
        &["--filter=.e src/rules"][..],
        &["--filter=merge,e src/rules"],
        &["--filter=:e .f"],
        &["--filter=dir-merge,e .f", "--filter=+ .f"],
    ] {
        for source in [Source::Local, Source::Push, Source::Pull] {
            assert_parity_from(&files, args, source);
        }
    }
}

/// Asserts an oc client and an upstream client agree on `args` over `files`
/// for a rule refused out of a merge FILE, where the two may differ only in
/// how `<rule from ...>` names the file: upstream by absolute path, oc by the
/// path it opened. So each diagnostic is compared up to `<rule from `.
///
/// On a push the refusal comes from oc's sender walk, which, like every
/// per-directory parse error there, still exits 11 inside a `transfer failed`
/// wrapper where upstream exits 1 (task #376: RERR codes chosen from
/// `ErrorKind`). That leg pins the refusal and its text, not the exit code.
fn assert_file_rule_parity(files: &[(&str, &str)], args: &[&str], source: Source) {
    let Some(upstream) = upstream() else { return };
    let server = upstream.binary();
    let expected = run(server, server, files, args, source);
    let actual = run(&oc_rsync_bin(), server, files, args, source);
    let context = format!("{args:?} {files:?}: oc {actual:?} vs upstream {expected:?}");
    assert_eq!(actual.transferred, expected.transferred, "{context}");
    let text = expected
        .diagnostic
        .split("<rule from ")
        .next()
        .unwrap_or_default();
    if matches!(source, Source::Push) && expected.exit != 0 {
        assert_ne!(actual.exit, 0, "{context}");
        assert!(actual.diagnostic.contains(text), "{context}");
    } else {
        assert_eq!(actual.exit, expected.exit, "{context}");
        assert!(actual.diagnostic.starts_with(text), "{context}");
    }
}

/// #226: a rule that names a side inside a `:s`/`:r` file is a
/// `filter_rule_err` (exclude.c:1447-1456), exit 1 (12 for the pull's client,
/// which sees its server die), with the line redacted to `<rule from ...>`
/// (exclude.c:133-137). A pull with `:r` refuses nothing in either: the
/// receiver-side rule never reaches the sender that reads the file.
#[test]
fn sided_merge_file_refuses_a_sided_rule() {
    for (rule, line) in [(":s .f", "-r foo"), (":r .f", "S foo"), (":s .f", ":r .g")] {
        let files = [("a", "a"), (".f", &*format!("- ok\n{line}\n"))];
        for source in [Source::Local, Source::Push, Source::Pull] {
            assert_file_rule_parity(&files, &[&format!("--filter={rule}")], source);
        }
    }
}

/// #227: an empty pattern is "unexpected end of filter rule" unless the rule
/// has `C` (exclude.c:1474-1476), so a bare `exclude` or `merge` in a merge
/// file is refused, and a word-split file ends every rule at whitespace
/// (exclude.c:1772), so `exclude foo` there is a bare `exclude`. A `C` merge
/// rule with no name merges `.cvsignore` (exclude.c:1553-1557).
#[test]
fn merge_file_rules_end_where_upstream_ends_them() {
    for (filter, file, contents) in [
        (":w .f", ".f", "exclude foo\n"),
        (":w .f", ".f", "- foo\n"),
        (":w .f", ".f", "-_foo\n"),
        (": .f", ".f", "exclude\n"),
        (": .f", ".f", "merge\n"),
        (": .f", ".f", "dir-merge,C\n"),
        (". src/rules", "rules", "exclude\n"),
        (". src/rules", "rules", "dir-merge,C\n"),
        (".w src/rules", "rules", "exclude foo\n"),
    ] {
        let files = [
            ("a", "a"),
            ("foo", "f"),
            (".cvsignore", "a\n"),
            (file, contents),
        ];
        for source in [Source::Local, Source::Push] {
            assert_file_rule_parity(&files, &[&format!("--filter={filter}")], source);
        }
    }
}
