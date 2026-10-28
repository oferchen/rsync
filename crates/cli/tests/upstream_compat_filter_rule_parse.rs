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

/// Marks a fixture path that starts out in the destination.
const DEST: &str = "@dst/";

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

/// Lays out `src/` from `files` (path, contents) plus `dst/`; a path under
/// [`DEST`] is placed in `dst/` instead, as a file the run may delete.
fn build_tree(root: &Path, files: &[(&str, &str)]) {
    fs::create_dir_all(root.join("src")).expect("source root");
    fs::create_dir_all(root.join("dst")).expect("destination root");
    for (path, contents) in files {
        let target = match path.strip_prefix(DEST) {
            Some(rest) => root.join("dst").join(rest),
            None => root.join("src").join(path),
        };
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
    // A per-directory merge file is named by its absolute path
    // (exclude.c:756-775), which holds this run's own temporary tree.
    let tree = tmp.path().to_string_lossy();
    Outcome {
        exit: output.status.code().expect("exit code"),
        diagnostic: diagnostic(&stderr).replace(tree.as_ref(), "<tree>"),
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
/// local-copy parser and, on a push, the sender's per-directory parser. A
/// refused rule names the file by its absolute path (exclude.c:756-775).
#[test]
fn c_modifier_inside_a_per_directory_merge_file() {
    for rules in ["-C\n", "+C\n- *.o\n", "-sC foo\n", "-rC\n", "-CC\n"] {
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

/// #226: a rule that names a side inside a `:s`/`:r` file is a
/// `filter_rule_err` (exclude.c:1447-1456), exit 1 (12 for the pull's client,
/// which sees its server die), with the line redacted to `<rule from ...>`
/// (exclude.c:133-137) naming the file by its absolute path (exclude.c:756-775),
/// on a push too, where the oc sender's walk used to exit 11 inside a
/// `transfer failed` wrapper. A pull with `:r` refuses nothing in either: the
/// receiver-side rule never reaches the sender that reads the file.
#[test]
fn sided_merge_file_refuses_a_sided_rule() {
    for (rule, line) in [(":s .f", "-r foo"), (":r .f", "S foo"), (":s .f", ":r .g")] {
        let files = [("a", "a"), (".f", &*format!("- ok\n{line}\n"))];
        for source in [Source::Local, Source::Push, Source::Pull] {
            assert_parity_from(&files, &[&format!("--filter={rule}")], source);
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
            assert_parity_from(&files, &[&format!("--filter={filter}")], source);
        }
    }
}

/// get_cvs_excludes() builds the CVS list once, under the first non-merge `C`
/// rule's flags (exclude.c:1489-1496), and every `C` rule matches that one list
/// (exclude.c:1201-1206). MEASURED: after `-C`, a later `-!C` keeps `b.c`.
#[test]
fn every_c_rule_matches_the_list_built_for_the_first() {
    let files = [
        ("a.o", "o"),
        ("b.c", "c"),
        ("sub/.f", "-!C\n"),
        ("sub/c.c", "c"),
    ];
    for args in [
        &["--filter=-C", "--filter=-!C"][..],
        &["--filter=-!C", "--filter=-C"],
        &["--filter=+!C", "--filter=-C", "--filter=-_*"],
        &["--filter=-C", "--filter=:_.f"],
    ] {
        for source in [Source::Local, Source::Push] {
            assert_parity_from(&files, args, source);
        }
    }
}

/// `-C` only sets cvs_exclude; send_filter_list() appends `:C` and `-C` after
/// every other rule (exclude.c:1951-1954, 1962-1967). MEASURED: `-C
/// --filter=+_*.o` transfers `a.o`.
#[test]
fn cvs_exclude_rules_follow_every_other_rule() {
    let files = [
        ("a.o", "o"),
        ("b.c", "c"),
        ("sub/.cvsignore", "b.c\n"),
        ("sub/b.c", "c"),
    ];
    for args in [
        &["-C", "--filter=+_*.o"][..],
        &["--filter=+_*.o", "-C"],
        &["-C", "--filter=-!C"],
        &["-C", "--exclude=sub/"],
    ] {
        for source in [Source::Local, Source::Push, Source::Pull] {
            assert_parity_from(&files, args, source);
        }
    }
}

/// `-sr` names both sides, so it counts as sided (exclude.c:1423-1432, 1448):
/// a side-specific merge file refuses it, and a merge rule that names both
/// sides refuses a sided rule inside it. oc used to read "both sides" as "no
/// side".
#[test]
fn naming_both_sides_still_names_a_side() {
    for (filter, contents) in [
        (":s .f", "-sr foo\n"),
        (":sr .f", "-s foo\n"),
        (":sr .f", "- foo\n"),
        (": .f", "-sr foo\n"),
    ] {
        let files = [("a", "a"), ("foo", "f"), (".f", contents)];
        for source in [Source::Local, Source::Push] {
            assert_parity_from(&files, &[&format!("--filter={filter}")], source);
        }
    }
}

/// A merge file named inside another merge file opens from the current
/// directory, because parse_merge_name() joins it onto a `dirbuf` that is still
/// empty while the options are parsed (exclude.c:696-745). oc used to open it
/// from the naming file's own directory.
#[test]
fn a_nested_merge_file_opens_from_the_current_directory() {
    for nested in ["src/r/b", "b"] {
        let naming = format!(". {nested}\n");
        let files = [("x", "x"), ("r/a", naming.as_str()), ("r/b", "- x\n")];
        for source in [Source::Local, Source::Push] {
            assert_parity_from(&files, &["--filter=. src/r/a"], source);
        }
    }
}

/// The local copy matches through the same wildmatch port as the transfer
/// roles. upstream rejects no pattern (exclude.c add_rule) and lib/wildmatch.c
/// fails an unclosed `[`, so `-[` transfers a file named `[`. The local copy's
/// old globset matcher refused the pattern and failed the run.
#[test]
fn unclosed_bracket_pattern_is_kept_and_matches_nothing() {
    let files = [("[", "a"), ("b[c", "b"), ("d", "d")];
    for rule in ["--exclude=[", "--exclude=b[c", "--filter=- *["] {
        for source in [Source::Local, Source::Push] {
            if let Some(outcome) = assert_parity_from(&files, &[rule], source) {
                assert_eq!(outcome.exit, 0, "{rule}");
            }
        }
    }
}

/// --delete-excluded turns only an unsided include/exclude into a sender-side
/// rule (exclude.c:1479-1481), so `-r *.keep` still protects the destination's
/// `x.keep` while the unsided `*.tmp` exclude lets `j.tmp` go. oc's local copy
/// used to delete both, because it let --delete-excluded override every
/// exclude at match time.
#[test]
fn delete_excluded_spares_a_receiver_side_exclude() {
    let files = [("s/a", "a"), ("@dst/s/x.keep", "k"), ("@dst/s/j.tmp", "t")];
    for source in [Source::Local, Source::Push] {
        assert_parity_from(
            &files,
            &[
                "--delete",
                "--filter=-r *.keep",
                "--exclude=*.tmp",
                "--delete-excluded",
            ],
            source,
        );
    }
}
