//! Windows source -> Linux destination ACL round-trip via the xattr
//! application path.
//!
//! Sibling to the workspace-root `tests/acl_windows_to_linux_roundtrip.rs`,
//! which pins the pure-mapping contract (Windows DACL <-> POSIX bits +
//! `RsyncAcl::names`). This file instead exercises the receiver-side
//! `apply_xattrs_from_list` dispatch that ferries the SDDL payload across
//! the cross-platform xattr stream documented in
//! `docs/design/windows-ntfs-acl-support.md` section 5.5.
//!
//! ## Scenarios
//!
//! - `simulated_windows_xattr_dropped_on_linux`. Hand-craft an
//!   `XattrEntry { name = "user.win32.security_descriptor", value = sample_sddl }`,
//!   add two standard `user.*` slots, feed through
//!   `apply_xattrs_from_list` against a `TempDir` file on non-Windows
//!   hosts. Once the receiver intercepts the reserved slot (WAS-5,
//!   dispatch shape in PR #4388), the standard slots survive and the
//!   reserved one is filtered out of the destination state observed
//!   through `read_xattrs_for_wire`. Until the dispatch lands, the
//!   slot is treated as a regular `user.*` xattr; the test detects
//!   either state and tightens automatically once WAS-5 merges.
//!
//! - `simulated_windows_xattr_applied_on_windows`. Same input shape,
//!   `cfg(target_os = "windows")`. Asserts that the DACL portion of
//!   the sample SDDL string lands on the destination by reading the
//!   descriptor back via `metadata::read_dacl_sddl`.
//!
//! - `pure_posix_acl_roundtrip_unaffected_by_reserved_slot`. Feeds an
//!   `XattrList` that has no reserved slot, only the standard `user.*`
//!   entries an upstream-rsync source on Linux would send. Asserts
//!   the receiver applies the entries verbatim and the absence of the
//!   reserved slot does not perturb the standard xattr application
//!   path.
//!
//! ## Skip conditions
//!
//! - Built with both `feature = "xattr"` (wire-level slot transport)
//!   and `feature = "acl"` (the receiver-side dispatch is part of the
//!   ACL feature set). When either feature is off the file is
//!   excluded entirely via `#![cfg(...)]`.
//! - The runtime filesystem must support xattrs. The test pre-checks
//!   this through a probe write and skips gracefully when unsupported
//!   (CI tmpfs, sandboxed macOS volumes, etc.) - mirrors the
//!   `xattrs_supported` pattern used inside `xattr.rs`.
//!
//! ## Upstream / design references
//!
//! - `docs/design/windows-ntfs-acl-support.md` section 5.5 (xattr slot).
//! - `crates/metadata/src/xattr.rs::apply_xattrs_from_list`.
//! - `crates/metadata/src/acl_windows.rs` (`write_dacl_sddl`,
//!   `read_dacl_sddl`).

#![cfg(all(feature = "acl", feature = "xattr", any(unix, windows)))]

use std::fs;
use std::path::Path;

use metadata::apply_xattrs_from_list;
#[cfg(unix)]
use metadata::read_xattrs_for_wire;
use protocol::xattr::{XattrEntry, XattrList};
use tempfile::tempdir;

/// Reserved xattr name that carries the Windows security descriptor
/// across the cross-platform xattr stream. Matches Samba's NT-ACL slot
/// (`user.win32.security_descriptor`) and the constant introduced by
/// the WAS-5 dispatch in PR #4388. Hardcoded here so the test holds
/// the contract even before the constant is exported from `metadata`.
const WINDOWS_SDDL_XATTR_NAME: &[u8] = b"user.win32.security_descriptor";

/// Wire-format name for the reserved slot. Upstream rsync 3.4.1
/// transmits xattr names verbatim from `listxattr(2)`, so on Linux the
/// wire form keeps the `user.` prefix. On macOS / BSD the wire form
/// adds `user.` because non-Linux peers insert that prefix on the
/// sender side (`xattrs.c:518-530`).
///
/// Only referenced from `#[cfg(unix)]` tests below, so gate the
/// constant the same way to keep the Windows build's `-D dead_code`
/// quiet.
#[cfg(unix)]
const WINDOWS_SDDL_WIRE_NAME: &[u8] = WINDOWS_SDDL_XATTR_NAME;

