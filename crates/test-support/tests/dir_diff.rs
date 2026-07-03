//! Behavioral tests for [`DirDiff`].
//!
//! Each test encodes *why* the primitive exists: an upstream-compat port
//! transfers a tree and then asserts the destination matches a known-good
//! tree. These tests prove `DirDiff` catches the exact regressions that
//! would otherwise slip through a port silently passing - a dropped file,
//! wrong bytes, wrong permission bits, a file/dir type flip, or a symlink
//! whose target drifted. Equally, a clean tree must not report spurious
//! differences, or every port would be a false positive.

use std::fs;
use std::path::Path;

use test_support::{DirDiff, DirDiffEntry, DirDiffError, DirDiffOptions};

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn seed(root: &Path) {
    write(root, "top.txt", b"hello");
    write(root, "sub/a.bin", b"\x00\x01\x02\x03");
    write(root, "sub/deep/b.txt", b"nested");
    fs::create_dir_all(root.join("empty_dir")).unwrap();
}

#[test]
fn identical_trees_report_no_difference() {
    // Why: if DirDiff flagged equal trees, every passing port would be a
    // false positive. This is the base case every port depends on.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    seed(a.path());
    seed(b.path());

    let result = DirDiff::compare(a.path(), b.path(), DirDiffOptions::structural()).unwrap();
    assert!(result.is_ok(), "expected no differences, got {result:?}");
}

#[test]
fn dropped_file_is_reported_as_only_in_expected() {
    // Why: the most common transfer regression is a file that never
    // arrived. A port must fail loudly, not skip the missing path.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    seed(a.path());
    seed(b.path());
    fs::remove_file(b.path().join("sub/deep/b.txt")).unwrap();

    let mismatch = DirDiff::compare(a.path(), b.path(), DirDiffOptions::structural())
        .unwrap()
        .unwrap_err();
    assert!(mismatch.differences.iter().any(|d| matches!(
        d,
        DirDiffEntry::OnlyInExpected(p) if p == Path::new("sub/deep/b.txt")
    )));
}

#[test]
fn extra_file_is_reported_as_only_in_actual() {
    // Why: a stale file left in the destination (e.g. a --delete bug) must
    // be caught symmetrically with a dropped file.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    seed(a.path());
    seed(b.path());
    write(b.path(), "sub/unexpected.txt", b"stale");

    let mismatch = DirDiff::compare(a.path(), b.path(), DirDiffOptions::structural())
        .unwrap()
        .unwrap_err();
    assert!(mismatch.differences.iter().any(|d| matches!(
        d,
        DirDiffEntry::OnlyInActual(p) if p == Path::new("sub/unexpected.txt")
    )));
}

#[test]
fn content_mismatch_is_detected_when_length_matches() {
    // Why: a same-length payload with different bytes is exactly what a
    // broken delta or checksum bug produces. Length-only comparison would
    // miss it, so content must be compared byte-for-byte.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write(a.path(), "f", b"aaaaa");
    write(b.path(), "f", b"aaaab");

    let mismatch = DirDiff::compare(a.path(), b.path(), DirDiffOptions::structural())
        .unwrap()
        .unwrap_err();
    assert!(mismatch.differences.iter().any(|d| matches!(
        d,
        DirDiffEntry::ContentMismatch { path, .. } if path == Path::new("f")
    )));
}

#[test]
fn content_not_compared_when_check_content_disabled() {
    // Why: structure-only comparison must ignore bytes, so a port that
    // only cares about layout is not coupled to file content.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write(a.path(), "f", b"aaaaa");
    write(b.path(), "f", b"bbbbb");

    let opts = DirDiffOptions::default();
    let result = DirDiff::compare(a.path(), b.path(), opts).unwrap();
    assert!(result.is_ok(), "structure-only compare must ignore bytes");
}

