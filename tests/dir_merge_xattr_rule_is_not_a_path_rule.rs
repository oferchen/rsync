//! An `x` rule in a per-directory merge file must not filter FILE names.
//!
//! upstream: exclude.c:1013 - `if (!(name_flags & NAME_IS_XATTR) ^
//! !(ex->rflags & FILTRULE_XATTR)) return 0;`. A rule carrying `FILTRULE_XATTR`
//! matches xattr names and nothing else, so it can never exclude a path.
//!
//! Measured against rsync 3.5.0: with `- x user.foo` (i.e. `-x user.foo`) in a
//! `.rsync-filter`, upstream transfers a FILE named `user.foo`; oc routed the
//! rule onto the path chain and silently dropped the file. `anchor_dir_merge_rule`
//! even prefixed the merge file's directory, so the "xattr name" became
//! `sub/user.foo`.
//!
//! ⚠ oc has no per-directory xattr chain, so such a rule is currently inert
//! rather than applied to xattr names. That residual is named in the PR; this
//! test pins only the half that was actively wrong - deleting the wrong file.

use std::fs;
use std::process::Command;

use tempfile::TempDir;
use test_support::oc_rsync_bin;

#[test]
fn a_dir_merge_xattr_rule_does_not_exclude_a_same_named_file() {
    let temp = TempDir::new().expect("tempdir");
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dst).expect("create dst");
    fs::write(src.join("user.foo"), b"payload\n").expect("write user.foo");
    fs::write(src.join("other.txt"), b"payload\n").expect("write other.txt");
    fs::write(src.join(".rsync-filter"), b"-x user.foo\n").expect("write filter");

    let status = Command::new(oc_rsync_bin())
        .arg("-rF")
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .status()
        .expect("run oc-rsync");
    assert!(status.success(), "transfer must succeed, got {status:?}");

    assert!(
        dst.join("user.foo").exists(),
        "an `x` rule matches xattr names only; the FILE named user.foo must transfer"
    );
    assert!(
        dst.join("other.txt").exists(),
        "non-vacuity: the transfer really ran and carried an unrelated file"
    );
}
