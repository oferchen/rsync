//! A pull's `--stats` "Number of files" line counts every received entry,
//! sub-lists included, so incremental recursion reports the same total as a
//! whole-list pull.
//!
//! upstream: flist.c:3335 - `stats.num_files += flist->used` at the end of
//! every `recv_file_list()` call, and the receiver calls it once per INC_RECURSE
//! sub-list. A receiver that counts only the initial list under-reports a
//! multi-directory pull while the whole-list run stays correct.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};
const PULL_INC_RECURSE_ENV: &str = "OC_RSYNC_PULL_INC_RECURSE";
/// `.` plus four directories, four regular files and one symlink, spread over
/// three directory levels so incremental recursion sends several sub-lists.
const EXPECTED_FILES_LINE: &str = "Number of files: 10 (reg: 4, dir: 5, link: 1)";
fn fixture(src: &Path) {
    for file in ["a/one", "a/b/two", "a/b/c/three", "d/four"] {
        let path = src.join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir src");
        fs::write(&path, file.as_bytes()).expect("write src file");
    }
    symlink("a/one", src.join("d/link")).expect("symlink");
}
fn files_line(root: &Path, inc_recurse: Option<&str>) -> String {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let dest = root.join(format!("dest-{}", inc_recurse.unwrap_or("default")));
    let mut runner = OcRsyncCliRunner::new();
    if let Some(value) = inc_recurse {
        runner = runner.env(PULL_INC_RECURSE_ENV, value);
    }
    let out = runner
        .arg("-a")
        .arg("--stats")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("localhost:{}/", root.join("src").display()))
        .arg(format!("{}/", dest.display()))
        .run()
        .expect("pull run");
    out.assert_exit(0);
    out.stdout_str()
        .lines()
        .find(|line| line.starts_with("Number of files:"))
        .unwrap_or_else(|| panic!("no --stats files line, stdout:\n{}", out.stdout_str()))
        .to_owned()
}
#[test]
fn inc_recurse_pull_stats_count_every_sub_list_like_a_whole_list_pull() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    fixture(&tmp.path().join("src"));
    assert_eq!(
        files_line(tmp.path(), Some("0")),
        EXPECTED_FILES_LINE,
        "whole-list pull"
    );
    assert_eq!(
        files_line(tmp.path(), None),
        EXPECTED_FILES_LINE,
        "default incremental pull"
    );
    assert_eq!(
        files_line(tmp.path(), Some("1")),
        EXPECTED_FILES_LINE,
        "explicit incremental pull"
    );
}
