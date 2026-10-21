//! `--remove-source-files` unlinks every transferred source when incremental
//! recursion splits the file list into many sub-lists.
//!
//! upstream: io.c:1831-1845 runs `successful_send()` for each `MSG_SUCCESS`
//! the moment it is read, so the confirmations for a sub-list arrive ahead of
//! the `NDX_DONE` after which sender.c:535-536 frees that list. A sender that
//! frees a list's entries first and confirms later has no source paths left to
//! unlink: before the fix every source outside the newest sub-list survived
//! and the run ended with exit 23.
#![cfg(unix)]
use std::fs;
use std::path::Path;
use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};
/// Twelve directories, each with files and a subdirectory, so the sender
/// emits and frees well over a dozen sub-lists.
fn fixture(src: &Path) -> usize {
    let mut files = 0;
    for dir in 1..=12 {
        for name in ["f1", "f2", "s/g1"] {
            let path = src.join(format!("d{dir}/{name}"));
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir src");
            fs::write(&path, format!("{dir}/{name}")).expect("write src file");
            files += 1;
        }
    }
    files
}
fn regular_files(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(
                    path.strip_prefix(root)
                        .expect("under root")
                        .display()
                        .to_string(),
                );
            }
        }
    }
    found.sort();
    found
}
/// Runs one `--remove-source-files` transfer through the lsh stub and checks
/// that every source is gone, every file arrived, and the run exited 0.
fn assert_every_source_removed(pull: bool) {
    let tmp = create_tempdir();
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    let expected = fixture(&src);
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let (from, to) = if pull {
        (
            format!("localhost:{}/", src.display()),
            format!("{}/", dest.display()),
        )
    } else {
        (
            format!("{}/", src.display()),
            format!("localhost:{}/", dest.display()),
        )
    };
    let out = OcRsyncCliRunner::new()
        .arg("-r")
        .arg("--remove-source-files")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(from)
        .arg(to)
        .run()
        .expect("transfer run");
    out.assert_exit(0);
    assert_eq!(regular_files(&dest).len(), expected, "every file arrives");
    assert_eq!(
        regular_files(&src),
        Vec::<String>::new(),
        "every source is unlinked"
    );
}
#[test]
fn pull_with_remove_source_files_unlinks_every_sub_lists_sources() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    assert_every_source_removed(true);
}
#[test]
fn push_with_remove_source_files_unlinks_every_sub_lists_sources() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    assert_every_source_removed(false);
}