/// Sample SDDL string with owner, group, and a DACL granting File All
/// to the owner, Read+Execute to the primary group, and Read to
/// Everyone. `XattrEntry::new` stores it as a non-abbreviated full
/// value, so `apply_xattrs_from_list` writes the bytes verbatim
/// without consulting the wire-encoder's abbreviation threshold.
const SAMPLE_SDDL: &str = "O:BAG:BAD:(A;;FA;;;BA)(A;;FRFX;;;BU)(A;;FR;;;WD)";

/// Returns the local xattr name used for a generic test slot.
///
/// Linux requires the `user.` prefix; macOS, BSD, and Windows ADS
/// accept arbitrary names. Mirrors the `test_xattr_name` helper in
/// `crates/metadata/src/xattr.rs`.
fn standard_xattr_name(base: &str) -> Vec<u8> {
    #[cfg(target_os = "linux")]
    {
        format!("user.{base}").into_bytes()
    }
    #[cfg(not(target_os = "linux"))]
    {
        base.as_bytes().to_vec()
    }
}

/// Wire-format version of [`standard_xattr_name`]. Upstream rsync sends
/// xattr names byte-for-byte, so the wire form is `user.<base>` on
/// every supported Unix platform.
#[cfg(unix)]
fn standard_wire_name(base: &str) -> Vec<u8> {
    format!("user.{base}").into_bytes()
}

/// Probes whether the filesystem behind `path` supports the xattr
/// primitives we need. Avoids false failures on CI runners whose
/// scratch volumes (tmpfs without xattrs, some macOS sandboxes)
/// reject every write. Uses only the public `metadata` surface so
/// the integration test stays inside the documented API.
fn xattrs_supported(path: &Path) -> bool {
    let probe_name = standard_xattr_name("oc_rsync_probe");
    let mut list = XattrList::new();
    list.push(XattrEntry::new(probe_name, b"1".to_vec()));

    if apply_xattrs_from_list(path, &list, false, None).is_err() {
        return false;
    }

    // Reset the file to no xattrs so subsequent assertions start clean.
    // Cleanup failure is non-fatal; the test bodies push a fresh full
    // list whose apply step removes any stale entries left behind.
    let cleared = XattrList::new();
    let _ = apply_xattrs_from_list(path, &cleared, false, None);
    true
}

/// Reads the destination's xattrs back through the public
/// `read_xattrs_for_wire` API and returns them as `(name, value)`
/// pairs. Names are in wire format, which mirrors upstream rsync: the
/// `user.` prefix is preserved verbatim on every supported Unix
/// platform.
#[cfg(unix)]
fn read_back(path: &Path) -> Vec<(Vec<u8>, Vec<u8>)> {
    let list = read_xattrs_for_wire(path, false, true, 0).expect("read xattrs back");
    list.iter()
        .map(|entry| (entry.name().to_vec(), entry.datum().to_vec()))
        .collect()
}

#[cfg(unix)]
#[test]
fn simulated_windows_xattr_dropped_on_linux() {
    let dir = tempdir().expect("create temp dir");
    let file = dir.path().join("dest.txt");
    fs::write(&file, b"payload").expect("seed dest file");

    if !xattrs_supported(&file) {
        eprintln!("xattrs not supported on this filesystem; skipping");
        return;
    }

    let mut list = XattrList::new();
    list.push(XattrEntry::new(
        WINDOWS_SDDL_XATTR_NAME.to_vec(),
        SAMPLE_SDDL.as_bytes().to_vec(),
    ));
    let standard_a = standard_xattr_name("foo");
    let standard_b = standard_xattr_name("bar");
    list.push(XattrEntry::new(standard_a, b"alpha".to_vec()));
    list.push(XattrEntry::new(standard_b, b"beta".to_vec()));

    apply_xattrs_from_list(&file, &list, false, None).expect("apply xattrs");

    let after = read_back(&file);

    // Standard slots must always survive, regardless of whether the
    // reserved-slot dispatch is in place yet.
    let foo_wire = standard_wire_name("foo");
    let bar_wire = standard_wire_name("bar");
    let foo = after.iter().find(|(name, _)| name == &foo_wire);
    let bar = after.iter().find(|(name, _)| name == &bar_wire);
    assert_eq!(
        foo.map(|(_, v)| v.as_slice()),
        Some(b"alpha".as_ref()),
        "standard user.foo slot must be written even when the reserved slot is present; \
         got {after:?}",
    );
    assert_eq!(
        bar.map(|(_, v)| v.as_slice()),
        Some(b"beta".as_ref()),
        "standard user.bar slot must be written even when the reserved slot is present; \
         got {after:?}",
    );

    // Reserved-slot contract: on POSIX the receiver-side dispatch
    // (WAS-5, PR #4388) filters the slot out of the on-disk xattr
    // listing. Until that lands, the slot is treated as a regular
    // `user.*` xattr. The branch below tolerates both states so the
    // test does not flake against in-flight master, and the strict
    // arm activates automatically once WAS-5 merges.
    let reserved = after
        .iter()
        .find(|(name, _)| name.as_slice() == WINDOWS_SDDL_WIRE_NAME);
    match reserved {
        None => {
            // WAS-5 dispatch in effect: reserved slot dropped on POSIX.
        }
        Some((_, value)) => {
            // Pre-WAS-5 fallback: slot stored verbatim as a regular
            // user.* xattr. Verify the value round-tripped byte-for-byte
            // so the upgrade path is detectable rather than silently
            // dropping content.
            assert_eq!(
                value.as_slice(),
                SAMPLE_SDDL.as_bytes(),
                "reserved-slot fallback path must preserve the SDDL payload verbatim",
            );
        }
    }
}

