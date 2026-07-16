//! Regression coverage for `security.selinux` xattr preservation under
//! `--xattrs` (LSM-SEL.1).
//!
//! RHEL / Fedora / CentOS hosts rely on `security.selinux` labels being
//! preserved byte-for-byte when files are copied across hosts. Any
//! divergence here silently breaks SELinux-protected services
//! (`httpd`, `postgresql`, `sshd`, ...) when the receiver runs in
//! enforcing mode and the new file lands with the default
//! `unconfined_u:object_r:default_t:s0` label instead of the source's
//! domain-typed label.
//!
//! Upstream `xattrs.c:rsync_xal_get()` (lines 237, 254-258) lets the
//! `security.*` namespace flow through the sender unless the sender is
//! non-root, and `xattrs.c:receive_xattr()` (lines 828-839) keeps the
//! verbatim name on the receiver side whenever the receiver is root.
//! oc-rsync mirrors this in `crates/metadata/src/xattr.rs` via
//! [`is_xattr_permitted`] / [`local_to_wire`] / [`wire_to_local`].
//!
//! ## Skip conditions
//!
//! These tests skip cleanly (no failure) when any of the following hold:
//! - The target is not Linux (only Linux has the `security.*` namespace).
//! - The `xattr` feature is disabled (the API surface under test is gone).
//! - The effective UID is not 0 (`security.*` writes always need root or
//!   `CAP_SYS_ADMIN`; the kernel returns `EPERM` otherwise and there is
//!   nothing for the regression to prove).
//! - The backing filesystem refuses `security.*` writes (`tmpfs` without
//!   the right LSM hooks, no SELinux/Smack policy loaded, container with
//!   the namespace dropped, etc.).
//!
//! The skip path always logs the reason so unprivileged CI runs surface
//! the gap loudly instead of pretending the round-trip is covered.

#![cfg(all(target_os = "linux", feature = "xattr"))]

use std::fs;
use std::path::Path;

use metadata::{apply_xattrs_from_list, sync_xattrs};
use protocol::xattr::{XattrEntry, XattrList};
use tempfile::tempdir;

/// Canonical SELinux label used by the round-trip fixtures.
///
/// The exact value does not need to be a real loaded type - the receiver
/// kernel either accepts it (SELinux/Smack loaded) or rejects the write
/// outright, in which case the test skips. Picking a vendor-typical
/// 4-field label keeps the assertion readable in failure output.
const FIXTURE_LABEL: &[u8] = b"unconfined_u:object_r:httpd_sys_content_t:s0";

/// Logs a skip reason in a format consistent with the surrounding
/// integration tests (e.g. `acl_root_root_interop.rs`).
fn skip(reason: &str) {
    eprintln!("[skip] security_selinux_roundtrip: {reason}");
}

/// Returns `true` when the effective UID is 0. `security.*` writes require
/// either root or `CAP_SYS_ADMIN`; in unprivileged CI the kernel will
/// reject every fixture write with `EPERM` and there is nothing to
/// assert.
fn running_as_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// Probes whether the backing filesystem accepts `security.*` xattr
/// writes for the current process. Returns `false` when the kernel
/// rejects the probe (no LSM loaded, kernel namespace stripped, FS
/// driver refuses the namespace, ...).
fn security_xattrs_supported(path: &Path) -> bool {
    match xattr::set(path, "security.selinux", FIXTURE_LABEL) {
        Ok(()) => {
            let _ = xattr::remove(path, "security.selinux");
            true
        }
        Err(_) => false,
    }
}

/// Verifies `sync_xattrs` copies a `security.selinux` label byte-for-byte
/// from `source` to `destination` on Linux when both files live on a
/// filesystem that accepts the namespace.
#[test]
fn security_selinux_xattr_round_trips() {
    if !running_as_root() {
        skip("requires root (security.* needs CAP_SYS_ADMIN)");
        return;
    }

    let dir = tempdir().expect("create temp dir");
    let source = dir.path().join("source");
    let destination = dir.path().join("dest");
    fs::write(&source, b"payload").expect("write source");
    fs::write(&destination, b"payload").expect("write dest");

    if !security_xattrs_supported(&source) {
        skip("backing filesystem rejects security.* writes (no SELinux/Smack policy?)");
        return;
    }

    xattr::set(&source, "security.selinux", FIXTURE_LABEL).expect("seed source label");

    sync_xattrs(&source, &destination, false, None).expect("sync xattrs");

    let landed = xattr::get(&destination, "security.selinux")
        .expect("read dest label")
        .expect("dest must carry security.selinux");

    assert_eq!(
        landed, FIXTURE_LABEL,
        "security.selinux must round-trip byte-for-byte under --xattrs"
    );
}

/// Verifies that wire-driven receiver application (the
/// [`apply_xattrs_from_list`] path used by transfer code) preserves
/// `security.selinux` even when the destination filesystem has no
/// SELinux policy enforcing the label - the receiver only has to store
/// the raw bytes; the kernel decides whether to honour them later.
///
/// This mirrors the cross-host scenario where a SELinux-enforcing sender
/// pushes content to a non-SELinux destination (e.g. for backup): the
/// bytes must survive the transfer so a subsequent restore lands the
/// correct label.
#[test]
fn security_selinux_xattr_preserved_via_wire_path() {
    if !running_as_root() {
        skip("requires root (security.* needs CAP_SYS_ADMIN)");
        return;
    }

    let dir = tempdir().expect("create temp dir");
    let destination = dir.path().join("dest");
    fs::write(&destination, b"payload").expect("write dest");

    if !security_xattrs_supported(&destination) {
        skip("backing filesystem rejects security.* writes (no SELinux/Smack policy?)");
        return;
    }

    let mut list = XattrList::new();
    list.push(XattrEntry::new(
        b"security.selinux".to_vec(),
        FIXTURE_LABEL.to_vec(),
    ));

    apply_xattrs_from_list(&destination, &list, false, None).expect("apply wire xattrs");

    let landed = xattr::get(&destination, "security.selinux")
        .expect("read dest label")
        .expect("dest must carry security.selinux after wire apply");

    assert_eq!(
        landed, FIXTURE_LABEL,
        "wire-driven xattr apply must preserve security.selinux bytes verbatim"
    );
}
