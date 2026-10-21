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
//! are present every cell must run and match.

use std::fs;
use std::path::Path;
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

/// Runs `BINARY -r ARGS src/ dst/` from inside a fresh tree.
fn run(binary: &Path, files: &[(&str, &str)], args: &[&str]) -> Outcome {
    let tmp = test_support::create_tempdir();
    build_tree(tmp.path(), files);
    let output = Command::new(binary)
        .current_dir(tmp.path())
        .arg("-r")
        .args(args)
        .arg("src/")
        .arg("dst/")
        .output()
        .expect("run rsync");
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

/// Asserts oc and upstream agree on `args` over `files`, returning the shared
/// outcome so a cell can also pin WHAT they agree on.
fn assert_parity(files: &[(&str, &str)], args: &[&str]) -> Option<Outcome> {
    if !upstream_compat_enabled() {
        return None;
    }
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_1)?;
    let expected = run(upstream.binary(), files, args);
    let actual = run(&oc_rsync_bin(), files, args);
    assert_eq!(actual, expected, "oc vs upstream 3.5.1 for {args:?}");
    Some(actual)
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
