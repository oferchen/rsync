//! SEC-1.q2 integration tests: the receiver-side `--delete` scan and
//! per-entry removal route through the `fast_io::*_via_sandbox_or_fallback`
//! helpers when a [`DirSandbox`] is wired, so a TOCTOU symlink swap at
//! the top of the destination tree cannot redirect either the listing
//! or the unlink to an attacker-chosen inode outside the destination.
//!
//! These tests do not stand up a full receiver pipeline. They exercise
//! the three sandbox helpers the receiver-deletion loop now consumes
//! (`read_dir_via_sandbox_or_fallback`, `unlink_via_sandbox_or_fallback`,
//! `recursive_unlinkat_via_sandbox_or_fallback`) at the same anchor the
//! receiver uses (an open dirfd over the destination root), on a tree
//! shaped like an attacker-controlled extraneous-files scenario, and
//! assert the TOCTOU-relevant outcomes:
//!
//! 1. A symlink at the deletion target is removed itself rather than
//!    followed to its target outside the sandbox.
//! 2. A swapped-to-symlink subdirectory refuses to list via the sandbox
//!    helper (`ELOOP` / `ENOTDIR`), leaving the outside tree intact.
//! 3. With the sandbox absent, the helpers fall back to the path-based
//!    `std::fs` syscalls and produce byte-identical observable behaviour
//!    for the legitimate (non-swapped) case.

#![cfg(unix)]

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use fast_io::{
    DirSandbox, EntryKind, ReadDirOutcome, UnlinkFlags, read_dir_via_sandbox_or_fallback,
    recursive_unlinkat_via_sandbox_or_fallback, unlink_via_sandbox_or_fallback,
};
use tempfile::tempdir;

/// `tempdir()` may sit under a symlink prefix on macOS / some CI
/// runners; canonicalise so the sandbox open succeeds under
/// `RESOLVE_NO_SYMLINKS`.
fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}

/// Simulates the attack the receiver-side `--delete` loop defends
/// against on its symlink-removal branch: between the receiver's
/// decide-to-delete moment and the syscall, an attacker swaps the
/// extraneous file for a symlink to a sensitive target outside the
/// destination. The sandbox-anchored unlink removes the symlink itself;
/// the sensitive target must survive.
#[test]
fn delete_symlink_extraneous_does_not_follow_swap() {
    let (_keep, parent) = canonical_tempdir();

    // Sensitive tree outside the destination the attacker wants the
    // unlink to land on.
    let sensitive_dir = parent.join("sensitive");
    std::fs::create_dir(&sensitive_dir).expect("mkdir sensitive");
    let sensitive_file = sensitive_dir.join("secret");
    std::fs::write(&sensitive_file, b"do-not-delete").expect("write secret");

    let dest = parent.join("dest");
    std::fs::create_dir(&dest).expect("mkdir dest");
    // Receiver decides `dest/extraneous` is not in the sender's file
    // list; the attacker has swapped it for a symlink to the sensitive
    // tree above.
    let extraneous = dest.join("extraneous");
    symlink(&sensitive_dir, &extraneous).expect("symlink swap");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox");

    let leaf = Path::new("extraneous");
    unlink_via_sandbox_or_fallback(Some(&sandbox), &dest, leaf, &extraneous, UnlinkFlags::File)
        .expect("unlink leaf");

    assert!(
        !extraneous.exists(),
        "swapped symlink must be removed itself, not followed"
    );
    assert!(
        sensitive_dir.is_dir(),
        "sensitive directory outside the sandbox must survive"
    );
    assert!(
        sensitive_file.is_file(),
        "file inside sensitive directory must survive"
    );
}

/// Simulates the attack the receiver-side recursive-delete branch
/// defends against: the attacker swaps an extraneous subdirectory for a
/// symlink to a sensitive directory outside the destination. The
/// sandbox-anchored recursive unlink must refuse to follow the symlink
/// and leave the sensitive directory intact.
#[test]
fn delete_directory_swap_to_symlink_refuses_descent() {
    let (_keep, parent) = canonical_tempdir();

    let sensitive_dir = parent.join("sensitive");
    std::fs::create_dir(&sensitive_dir).expect("mkdir sensitive");
    std::fs::write(sensitive_dir.join("secret"), b"do-not-delete").expect("write secret");

    let dest = parent.join("dest");
    std::fs::create_dir(&dest).expect("mkdir dest");
    let swapped = dest.join("swapped");
    symlink(&sensitive_dir, &swapped).expect("symlink swap");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox");
    let leaf = Path::new("swapped");
    let err = recursive_unlinkat_via_sandbox_or_fallback(Some(&sandbox), &dest, leaf, &swapped)
        .expect_err("recursive unlinkat must refuse a symlinked descent root");
    let errno = err.raw_os_error();
    assert!(
        errno == Some(libc::ELOOP) || errno == Some(libc::ENOTDIR),
        "expected ELOOP or ENOTDIR (symlink not followed), got {err:?}"
    );
    assert!(
        sensitive_dir.is_dir(),
        "sensitive directory must survive the refused descent"
    );
    assert!(
        sensitive_dir.join("secret").is_file(),
        "secret file must survive"
    );
}

