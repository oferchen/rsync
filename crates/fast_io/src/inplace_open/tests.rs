use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::{InplaceResolution, open_inplace_output};

const READ_ONLY: u32 = 0o444;

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

/// Arm 1: the ordinary case. An absent target is created, an existing one is
/// opened without truncation. upstream: receiver.c:1204-1209.
#[test]
fn the_first_arm_creates_an_absent_target_and_keeps_an_existing_one() {
    let dir = tempfile::tempdir().unwrap();

    let fresh = dir.path().join("fresh.bin");
    let mut file = open_inplace_output(&fresh, false, InplaceResolution::Direct).expect("created");
    file.write_all(b"payload").unwrap();
    drop(file);
    assert_eq!(fs::read(&fresh).unwrap(), b"payload");

    // No truncation: the existing bytes past the write survive, which is what
    // makes the destination usable as its own delta basis.
    let mut file = open_inplace_output(&fresh, false, InplaceResolution::Direct).expect("reopened");
    file.write_all(b"NEW").unwrap();
    drop(file);
    assert_eq!(fs::read(&fresh).unwrap(), b"NEWload");
}

/// Arm 3 reached through the chain: a 0444 target is recovered, and its mode is
/// restored by the time the descriptor arrives. Severing the third arm makes
/// this fail with the bare `EACCES` the chain exists to absorb.
/// upstream: receiver.c:1219-1224.
#[test]
fn the_third_arm_recovers_a_read_only_target() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readonly.bin");
    fs::write(&path, b"old").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(READ_ONLY)).unwrap();

    let mut file =
        open_inplace_output(&path, false, InplaceResolution::Direct).expect("recoverable");
    file.write_all(b"new").unwrap();
    drop(file);

    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert_eq!(mode_of(&path), READ_ONLY);
}

/// The caller's truncate choice must survive every arm, including the recovery.
/// Without this the read-only path would silently keep a tail the caller asked
/// to drop - a correct mode with wrong contents.
#[test]
fn the_truncate_choice_survives_the_recovery_arm() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readonly.bin");
    fs::write(&path, b"a longer old payload").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(READ_ONLY)).unwrap();

    let mut file =
        open_inplace_output(&path, true, InplaceResolution::Direct).expect("recoverable");
    file.write_all(b"new").unwrap();
    drop(file);

    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert_eq!(mode_of(&path), READ_ONLY);
}

/// The two resolutions are genuinely different code paths, and this is the
/// cheapest proof that `OperatorWalk` reaches the ownership walk rather than
/// silently degrading to a plain open: the walk refuses a path with no final
/// component with `EINVAL` (`/`, `.` and `..` name a directory, never a file to
/// open, and the walk would otherwise hand back its own anchor), where the
/// direct open reports the kernel's `EISDIR`.
///
/// A symlink cannot serve as the discriminator here: `ona_open` FOLLOWS a link
/// owned by uid 0 or our euid at every component, the leaf included - authority
/// is the trust signal, not the presence of a link (`syscall.c:406`). Both
/// resolutions therefore follow a self-owned link, and refusing a foreign-owned
/// one needs a second uid, which `owner_walk`'s own tests already cover.
#[test]
fn the_operator_walk_resolution_reaches_the_ownership_walk() {
    let dir = tempfile::tempdir().unwrap();
    let no_leaf = dir.path().join("sub").join("..");
    fs::create_dir(dir.path().join("sub")).unwrap();

    let walked = open_inplace_output(&no_leaf, false, InplaceResolution::OperatorWalk)
        .expect_err("the walk refuses a path with no final component");
    assert_eq!(walked.raw_os_error(), Some(libc::EINVAL));

    let direct = open_inplace_output(&no_leaf, false, InplaceResolution::Direct)
        .expect_err("control: the direct open refuses it too, differently");
    assert_eq!(
        direct.raw_os_error(),
        Some(libc::EISDIR),
        "control: the kernel's own refusal, so the errno above is the walk's"
    );
}

/// Non-vacuity for the cell above: the walk is not simply failing on every
/// input. A plain regular file under a self-owned parent opens through it, and
/// through the recovery arm too.
#[test]
fn the_operator_walk_opens_an_ordinary_target() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.bin");

    let mut file =
        open_inplace_output(&path, false, InplaceResolution::OperatorWalk).expect("created");
    file.write_all(b"payload").unwrap();
    drop(file);

    assert_eq!(fs::read(&path).unwrap(), b"payload");
    // 0600 is upstream's recv-open mode; the final mode is set_file_attrs' job.
    assert_eq!(mode_of(&path), 0o600);
}
