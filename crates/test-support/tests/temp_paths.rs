//! The temp-path helpers must give every path the `.tmp<pid>-` prefix.
//!
//! A plain `tempfile` name can collide across concurrent nextest processes,
//! and on Windows that collision fails with `PermissionDenied` instead of
//! being retried. These tests fail if a helper falls back to plain naming.

use std::path::Path;

use test_support::{create_canonical_tempdir, create_named_tempfile, create_tempdir, temp_prefix};

fn assert_pid_prefixed(path: &Path) {
    let name = path.file_name().expect("temp path has a file name");
    let name = name.to_str().expect("temp name is UTF-8");
    let prefix = format!(".tmp{}-", std::process::id());
    assert!(
        name.starts_with(&prefix),
        "{name:?} lacks the per-process prefix {prefix:?}"
    );
}

#[test]
fn prefix_embeds_this_process_id() {
    assert_eq!(temp_prefix(), format!(".tmp{}-", std::process::id()));
}

#[test]
fn tempdir_name_is_pid_prefixed() {
    let dir = create_tempdir();
    assert!(dir.path().is_dir());
    assert_pid_prefixed(dir.path());
}

#[test]
fn named_tempfile_name_is_pid_prefixed() {
    let file = create_named_tempfile();
    assert!(file.path().is_file());
    assert_pid_prefixed(file.path());
}

#[test]
fn canonical_tempdir_is_pid_prefixed_and_resolved() {
    let (dir, canon) = create_canonical_tempdir();
    assert_pid_prefixed(&canon);
    assert_eq!(canon, dir.path().canonicalize().expect("canonicalize"));
}