/// The receiver's scan of the destination root must refuse to list a
/// symlink that has been swapped in place of a subdirectory. The
/// `read_dir_via_sandbox_or_fallback` helper surfaces ELOOP / ENOTDIR,
/// which the receiver translates into a "skip this directory" branch
/// (it cannot enumerate what to delete and so deletes nothing inside),
/// and the outside tree is untouched.
#[test]
fn read_dir_swap_to_symlink_refuses_listing() {
    let (_keep, parent) = canonical_tempdir();

    let outside = parent.join("outside");
    std::fs::create_dir(&outside).expect("mkdir outside");
    std::fs::write(outside.join("sentinel"), b"do-not-touch").expect("sentinel");

    let dest = parent.join("dest");
    std::fs::create_dir(&dest).expect("mkdir dest");
    let scan_target = dest.join("subdir");
    symlink(&outside, &scan_target).expect("symlink swap");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox");

    let err =
        read_dir_via_sandbox_or_fallback(Some(&sandbox), &dest, Path::new("subdir"), &scan_target)
            .expect_err("symlink at listing leaf must be refused");
    let errno = err.raw_os_error();
    assert!(
        errno == Some(libc::ELOOP) || errno == Some(libc::ENOTDIR),
        "expected ELOOP or ENOTDIR (symlink not followed), got {err:?}"
    );
    assert!(
        outside.join("sentinel").is_file(),
        "outside sentinel must survive"
    );
}

/// Sandbox-anchored read_dir at the destination root reports every
/// extraneous entry with the correct classify bits the receiver uses to
/// dispatch between `unlink_via_sandbox_or_fallback` (files / symlinks /
/// devices) and `recursive_unlinkat_via_sandbox_or_fallback`
/// (directories).
#[test]
fn read_dir_at_root_classifies_entries_for_dispatch() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("file"), b"x").expect("write file");
    std::fs::create_dir(root.join("dir")).expect("mkdir dir");
    symlink(root.join("file"), root.join("link")).expect("symlink link");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let outcome = read_dir_via_sandbox_or_fallback(Some(&sandbox), &root, Path::new(""), &root)
        .expect("read_dir at root");
    assert!(matches!(outcome, ReadDirOutcome::At(_)));

    let mut by_name: std::collections::HashMap<_, _> = outcome
        .map(|res| {
            let view = res.expect("entry");
            (
                view.file_name().to_string_lossy().into_owned(),
                view.file_type(),
            )
        })
        .collect();
    assert_eq!(by_name.remove("file"), Some(Some(EntryKind::Other)));
    assert_eq!(by_name.remove("dir"), Some(Some(EntryKind::Dir)));
    assert_eq!(by_name.remove("link"), Some(Some(EntryKind::Symlink)));
    assert!(
        by_name.is_empty(),
        "no extra entries should remain: {by_name:?}"
    );
}

/// Sandbox-off behaviour: the helpers must produce byte-identical
/// observable outcomes to the pre-SEC-1.q2 path-based syscalls. Pins
/// the documented identical-behaviour-fallback contract so a future
/// regression cannot silently change the `None`-carrier semantics that
/// every existing `delete_extraneous_files` test relies on.
#[test]
fn sandbox_off_fallback_matches_std_for_legitimate_delete() {
    let (_keep, root) = canonical_tempdir();
    let dir = root.join("subdir");
    std::fs::create_dir(&dir).expect("mkdir subdir");
    std::fs::write(dir.join("a"), b"a").expect("write a");
    std::fs::write(dir.join("b"), b"b").expect("write b");

    let outcome =
        read_dir_via_sandbox_or_fallback(None, &root, Path::new("subdir"), &dir).expect("read_dir");
    assert!(matches!(outcome, ReadDirOutcome::Std(_)));
    let mut names: Vec<_> = outcome
        .map(|r| r.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["a", "b"]);

    let leaf = Path::new("a");
    let leaf_path = dir.join(leaf);
    // Multi-component path forces the path-based fallback even with a
    // sandbox; without a sandbox the helper takes the same branch
    // unconditionally.
    unlink_via_sandbox_or_fallback(
        None,
        &root,
        Path::new("subdir/a"),
        &leaf_path,
        UnlinkFlags::File,
    )
    .expect("unlink fallback");
    assert!(!leaf_path.exists(), "fallback unlink must remove file");
    assert!(dir.join("b").is_file(), "sibling file must survive");

    recursive_unlinkat_via_sandbox_or_fallback(None, &root, Path::new("subdir"), &dir)
        .expect("recursive unlink fallback");
    assert!(!dir.exists(), "fallback recursive unlink must remove dir");
}

/// upstream: `delete.c:100-101`/`141-142` `DEL_NO_UID_WRITE` - a destination
/// directory we own but cannot write to (no owner-write bit) must still be
/// emptied and removed under `--delete`, via a proactive chmod +w rather
/// than failing with `EACCES`. Exercised at the same sandbox anchor the
/// receiver's `delete_extraneous_files` recursive branch uses.
#[test]
fn delete_removes_owned_read_only_directory_containing_extraneous_file() {
    // SAFETY: geteuid(2) is a pure accessor; root bypasses every
    // permission check, which would make this test meaningless.
    #[allow(unsafe_code)]
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        return;
    }

    let (_keep, root) = canonical_tempdir();
    let dir = root.join("readonly");
    std::fs::create_dir(&dir).expect("mkdir readonly");
    std::fs::write(dir.join("extraneous"), b"stale").expect("write extraneous");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod 0555");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let result = recursive_unlinkat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        Path::new("readonly"),
        &dir,
    );
    // Defensive restore so tempdir cleanup can proceed even if the
    // assertion below fails.
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));

    result.expect("a read-only owned directory must be removable under --delete");
    assert!(!dir.exists(), "read-only directory must be gone");
}
