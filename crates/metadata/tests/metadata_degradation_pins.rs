//! Behaviour pins for metadata paths that degrade on platforms or privilege
//! levels where the underlying attribute cannot be applied. Each test asserts
//! the exact observable outcome - documented no-op, applied value, or silent
//! upstream-faithful skip - so a regression to silent data loss fails loudly.
//!
//! Scope split from the existing coverage to avoid duplication:
//!
//! - crtime: `atimes_crtimes_roundtrip.rs` already pins the Linux no-op and the
//!   macOS applied path through the *source-metadata* apply
//!   (`apply_file_metadata_with_options`). This file pins the *entry-based*
//!   apply (`apply_metadata_with_attrs_flags`, the wire-receiver path) and adds
//!   the Windows applied arm, which the round-trip file documents as out of
//!   scope ("macOS + Linux only").
//! - non-root xattr namespace: `security_selinux_roundtrip.rs` pins the *root*
//!   `security.*` round-trip and skips when unprivileged. This file pins the
//!   *non-root* receiver skip - the mechanism by which a resource fork or any
//!   non-`user.*` attribute is dropped.
//!
//! Companion to `nfsv4_acl_unsupported_pin.rs` (NFSv4-ACL surfaced-error /
//! silent-absence).

// The entry-based crtime apply and the non-root xattr-namespace pins below run
// only on non-macOS targets: macOS crtime is covered by
// `atimes_crtimes_roundtrip.rs`, so this file has no macOS test using these
// items. Gate the imports and the shared constant to match, so a macOS build
// under `-D warnings` does not fail on unused imports / dead code.
#[cfg(not(target_os = "macos"))]
use metadata::{AttrsFlags, MetadataOptions, apply_metadata_with_attrs_flags};
#[cfg(not(target_os = "macos"))]
use protocol::flist::FileEntry;

/// 2000-01-01 UTC: a fixed historical creation time that the freshly created
/// destination cannot already carry, so an applied-vs-no-op regression cannot
/// pass by coincidence.
#[cfg(not(target_os = "macos"))]
const HISTORICAL_CRTIME_SECS: i64 = 946_684_800;

/// #198 - the entry-based crtime apply is a documented no-op on non-macOS,
/// non-Windows Unix.
///
/// `set_crtime()` is a stub returning `Ok(())` on Linux (and other non-macOS,
/// non-Windows targets) because birth time is not settable there. The
/// wire-receiver path `apply_metadata_with_attrs_flags` must therefore run
/// cleanly under `--crtimes` and leave the file untouched. A regression that
/// started erroring would silently break every `--crtimes` transfer on Linux;
/// this complements the source-metadata no-op pin in `atimes_crtimes_roundtrip`.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn crtimes_entry_apply_is_a_documented_noop_on_non_macos_unix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dst = dir.path().join("f");
    std::fs::write(&dst, b"payload").expect("write dst");

    let mut entry = FileEntry::new_file("f".into(), 7, 0o644);
    entry.set_crtime(HISTORICAL_CRTIME_SECS);
    let options = MetadataOptions::new().preserve_crtimes(true);

    apply_metadata_with_attrs_flags(&dst, &entry, &options, None, AttrsFlags::empty())
        .expect("entry crtime apply must be a clean no-op on non-macOS unix");

    assert_eq!(
        std::fs::read(&dst).expect("read dst"),
        b"payload",
        "a no-op crtime apply must not disturb file content",
    );
}