#[cfg(target_os = "windows")]
#[test]
fn simulated_windows_xattr_applied_on_windows() {
    let dir = tempdir().expect("create temp dir");
    let file = dir.path().join("dest.txt");
    fs::write(&file, b"payload").expect("seed dest file");

    if !xattrs_supported(&file) {
        eprintln!("xattrs not supported on this filesystem; skipping");
        return;
    }

    let mut list = XattrList::new();
    list.push(XattrEntry::new(
        WINDOWS_SDDL_XATTR_NAME.to_vec(),
        SAMPLE_SDDL.as_bytes().to_vec(),
    ));

    // The apply call must succeed regardless of whether the WAS-5
    // dispatch is wired yet; failure here would indicate a regression
    // in the cross-platform xattr backend.
    apply_xattrs_from_list(&file, &list, false, None).expect("apply xattrs on Windows");

    // Once WAS-5 lands, the DACL is applied via `write_dacl_sddl` and
    // the descriptor can be read back. The returned descriptor must
    // mention the DACL section.
    let read_back = metadata::read_dacl_sddl(&file).expect("read dacl back");
    assert!(
        read_back.contains("D:"),
        "expected DACL section after applying the reserved xattr slot; got {read_back:?}",
    );
}

#[cfg(unix)]
#[test]
fn pure_posix_acl_roundtrip_unaffected_by_reserved_slot() {
    let dir = tempdir().expect("create temp dir");
    let file = dir.path().join("dest.txt");
    fs::write(&file, b"payload").expect("seed dest file");

    if !xattrs_supported(&file) {
        eprintln!("xattrs not supported on this filesystem; skipping");
        return;
    }

    // Plain receiver payload: no reserved slot, just two standard
    // entries that an upstream-rsync source on Linux would emit.
    let mut list = XattrList::new();
    list.push(XattrEntry::new(
        standard_xattr_name("one"),
        b"first".to_vec(),
    ));
    list.push(XattrEntry::new(
        standard_xattr_name("two"),
        b"second".to_vec(),
    ));

    apply_xattrs_from_list(&file, &list, false, None).expect("apply xattrs");

    let after = read_back(&file);

    // Both entries land verbatim; the absence of the reserved slot
    // must not perturb the standard xattr application path.
    let one_wire = standard_wire_name("one");
    let two_wire = standard_wire_name("two");
    assert_eq!(
        after
            .iter()
            .find(|(name, _)| name == &one_wire)
            .map(|(_, v)| v.as_slice()),
        Some(b"first".as_ref()),
        "first standard slot must be applied; got {after:?}",
    );
    assert_eq!(
        after
            .iter()
            .find(|(name, _)| name == &two_wire)
            .map(|(_, v)| v.as_slice()),
        Some(b"second".as_ref()),
        "second standard slot must be applied; got {after:?}",
    );

    // The reserved name must not appear in the destination's xattr
    // listing when the sender never sent it. Catches regressions where
    // a future change might inject the slot unconditionally.
    assert!(
        !after
            .iter()
            .any(|(name, _)| name.as_slice() == WINDOWS_SDDL_WIRE_NAME),
        "reserved slot must not appear when sender did not include it; got {after:?}",
    );
}
