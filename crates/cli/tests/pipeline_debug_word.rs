//! oc's transfer-pipeline internals print only under the oc-only PIPELINE
//! debug word.
//!
//! Upstream has no phase timings, call totals or segment lines, so no `-v`
//! count may print them: its `-v` rows (options.c:244-252
//! `debug_verbosity[]`) name only upstream's own words. `--debug=pipeline`
//! still shows them, level 1 the summaries and level 2 the detail.
#![cfg(unix)]
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;
use test_support::{
    UpstreamVersion, oc_rsync_bin, require_upstream_rsync, upstream_compat_enabled,
};

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
    copy_tree(&oc_rsync_bin(), args).0
}

/// Runs `binary -a ARGS` over a fresh tree whose `e/` subtree is already up
/// to date in the destination. Returns the output and the temp root.
fn copy_tree(binary: &Path, args: &[&str]) -> (String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("d1/d2")).unwrap();
    fs::create_dir_all(src.join("e")).unwrap();
    fs::write(src.join("f1"), "a").unwrap();
    fs::write(src.join("d1/f2"), "bb").unwrap();
    fs::write(src.join("d1/d2/f3"), "ccc").unwrap();
    fs::write(src.join("e/f4"), "dddd").unwrap();
    symlink("f1", src.join("l1")).unwrap();
    let dst = tmp.path().join("dst");
    let seeded = Command::new(oc_rsync_bin())
        .arg("-a")
        .arg(src.join("e"))
        .arg(&dst)
        .status()
        .unwrap();
    assert!(seeded.success());
    // Pin `src`'s mtime to a fixed past epoch so `src/` and `dst/` never
    // depend on whether `symlink("f1", ...)` and `seeded` happened to land in
    // the same whole wall-clock second (`./ is uptodate` vs `./`).
    let fixed = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    fs::File::open(&src)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(fixed))
        .unwrap();
    let output = Command::new(binary)
        .arg("-a")
        .args(args)
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (text, tmp.path().display().to_string())
}

/// The lines of `text` with the temp root replaced and every number masked,
/// so runs compare by line shape. A digit run swallows the `,` and `.` inside
/// it: 3.5.2 groups the rate's thousands and oc does not yet (filed apart).
fn line_shapes(text: &str, root: &str) -> BTreeSet<String> {
    text.replace(root, "ROOT")
        .lines()
        .map(|line| {
            let line = mask_temp_suffix(line);
            let mut shape = String::with_capacity(line.len());
            let mut in_number = false;
            for c in line.chars() {
                let numeric = c.is_ascii_digit() || (in_number && (c == ',' || c == '.'));
                if numeric {
                    if !in_number {
                        shape.push('N');
                    }
                } else {
                    shape.push(c);
                }
                in_number = numeric;
            }
            shape
        })
        .collect()
}

/// Masks the random six-character suffix of the temp name in a receiver
/// `renaming .<name>.XXXXXX to <name>` line (rsync.c:917), which differs on
/// every run of either implementation.
fn mask_temp_suffix(line: &str) -> String {
    match line
        .strip_prefix("renaming ")
        .and_then(|rest| rest.split_once(" to "))
    {
        Some((temp, dest)) if temp.len() > 6 => {
            format!("renaming {}XXXXXX to {dest}", &temp[..temp.len() - 6])
        }
        _ => line.to_owned(),
    }
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

/// Every line a local `-vvv` copy prints, 3.5.2 prints too. The oc-only
/// SECCOMP and LANDLOCK words are in no `-v` row, so a sandbox line here is
/// a regression.
#[test]
fn local_vvv_prints_no_oc_only_line() {
    if !upstream_compat_enabled() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    for verbosity in ["-vvv", "-vvvv"] {
        let (oc, oc_root) = copy_tree(&oc_rsync_bin(), &[verbosity]);
        let (up, up_root) = copy_tree(upstream.binary(), &[verbosity]);
        let upstream_shapes = line_shapes(&up, &up_root);
        let oc_only: Vec<String> = line_shapes(&oc, &oc_root)
            .into_iter()
            .filter(|line| !upstream_shapes.contains(line))
            .collect();
        assert!(
            oc_only.is_empty(),
            "{verbosity} lines 3.5.2 never prints: {oc_only:#?}"
        );
    }
}