/// #198 - the entry-based crtime apply actually stamps the creation time on
/// Windows via `SetFileTime`.
///
/// On Windows `set_crtime()` writes the NTFS creation time, so an entry that
/// carries a creation time with `--crtimes` active must move the destination's
/// creation time to that value. A regression to a silent no-op (creation-time
/// data loss) is caught here: the destination crtime would stay at "now"
/// instead of the historical value. This fills the Windows gap that
/// `atimes_crtimes_roundtrip` documents as out of scope.
#[cfg(windows)]
#[test]
fn crtimes_entry_apply_sets_creation_time_on_windows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dst = dir.path().join("f");
    std::fs::write(&dst, b"payload").expect("write dst");

    let mut entry = FileEntry::new_file("f".into(), 7, 0o644);
    entry.set_crtime(HISTORICAL_CRTIME_SECS);
    let options = MetadataOptions::new().preserve_crtimes(true);

    apply_metadata_with_attrs_flags(&dst, &entry, &options, None, AttrsFlags::empty())
        .expect("entry crtime apply must set the creation time on windows");

    let created = std::fs::metadata(&dst)
        .expect("stat dst")
        .created()
        .expect("windows exposes a creation time");
    let secs = created
        .duration_since(std::time::UNIX_EPOCH)
        .expect("crtime is after the unix epoch")
        .as_secs() as i64;
    assert_eq!(
        secs, HISTORICAL_CRTIME_SECS,
        "windows --crtimes must stamp the source creation time, not silently no-op",
    );
}

/// #197 + #199 - a non-root receiver silently skips a non-`user.*` xattr and
/// preserves a `user.*` one, continuing without error.
///
/// There is no separate resource-fork subsystem: a macOS resource fork travels
/// as the ordinary `com.apple.ResourceFork` xattr, so its fate on a
/// non-supporting receiver is governed entirely by the generic xattr namespace
/// rule. Upstream `xattrs.c:830` restricts a non-root receiver's stored xattrs
/// to the `user.*` namespace: a non-`user.*` attribute is silently skipped,
/// while a `user.*` attribute is applied. This pins both halves so a regression
/// cannot (a) hard-error on the skipped attribute, nor (b) drop the preserved
/// one - either would be a silent behaviour change from upstream's contract.
#[cfg(all(target_os = "linux", feature = "xattr"))]
#[test]
fn non_root_receiver_skips_non_user_namespace_xattr_and_keeps_user_one() {
    use metadata::apply_xattrs_from_list;
    use protocol::xattr::{XattrEntry, XattrList};

    // A root receiver keeps every namespace (upstream `user_only == 0`), so the
    // skip this test pins is only observable for a non-root receiver.
    if rustix::process::geteuid().is_root() {
        eprintln!(
            "[skip] non_root_receiver_skips_non_user_namespace_xattr: \
             requires a non-root receiver (root keeps all namespaces)"
        );
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("f");
    std::fs::write(&file, b"x").expect("write file");

    // Pre-check: the backing filesystem must support the `user.*` namespace, or
    // the pin cannot be exercised. Degrade with a reason - never a silent pass.
    if xattr::set(&file, "user.pin.precheck", b"1").is_err() {
        eprintln!(
            "[skip] non_root_receiver_skips_non_user_namespace_xattr: \
             backing filesystem does not support user.* xattrs"
        );
        return;
    }
    xattr::remove(&file, "user.pin.precheck").expect("remove precheck xattr");

    let list = XattrList::with_entries(vec![
        XattrEntry::new(b"user.pin.kept".to_vec(), b"v".to_vec()),
        // A non-`user.*` stand-in for a resource fork / system attribute that a
        // non-root receiver is not permitted to store (upstream `xattrs.c:830`).
        XattrEntry::new(b"security.pin.dropped".to_vec(), b"v".to_vec()),
    ]);

    apply_xattrs_from_list(&file, &list, true, None, None, None)
        .expect("a non-user xattr must be skipped-and-continue, never a hard error");

    let names: Vec<String> = xattr::list(&file)
        .expect("list xattrs")
        .filter_map(|name| name.to_str().map(str::to_string))
        .collect();
    assert!(
        names.iter().any(|name| name == "user.pin.kept"),
        "a user.* xattr must be preserved for a non-root receiver, not dropped",
    );
    assert!(
        !names.iter().any(|name| name == "security.pin.dropped"),
        "a non-user.* xattr must be silently skipped for a non-root receiver (xattrs.c:830)",
    );
}
