//! oc's transfer-pipeline internals print only under the oc-only PIPELINE
//! debug word.
//!
//! Upstream has no phase timings, call totals or segment lines, so no `-v`
//! count may print them: its `-v` rows (options.c:244-252
//! `debug_verbosity[]`) name only upstream's own words. `--debug=pipeline`
//! still shows them, level 1 the summaries and level 2 the detail.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;
use test_support::oc_rsync_bin;

/// One line from each family of pipeline internals, by level.
const LEVEL_ONE: [&str; 4] = [
    "file-list-sort: ",
    "generator-transfer-loop: ",
    "totals: calls=",
    "partitioned file list: ",
];
const LEVEL_TWO: [&str; 3] = [
    "reclaiming segment ",
    "inc_recurse producer: ",
    "remote-shell wire: ",
];

/// Copies a small nested tree, which recurses incrementally over several
/// sub-lists, and returns stdout and stderr together.
fn copy_with(args: &[&str]) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("d1/d2")).unwrap();
    fs::write(src.join("f1"), "a").unwrap();
    fs::write(src.join("d1/f2"), "bb").unwrap();
    fs::write(src.join("d1/d2/f3"), "ccc").unwrap();
    symlink("f1", src.join("l1")).unwrap();
    let output = Command::new(oc_rsync_bin())
        .arg("-a")
        .args(args)
        .arg(format!("{}/", src.display()))
        .arg(tmp.path().join("dst"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

#[test]
fn no_verbosity_prints_a_pipeline_line() {
    let out = copy_with(&["-vvvvv"]);
    for line in LEVEL_ONE.iter().chain(&LEVEL_TWO) {
        assert!(!out.contains(line), "-vvvvv printed {line:?}:\n{out}");
    }
}

#[test]
fn the_pipeline_word_prints_its_lines_by_level() {
    let one = copy_with(&["--debug=pipeline"]);
    for line in LEVEL_ONE {
        assert!(
            one.contains(line),
            "--debug=pipeline lacks {line:?}:\n{one}"
        );
    }
    for line in LEVEL_TWO {
        assert!(
            !one.contains(line),
            "--debug=pipeline printed level-2 {line:?}:\n{one}"
        );
    }
    let two = copy_with(&["--debug=pipeline2"]);
    for line in LEVEL_ONE.iter().chain(&LEVEL_TWO) {
        assert!(
            two.contains(line),
            "--debug=pipeline2 lacks {line:?}:\n{two}"
        );
    }
}
