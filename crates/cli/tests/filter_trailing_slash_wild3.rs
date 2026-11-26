//! A filter rule written with a trailing slash never matches a file, however
//! far its `/***` suffix reaches.
//!
//! upstream: `add_rule()` peels the trailing `/` into `FILTRULE_DIRECTORY`
//! (exclude.c:287-290) before it flags the `***` suffix (exclude.c:340-345),
//! and `rule_matches()` returns no match for a non-directory name on a
//! `FILTRULE_DIRECTORY` rule before any wildmatch (exclude.c:1037-1038). A file
//! below such a rule is lost only when the walk prunes its excluded directory.
//!
//! Every cell asserts the names rsync 3.5.2 printed for the same rules and
//! tree. When the 3.5.2 oracle resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_2`,
//! required under `OC_RSYNC_UPSTREAM_COMPAT=1`), each cell also runs 3.5.2 and
//! compares the two outputs.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// How the names reach the filter.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// `-r src/ empty`: the sender walks the tree and prunes excluded
    /// directories.
    Walk,
    /// `--files-from` naming the files: no walk through their parents.
    FilesFrom,
    /// `-R` with the files as operands.
    Relative,
    /// `-r --delete` into a destination holding extraneous entries.
    Delete,
    /// `-r --delete-excluded` into the same destination.
    DeleteExcluded,
}

/// The rules, how the names are reached, and the sorted output lines.
const CELLS: [(&[&str], Mode, &str); 26] = [
    (&["+ d/", "- **/***/"], Mode::Walk, "./ I d/ d/I d/f"),
    (&["- d/***/"], Mode::Walk, "./ I"),
    (&["- **/***/"], Mode::Walk, "./ I"),
    (
        &["+ d/", "- **/***/"],
        Mode::FilesFrom,
        "I d/ d/f d/sub/ d/sub/f",
    ),
    (&["- d/***/"], Mode::FilesFrom, "I d/ d/f d/sub/ d/sub/f"),
    (&["- **/***/"], Mode::FilesFrom, "I d/ d/f d/sub/ d/sub/f"),
    (
        &["+ d/", "- **/***/"],
        Mode::Relative,
        "I d/ d/f d/sub/ d/sub/f",
    ),
    (&["- d/***/"], Mode::Relative, "I d/ d/f d/sub/ d/sub/f"),
    (&["- **/***/"], Mode::Relative, "I d/ d/f d/sub/ d/sub/f"),
    (
        &["+ d/", "- **/***/"],
        Mode::Delete,
        "del.:X del.:d/X send:I send:d/I send:d/f",
    ),
    (
        &["- d/***/"],
        Mode::Delete,
        "del.:X del.:e/ del.:e/Y send:I",
    ),
    (&["- **/***/"], Mode::Delete, "del.:X send:I"),
    (
        &["+ d/", "- **/***/"],
        Mode::DeleteExcluded,
        "del.:X del.:d/X del.:d/sub/ del.:d/sub/X del.:e/ del.:e/Y send:I send:d/I send:d/f",
    ),
    (
        &["- d/***/"],
        Mode::DeleteExcluded,
        "del.:X del.:d/ del.:d/X del.:d/sub/ del.:d/sub/X del.:e/ del.:e/Y send:I",
    ),
    (
        &["- **/***/"],
        Mode::DeleteExcluded,
        "del.:X del.:d/ del.:d/X del.:d/sub/ del.:d/sub/X del.:e/ del.:e/Y send:I",
    ),
    (&["P **/***/"], Mode::Walk, "./ I d/ d/I d/f d/sub/ d/sub/f"),
    (
        &["P **/***/"],
        Mode::Delete,
        "del.:X del.:d/X del.:d/sub/X send:I send:d/I send:d/f send:d/sub/f",
    ),
    (&["R **/***/"], Mode::Walk, "./ I d/ d/I d/f d/sub/ d/sub/f"),
    (
        &["R **/***/"],
        Mode::Delete,
        "del.:X del.:d/X del.:d/sub/X del.:e/ del.:e/Y send:I send:d/I send:d/f send:d/sub/f",
    ),
    (&["H **/***/"], Mode::Walk, "./ I"),
    (
        &["H **/***/"],
        Mode::Delete,
        "del.:X del.:d/ del.:d/X del.:d/sub/ del.:d/sub/X del.:e/ del.:e/Y send:I",
    ),
    (&["S **/***/"], Mode::Walk, "./ I d/ d/I d/f d/sub/ d/sub/f"),
    (
        &["S **/***/"],
        Mode::Delete,
        "del.:X del.:d/X del.:d/sub/X del.:e/ del.:e/Y send:I send:d/I send:d/f send:d/sub/f",
    ),
    (
        &["S **/***/", "H *"],
        Mode::Delete,
        "del.:X del.:d/X del.:d/sub/X del.:e/ del.:e/Y",
    ),
    (&["S **/***/", "H *"], Mode::Walk, "./ d/ d/sub/"),
    (
        &["R **/***/", "P *"],
        Mode::Delete,
        "cannot delete non-empty directory: e send:I send:d/I send:d/f send:d/sub/f",
    ),
];

