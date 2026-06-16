//! Verify that a child file's destination mode inherits from the parent
//! directory's POSIX default ACL when `--perms` is off, matching upstream
//! rsync's `default_perms_for_dir()` semantics.
//!
//! Closes the gap that caused upstream testsuite `acls-default.test` to fail:
//! oc-rsync's `compute_dest_mode()` previously seeded `dflt_perms` from
//! umask alone, ignoring the destination directory's default ACL.
//!
//! # Upstream Reference
//!
//! - `acls.c:1083-1139` `default_perms_for_dir`
//! - `generator.c:1337-1339` per-parent `dflt_perms` lookup driving
//!   `dest_mode()` when `!preserve_perms`.

#![cfg(all(target_os = "linux", feature = "acl"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use exacl::{AclEntry, AclOption, Perm, setfacl};
use tempfile::tempdir;

use metadata::{MetadataOptions, apply_file_metadata_with_options, default_perms_for_dir};

/// Creates a destination directory with a POSIX default ACL granting
/// user::rwx, group::r-x, other::r-x, then transfers a file whose source
/// mode is `0o644` with `--perms` disabled. The applied mode must reflect
/// the parent's default ACL bits (`0o755`) ANDed with the source mode's
/// CHMOD bits, not the umask-derived fallback.
#[test]
fn child_file_inherits_parent_default_acl_when_perms_disabled() {
    let dir = tempdir().expect("tempdir");
    let dest_parent = dir.path().join("dest_parent");
    fs::create_dir(&dest_parent).expect("create dest_parent");

    let default_entries = vec![
        AclEntry::allow_user("", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        AclEntry::allow_group("", Perm::READ | Perm::EXECUTE, None),
        AclEntry::allow_other(Perm::READ | Perm::EXECUTE, None),
    ];
    if setfacl(
        &[&dest_parent],
        &default_entries,
        Some(AclOption::DEFAULT_ACL),
    )
    .is_err()
    {
        // tmpfs without acl mount option, or other unsupported filesystem.
        // Upstream takes the same umask-fallback branch in this case; we
        // cannot exercise the inheritance path here.
        return;
    }

    let source = dir.path().join("source");
    fs::write(&source, b"payload").expect("write source");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("chmod source");
    let source_meta = fs::metadata(&source).expect("stat source");

    let destination = dest_parent.join("child");
    fs::write(&destination, b"payload").expect("write destination");
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))
        .expect("chmod destination");

    let options = MetadataOptions::default()
        .preserve_permissions(false)
        .with_destination_is_new(true);

    apply_file_metadata_with_options(&destination, &source_meta, &options)
        .expect("apply file metadata");

    let applied_mode = fs::metadata(&destination)
        .expect("stat destination")
        .permissions()
        .mode()
        & 0o7777;

    // upstream: rsync.c:dest_mode() new-file branch is
    //   `source_mode & (~CHMOD_BITS | dflt_perms)`, which for source 0o644
    //   and dflt_perms = 0o755 (the parent's default ACL) yields 0o644.
    // Without the fix the umask-derived seed would clear the world-execute
    // bit and the destination would diverge from upstream's behaviour on
    // filesystems carrying a default ACL.
    assert_eq!(
        applied_mode, 0o644,
        "child mode {applied_mode:o} did not honour parent default ACL"
    );
}

/// When a default ACL carries a `mask` entry, upstream's
/// `rsync_acl_get_perms` returns the mask in the middle three bits instead
/// of the `group_obj`. This matches POSIX semantics where the mask is the
/// effective upper bound for the group entry.
///
/// Pins the upstream `acls-default.test` `da750mask` scenario: default ACL
/// `u::7,u:0:7,g::7,m::5,o::0` must collapse to `0o750` (group_obj=7 masked
/// to 5), not `0o770`.
///
/// # Upstream Reference
///
/// - `acls.c:129-134` `rsync_acl_get_perms` mask-vs-group_obj precedence
#[test]
fn default_perms_honours_mask_over_group_obj() {
    let dir = tempdir().expect("tempdir");
    let probe = dir.path().join("probe");
    fs::create_dir(&probe).expect("create probe");

    // upstream testsuite acls-default.test `da750mask` opts:
    //   d:u::7,d:u:0:7,d:g::7,d:m:5,d:o:0
    let default_entries = vec![
        AclEntry::allow_user("", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        AclEntry::allow_user("root", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        AclEntry::allow_group("", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        AclEntry::allow_mask(Perm::READ | Perm::EXECUTE, None),
        AclEntry::allow_other(Perm::empty(), None),
    ];
    if setfacl(&[&probe], &default_entries, Some(AclOption::DEFAULT_ACL)).is_err() {
        // tmpfs without acl mount option, or other unsupported filesystem.
        return;
    }

    // Pass umask = 0 so the fallback path cannot mask the ACL-derived bits.
    let perms = default_perms_for_dir(&probe, 0);
    assert_eq!(
        perms, 0o750,
        "mask entry not honoured: got {perms:o}, expected 0o750"
    );
}