#[test]
fn type_flip_file_to_dir_is_reported() {
    // Why: a path that is a file in one tree and a directory in the other
    // is a corruption a content-only check would mis-handle.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write(a.path(), "x", b"file");
    fs::create_dir_all(b.path().join("x")).unwrap();

    let mismatch = DirDiff::compare(a.path(), b.path(), DirDiffOptions::structural())
        .unwrap()
        .unwrap_err();
    assert!(mismatch.differences.iter().any(|d| matches!(
        d,
        DirDiffEntry::TypeMismatch { path, expected: "file", actual: "dir" } if path == Path::new("x")
    )));
}

#[test]
fn unsupported_options_error_instead_of_passing_silently() {
    // Why: Rule 12 - a port that asks for xattr comparison must never
    // believe it checked xattrs when the harness silently ignored them.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();

    let opts = DirDiffOptions {
        check_xattr: true,
        ..DirDiffOptions::default()
    };
    let err = DirDiff::compare(a.path(), b.path(), opts).unwrap_err();
    assert!(matches!(err, DirDiffError::Unsupported("check_xattr")));
}

#[cfg(unix)]
#[test]
fn mode_mismatch_is_detected() {
    // Why: rsync -p / -a must preserve permission bits; a mode regression
    // is invisible to content comparison alone.
    use std::os::unix::fs::PermissionsExt;

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write(a.path(), "f", b"same");
    write(b.path(), "f", b"same");
    fs::set_permissions(a.path().join("f"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(b.path().join("f"), fs::Permissions::from_mode(0o600)).unwrap();

    let mismatch = DirDiff::compare(a.path(), b.path(), DirDiffOptions::structural())
        .unwrap()
        .unwrap_err();
    assert!(mismatch.differences.iter().any(|d| matches!(
        d,
        DirDiffEntry::ModeMismatch { path, expected: 0o644, actual: 0o600 } if path == Path::new("f")
    )));
}

#[cfg(unix)]
#[test]
fn symlink_target_drift_is_detected_literally() {
    // Why: rsync -l recreates the link target string; a broken symlink
    // transfer points somewhere else. DirDiff compares the target
    // literally rather than dereferencing, matching -a semantics.
    use std::os::unix::fs::symlink;

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    symlink("target/one", a.path().join("link")).unwrap();
    symlink("target/two", b.path().join("link")).unwrap();

    let mismatch = DirDiff::compare(a.path(), b.path(), DirDiffOptions::archive())
        .unwrap()
        .unwrap_err();
    assert!(mismatch.differences.iter().any(|d| matches!(
        d,
        DirDiffEntry::SymlinkMismatch { path, .. } if path == Path::new("link")
    )));
}

#[cfg(unix)]
#[test]
fn matching_symlinks_are_equal_under_archive() {
    // Why: a correctly transferred symlink must not be flagged, or every
    // -a port with a symlink would be a false positive.
    use std::os::unix::fs::symlink;

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    symlink("same/target", a.path().join("link")).unwrap();
    symlink("same/target", b.path().join("link")).unwrap();

    let result = DirDiff::compare(a.path(), b.path(), DirDiffOptions::archive()).unwrap();
    assert!(
        result.is_ok(),
        "matching symlinks must compare equal: {result:?}"
    );
}

#[test]
fn large_file_streaming_compare_matches() {
    // Why: the streaming path (> 1 MiB) must produce the same verdict as
    // the small-file path, so a big basis file is not a blind spot.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let big: Vec<u8> = (0..(2 * 1024 * 1024u32)).map(|i| i as u8).collect();
    write(a.path(), "big", &big);
    write(b.path(), "big", &big);

    assert!(
        DirDiff::compare(a.path(), b.path(), DirDiffOptions::structural())
            .unwrap()
            .is_ok()
    );

    // Flip one byte deep in the file; the streaming compare must catch it.
    let mut corrupted = big.clone();
    let mid = corrupted.len() / 2;
    corrupted[mid] ^= 0xff;
    write(b.path(), "big", &corrupted);

    let mismatch = DirDiff::compare(a.path(), b.path(), DirDiffOptions::structural())
        .unwrap()
        .unwrap_err();
    assert!(mismatch.differences.iter().any(|d| matches!(
        d,
        DirDiffEntry::ContentMismatch { path, .. } if path == Path::new("big")
    )));
}
