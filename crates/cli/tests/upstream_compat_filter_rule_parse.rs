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
}

/// Writes a remote shell that drops the host argument and runs the rest here.
fn local_rsh(dir: &Path) -> PathBuf {
    let path = dir.join("lrsh");
    fs::write(&path, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    path
}

/// Runs `BINARY -r ARGS SOURCE dst/` from inside a fresh tree; `server` is the
/// remote binary a [`Source::Pull`] starts.
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
    match source {
        Source::Local => {
            command.arg("src/");
        }
        Source::Pull => {
            command
                .arg("-e")
                .arg(local_rsh(tmp.path()))
                .arg(format!("--rsync-path={}", server.display()))
                .arg(format!("localhost:{}/src/", tmp.path().display()));
        }
    }
    let output = command.arg("dst/").output().expect("run rsync");
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
