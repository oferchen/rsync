//! `--delete-excluded` under an incremental-recursion pull, where the receiver
//! deletes per directory as each sub-list arrives.
//!
//! upstream: generator.c:2780-2801 runs delete_in_dir() for each sub-list's
//! parent directory, and exclude.c:1103-1108 drops the sender-side excludes
//! from the receiver's rules under --delete-excluded, so an excluded
//! destination entry is a deletion candidate like any other extraneous one.
//! Without --delete-excluded the same exclude protects it.
#![cfg(unix)]

use std::fs;
use std::path::Path;

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

/// Twelve directories, so the deletions happen in per-segment passes rather
/// than in one sweep of a single list.
const DIRS: usize = 12;

fn fixture(src: &Path, dest: &Path) {
    for dir in 1..=DIRS {
        let path = src.join(format!("d{dir}/s/keep"));
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir src");
        fs::write(&path, b"keep").expect("write src file");

        let stale = dest.join(format!("d{dir}/s"));
        fs::create_dir_all(stale.join("cache")).expect("mkdir dest");
        fs::write(stale.join("stale"), b"stale").expect("write stale");
        fs::write(stale.join("junk.tmp"), b"tmp").expect("write excluded file");
        fs::write(stale.join("cache/blob"), b"blob").expect("write excluded dir");
    }
}

fn pull(src: &Path, dest: &Path, delete_excluded: bool) {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let mut runner = OcRsyncCliRunner::new()
        .arg("-r")
        .arg("--delete")
        .arg("--exclude=*.tmp")
        .arg("--exclude=cache/")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ));
    if delete_excluded {
        runner = runner.arg("--delete-excluded");
    }
    runner
        .arg(format!("localhost:{}/", src.display()))
        .arg(format!("{}/", dest.display()))
        .run()
        .expect("transfer run")
        .assert_exit(0);
}

/// The per-directory state of every segment's `s` directory after the pull.
fn survivors(dest: &Path, name: &str) -> usize {
    (1..=DIRS)
        .filter(|dir| dest.join(format!("d{dir}/s/{name}")).exists())
        .count()
}

#[test]
fn delete_excluded_removes_excluded_entries_in_every_segment() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let (src, dest) = (tmp.path().join("src"), tmp.path().join("dest"));
    fixture(&src, &dest);

    pull(&src, &dest, true);

    assert_eq!(survivors(&dest, "keep"), DIRS, "every source file arrives");
    assert_eq!(survivors(&dest, "stale"), 0, "extraneous files are deleted");
    assert_eq!(
        survivors(&dest, "junk.tmp"),
        0,
        "excluded files are deleted"
    );
    assert_eq!(
        survivors(&dest, "cache"),
        0,
        "excluded directories are deleted"
    );
}

/// The control: the same excludes protect the destination entries when
/// --delete-excluded is absent, so the deletions above are its doing.
#[test]
fn excludes_protect_destination_entries_without_delete_excluded() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let (src, dest) = (tmp.path().join("src"), tmp.path().join("dest"));
    fixture(&src, &dest);

    pull(&src, &dest, false);

    assert_eq!(survivors(&dest, "keep"), DIRS, "every source file arrives");
    assert_eq!(survivors(&dest, "stale"), 0, "extraneous files are deleted");
    assert_eq!(survivors(&dest, "junk.tmp"), DIRS, "excluded files survive");
    assert_eq!(
        survivors(&dest, "cache/blob"),
        DIRS,
        "excluded dirs survive"
    );
}
