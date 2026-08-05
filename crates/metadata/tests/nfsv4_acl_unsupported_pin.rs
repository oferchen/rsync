//! TEST-PIN: NFSv4-ACL degradation on a non-macOS/Windows platform whose
//! filesystem does not support NFSv4 ACLs (the common Linux case -
//! `system.nfs4_acl` is only implemented on actual NFSv4 mounts; ext4 / tmpfs /
//! overlayfs return `EOPNOTSUPP`).
//!
//! The behavior being pinned mirrors upstream `acls.c`'s two-sided degradation
//! intent:
//!
//! - READ (`get_acl`, acls.c:525-527): an unsupported filesystem is treated as
//!   "no ACL present" (upstream fakes a basic ACL) rather than an error. oc's
//!   `get_nfsv4_acl` mirrors this: an absent / unsupported NFSv4 ACL decodes to
//!   `None`, silently.
//! - WRITE (`set_acl` -> `sys_acl_set_file`, acls.c:993-996): a failed apply is
//!   SURFACED via `rsyserr(FERROR_XFER, ...)` and `return -1` - it is never
//!   silently swallowed as a success. oc's `set_nfsv4_acl` mirrors this: an
//!   NFSv4 ACL that cannot be applied returns `Err`, so the receiver never
//!   pretends it preserved metadata it actually dropped.
//!
//! Gated to Linux: macOS stores an arbitrary-named `system.nfs4_acl` xattr as a
//! plain user attribute (the write would succeed), and Windows has no xattrs at
//! all, so the "unsupported filesystem" precondition only holds here.

#![cfg(all(target_os = "linux", feature = "xattr"))]

use metadata::nfsv4_acl::{
    AccessMask, AceFlags, AceType, Nfs4Ace, Nfs4Acl, get_nfsv4_acl, set_nfsv4_acl,
};
use tempfile::tempdir;

fn sample_acl() -> Nfs4Acl {
    Nfs4Acl {
        aces: vec![Nfs4Ace {
            ace_type: AceType::Allow,
            flags: AceFlags::default(),
            mask: AccessMask::from_raw(0x1f),
            who: "OWNER@".to_string(),
        }],
    }
}

/// The core pin: applying a non-empty NFSv4 ACL to a filesystem that does not
/// support one must SURFACE the failure (`Err`), never silently succeed. A
/// silent `Ok(())` here would let a transfer report success while dropping the
/// ACL the user asked to preserve.
#[test]
fn applying_nfsv4_acl_on_unsupported_fs_surfaces_error_not_silent_success() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("f");
    std::fs::write(&file, b"x").expect("write file");

    let err = set_nfsv4_acl(&file, Some(&sample_acl()), false).expect_err(
        "applying an NFSv4 ACL to a filesystem that does not support it must surface \
         an error, not silently succeed",
    );

    // The surfaced error identifies the failed NFSv4 ACL apply (upstream's
    // rsyserr also names the operation and path).
    let msg = err.to_string();
    assert!(
        msg.contains("NFSv4 ACL"),
        "the error must identify the failed NFSv4 ACL apply: {msg}"
    );
}

/// The read side is a SILENT absence, matching upstream `get_acl`'s
/// "unsupported filesystem -> pretend a basic ACL": a file on a filesystem with
/// no NFSv4 ACL support decodes to `None`, not an error, so a plain source file
/// never spuriously fails the ACL read.
#[test]
fn reading_nfsv4_acl_on_unsupported_fs_is_a_silent_absence() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("f");
    std::fs::write(&file, b"x").expect("write file");

    let acl = get_nfsv4_acl(&file, false)
        .expect("reading a missing / unsupported NFSv4 ACL must not be an error");
    assert!(
        acl.is_none(),
        "a normal filesystem has no NFSv4 ACL, so the read decodes to None"
    );
}

/// Clearing an absent NFSv4 ACL (`set_nfsv4_acl(None)`, which removes the
/// attribute) tolerates a missing attribute rather than surfacing it - so a
/// source WITHOUT an NFSv4 ACL never fails the destination apply. This is the
/// deliberate asymmetry with the apply path above: removing nothing is success,
/// applying-but-cannot is an error.
#[test]
fn clearing_absent_nfsv4_acl_is_a_tolerated_no_op() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("f");
    std::fs::write(&file, b"x").expect("write file");

    set_nfsv4_acl(&file, None, false)
        .expect("clearing an absent NFSv4 ACL must be a tolerated no-op success");
}
