//! `--existing` skips a destination directory that does not exist, and that
//! skip is not an error.
//!
//! upstream: generator.c:1755-1761 - with `ignore_non_existing` an absent
//! directory sets `skip_dir` and `FLAG_MISSING_DIR`, and generator.c:1646-1658
//! returns early for every entry below it. Neither path touches `io_error`, so
//! the pull exits 0. Under incremental recursion each sub-list reaches the
//! receiver separately; a receiver that records the skipped directory as a
//! failed mkdir counts every descendant as a failure and exits 23 with no
//! error printed. The UTS `exclude-lsh` cell runs exactly this pull.
#![cfg(unix)]
use std::fs;
use std::path::Path;
use test_support::{
    CliOutput, LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};
const PULL_INC_RECURSE_ENV: &str = "OC_RSYNC_PULL_INC_RECURSE";
/// Builds `from/` with directories the destination lacks (`foo/down/...`,
/// `new/...`) and `chk/` holding only the top-level `foo` and `bar`.
fn fixture(root: &Path) {
    for file in [
        "foo/down/to/you",
        "new/keep/this",
        "new/lose/this",
        "bar/file",
    ] {
        let path = root.join("from").join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir from");
        fs::write(&path, file.as_bytes()).expect("write from file");
    }
    fs::create_dir_all(root.join("chk/foo")).expect("mkdir chk/foo");
    fs::create_dir_all(root.join("chk/bar")).expect("mkdir chk/bar");
}
fn pull(root: &Path, inc_recurse: bool, filters: &[&str]) -> CliOutput {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    OcRsyncCliRunner::new()
        .env(PULL_INC_RECURSE_ENV, if inc_recurse { "1" } else { "0" })
        .arg("-av")
        .arg("--existing")
        .args(filters)
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("localhost:{}/", root.join("from").display()))
        .arg(format!("{}/", root.join("chk").display()))
        .run()
        .expect("pull run")
}
/// Lists every path under `dir`, relative and sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            out.push(
                path.strip_prefix(base)
                    .expect("relative")
                    .display()
                    .to_string(),
            );
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}
fn assert_skipped_cleanly(out: &CliOutput, root: &Path, expected_tree: &[&str]) {
    out.assert_exit(0);
    assert!(
        !out.stderr_contains("some files/attrs were not transferred"),
        "an --existing skip is not an error, stderr:\n{}",
        out.stderr_str()
    );
    assert_eq!(tree(&root.join("chk")), expected_tree);
}
fn assert_dirs_only_pull(inc_recurse: bool) {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    fixture(tmp.path());
    let out = pull(tmp.path(), inc_recurse, &["--include=*/", "--exclude=*"]);
    assert_skipped_cleanly(&out, tmp.path(), &["bar", "foo"]);
    // upstream: generator.c:1505 names only directories set_file_attrs()
    // changed; a skipped missing directory is never named.
    for missing in ["foo/down/", "new/"] {
        assert!(
            !out.stdout_str().lines().any(|line| line == missing),
            "skipped directory {missing} must not be listed, stdout:\n{}",
            out.stdout_str()
        );
    }
}
#[test]
fn inc_recurse_pull_existing_skips_missing_dirs_and_exits_0() {
    assert_dirs_only_pull(true);
}
#[test]
fn eager_pull_existing_skips_missing_dirs_and_exits_0() {
    assert_dirs_only_pull(false);
}
/// Entries below a skipped directory are skipped silently too: upstream
/// returns at generator.c:1646-1656 before the "not creating new" notices, so
/// `--info=skip` names only the topmost missing directories, by their
/// transfer-relative names.
#[test]
fn inc_recurse_pull_existing_skips_entries_below_missing_dirs_silently() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    fixture(tmp.path());
    let out = pull(tmp.path(), true, &["--info=skip"]);
    assert_skipped_cleanly(&out, tmp.path(), &["bar", "foo"]);
    let mut notices: Vec<String> = out
        .stdout_str()
        .lines()
        .filter(|line| line.starts_with("not creating new"))
        .map(str::to_owned)
        .collect();
    notices.sort();
    assert_eq!(
        notices,
        [
            "not creating new directory \"foo/down\"",
            "not creating new directory \"new\"",
            "not creating new file \"bar/file\"",
        ]
    );
}
