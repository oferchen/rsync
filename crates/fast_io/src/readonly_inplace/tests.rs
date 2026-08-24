use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::open_readonly_inplace;
use crate::inplace_open::InplaceResolution;

const READ_ONLY: u32 = 0o444;

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn plant_readonly(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(READ_ONLY)).unwrap();
    path
}

/// The recovery itself: a 0444 regular file becomes writable, and the mode is
/// already restored by the time the descriptor is returned - which is why no
/// abort path can strand it at 0600. upstream: receiver.c:200-241.
#[test]
fn a_read_only_file_is_writable_and_its_mode_is_restored_on_return() {
    let dir = tempfile::tempdir().unwrap();
    let path = plant_readonly(dir.path(), "basis.bin", b"old");

    let mut file =
        open_readonly_inplace(&path, false, InplaceResolution::Direct).expect("recoverable");

    assert_eq!(
        mode_of(&path),
        READ_ONLY,
        "restored while the fd is still open"
    );
    file.write_all(b"new").unwrap();
    drop(file);

    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert_eq!(mode_of(&path), READ_ONLY);
}

/// Upstream refuses rather than chmods when the file is already owner-writable:
/// the EACCES came from an ACL or the parent directory, so a chmod cannot help
/// and would risk dropping a special bit. upstream: receiver.c:224-230.
#[test]
fn an_owner_writable_file_is_refused_instead_of_chmodded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("writable.bin");
    fs::write(&path, b"old").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    let error = open_readonly_inplace(&path, false, InplaceResolution::Direct)
        .expect_err("must be refused");

    assert_eq!(error.raw_os_error(), Some(libc::EACCES));
    assert_eq!(mode_of(&path), 0o644, "a refusal must not spend a chmod");
}

/// The recovery pins an inode: `O_NOFOLLOW` refuses a symlink at the leaf, so a
/// planted link cannot redirect the owner-write grant onto another file.
/// upstream: receiver.c:216.
#[test]
fn a_symlink_at_the_leaf_is_refused_without_touching_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = plant_readonly(dir.path(), "target.bin", b"old");
    let link = dir.path().join("link.bin");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    open_readonly_inplace(&link, false, InplaceResolution::Direct)
        .expect_err("a symlink leaf must be refused");

    assert_eq!(mode_of(&target), READ_ONLY, "the target keeps its mode");
    assert_eq!(fs::read(&target).unwrap(), b"old");
}

/// Only a regular file is recoverable. The directory is planted 0555 so that it
/// is *not* owner-writable: EACCES can then only come from the type check, which
/// pins that the type check runs before the owner-write rule.
/// upstream: receiver.c:219-222.
#[test]
fn a_non_regular_target_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let subdir = dir.path().join("subdir");
    fs::create_dir(&subdir).unwrap();
    fs::set_permissions(&subdir, fs::Permissions::from_mode(0o555)).unwrap();

    let error = open_readonly_inplace(&subdir, false, InplaceResolution::Direct)
        .expect_err("must be refused");

    assert_eq!(error.raw_os_error(), Some(libc::EACCES));
}

/// The caller owns its open semantics: the local-copy path truncates when there
/// is no delta basis, the receiver never does. Passing `truncate` must therefore
/// still truncate through the recovery.
#[test]
fn the_callers_truncate_choice_is_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let path = plant_readonly(dir.path(), "basis.bin", b"a longer old payload");

    let mut file =
        open_readonly_inplace(&path, true, InplaceResolution::Direct).expect("recoverable");
    file.write_all(b"new").unwrap();
    drop(file);

    assert_eq!(fs::read(&path).unwrap(), b"new", "truncate was honoured");
    assert_eq!(mode_of(&path), READ_ONLY);
}

/// The resolution is threaded into the recovery, not only into the arms above
/// it: under `OperatorWalk` a parent flipped to a foreign-owned symlink must be
/// refused here too. Without this the recovery would be the one arm that still
/// walks an attacker's parent by path.
///
/// The symlink is planted owned by *this* euid, which the walk trusts, so the
/// refusal cannot come from ownership - it has to come from the walk running at
/// all versus not. See the companion below for the discriminator.
/// upstream: receiver.c:214 passes `one_inplace` into `secure_recv_open()`.
#[test]
fn the_operator_walk_resolution_reaches_the_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    fs::create_dir(&real).unwrap();
    let target = plant_readonly(&real, "leaf.bin", b"old");

    let via_link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &via_link).unwrap();

    // A self-owned parent symlink IS trusted, so the walk follows it and the
    // recovery still succeeds - the non-vacuity half of the pair below.
    let mut file = open_readonly_inplace(
        &via_link.join("leaf.bin"),
        false,
        InplaceResolution::OperatorWalk,
    )
    .expect("a self-owned parent symlink is trusted");
    file.write_all(b"new").unwrap();
    drop(file);

    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(
        mode_of(&target),
        READ_ONLY,
        "mode restored through the walk"
    );
}
