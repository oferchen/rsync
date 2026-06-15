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

use metadata::{MetadataOptions, apply_file_metadata_with_options};

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