/// Runs `binary` in dry-run mode over a fresh copy of the fixture and returns
/// its stdout and stderr lines, sorted and joined with spaces.
///
/// The `created directory` notice is dropped: it names the temporary
/// destination, and only one of the two implementations prints it in a dry
/// run.
fn run(binary: &Path, rules: &[&str], mode: Mode) -> String {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    fs::create_dir_all(src.join("d/sub")).expect("mkdir src");
    fs::create_dir_all(dst.join("d/sub")).expect("mkdir dst");
    fs::create_dir_all(dst.join("e")).expect("mkdir dst/e");
    for name in ["I", "d/I", "d/f", "d/sub/f"] {
        fs::write(src.join(name), b"x\n").expect("write source file");
    }
    for name in ["X", "d/X", "d/sub/X", "e/Y"] {
        fs::write(dst.join(name), b"y\n").expect("write extraneous file");
    }
    let list = dir.path().join("list");
    fs::write(&list, "d/f\nI\nd/sub/f\n").expect("write list");
    let empty = dir.path().join("empty");

    let mut command = Command::new(binary);
    command.arg("-n").current_dir(&src);
    for rule in rules {
        command.arg(format!("--filter={rule}"));
    }
    let source_dir = format!("{}/", src.display());
    match mode {
        Mode::Walk => command
            .args(["-r", "--out-format=%n", &source_dir])
            .arg(&empty),
        Mode::FilesFrom => command
            .arg("--out-format=%n")
            .arg(format!("--files-from={}", list.display()))
            .arg(&source_dir)
            .arg(&empty),
        Mode::Relative => command
            .args(["-R", "--out-format=%n", "d/f", "I", "d/sub/f"])
            .arg(&empty),
        Mode::Delete => command
            .args(["-r", "--delete", "--out-format=%o:%n", &source_dir])
            .arg(format!("{}/", dst.display())),
        Mode::DeleteExcluded => command
            .args(["-r", "--delete-excluded", "--out-format=%o:%n", &source_dir])
            .arg(format!("{}/", dst.display())),
    };
    let output = command.output().expect("run rsync");
    let mut lines: Vec<String> = [output.stdout, output.stderr]
        .iter()
        .flat_map(|stream| {
            String::from_utf8_lossy(stream)
                .into_owned()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|line| !line.is_empty() && !line.starts_with("created directory"))
        .collect();
    lines.sort();
    lines.join(" ")
}

#[test]
fn trailing_slash_wild3_rules_match_rsync() {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for (rules, mode, expected) in CELLS {
        let cell = format!("{mode:?} {rules:?}");
        let oc = run(&oc_rsync_bin(), rules, mode);
        assert_eq!(oc, expected, "{cell}");
        if let Some(upstream) = upstream.as_ref() {
            let reference = run(upstream.binary(), rules, mode);
            assert_eq!(oc, reference, "{cell}: oc differs from rsync 3.5.2");
        }
    }
}
