//! ACL round-trip tests via oc-rsync local copy mode (`-aA`).
//!
//! Validates that oc-rsync preserves ACLs during local-to-local transfers.
//! This is the simplest round-trip: no network, no upstream rsync dependency.
//! The harness stamps known ACLs on source files, runs oc-rsync with `-aA`,
//! then reads back the ACLs on the destination and asserts equivalence.
//!
//! # Skip Conditions
//!
//! - `OC_RSYNC_ACL_ROUNDTRIP` env var is not set to `1`.
//! - Platform ACL tools not available.
//! - Filesystem does not support ACLs (common on tmpfs without mount option).
//! - Required user/group names do not exist on the system.
//!
//! # Upstream Reference
//!
//! - `acls.c:set_acl()` - receiver applies ACLs from cache to destination.
//! - `acls.c:get_rsync_acl()` - sender reads ACLs from source files.

mod integration;

use integration::acl_roundtrip::{AclEntry, AclTestFixture, FixtureFile, verify_acl_roundtrip};

/// Determine a user name that exists on this system for ACL entries.
///
/// On Linux, `nobody` is universally present. On macOS, `_www` or `nobody`
/// works. On Windows, `Everyone` or `Users` is always available.
fn test_user() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "nobody"
    }
    #[cfg(target_os = "macos")]
    {
        "_www"
    }
    #[cfg(target_os = "windows")]
    {
        "Everyone"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "nobody"
    }
}

/// Determine a group name that exists on this system for ACL entries.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn test_group() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "nogroup"
    }
    #[cfg(target_os = "macos")]
    {
        "staff"
    }
}

#[test]
fn acl_roundtrip_single_file_named_user() {
    let entries = vec![FixtureFile::file(
        "simple.txt",
        b"hello world",
        vec![AclEntry::user_rwx(test_user())],
    )];

    let Some(fixture) = AclTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_acl_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "ACL roundtrip failed for single file named user:\n{}",
        result.mismatch_report()
    );
}

#[test]
fn acl_roundtrip_multiple_files_mixed_perms() {
    let entries = vec![
        FixtureFile::file(
            "readable.txt",
            b"read only content",
            vec![AclEntry::user_r(test_user())],
        ),
        FixtureFile::file(
            "executable.bin",
            b"\x7fELF...",
            vec![AclEntry::user_rx(test_user())],
        ),
        FixtureFile::file(
            "full_access.dat",
            b"important data",
            vec![AclEntry::user_rwx(test_user())],
        ),
    ];

    let Some(fixture) = AclTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_acl_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "ACL roundtrip failed for mixed perms:\n{}",
        result.mismatch_report()
    );
}

#[test]
fn acl_roundtrip_nested_directory_with_default_acl() {
    // Default ACLs only meaningful on Linux/FreeBSD.
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        eprintln!("skip: default ACLs only supported on Linux/FreeBSD");
        return;
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let entries = vec![
            FixtureFile::dir(
                "subdir",
                vec![
                    AclEntry::user_rwx(test_user()),
                    AclEntry::default_user_rwx(test_user()),
                ],
            ),
            FixtureFile::file(
                "subdir/inner.txt",
                b"nested content",
                vec![AclEntry::user_rx(test_user())],
            ),
        ];

        let Some(fixture) = AclTestFixture::try_build(entries.clone()) else {
            return;
        };

        fixture.transfer().expect("transfer should succeed");

        let result = verify_acl_roundtrip(&fixture.src, &fixture.dst, &entries);
        assert!(
            result.all_match(),
            "ACL roundtrip failed for directory with default ACL:\n{}",
            result.mismatch_report()
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn acl_roundtrip_group_entry() {
    let entries = vec![FixtureFile::file(
        "group_test.txt",
        b"group acl content",
        vec![AclEntry::group_rx(test_group())],
    )];

    let Some(fixture) = AclTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_acl_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "ACL roundtrip failed for group entry:\n{}",
        result.mismatch_report()
    );
}

#[test]
fn acl_roundtrip_multiple_named_entries_on_same_file() {
    // Multiple named ACL entries on a single file - tests that the wire
    // protocol preserves the full ida_entries list.
    let user = test_user();

    let mut acl_entries = vec![AclEntry::user_rwx(user)];

    // On Linux, we can add a group entry too.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        acl_entries.push(AclEntry::group_rx(test_group()));
    }

    let entries = vec![FixtureFile::file(
        "multi_acl.txt",
        b"multi entry content",
        acl_entries,
    )];

    let Some(fixture) = AclTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_acl_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "ACL roundtrip failed for multiple named entries:\n{}",
        result.mismatch_report()
    );
}

#[test]
fn acl_roundtrip_deep_tree() {
    // Validates ACL preservation across a deeper directory hierarchy.
    let user = test_user();

    let entries = vec![
        FixtureFile::dir("a", vec![AclEntry::user_rwx(user)]),
        FixtureFile::dir("a/b", vec![AclEntry::user_rx(user)]),
        FixtureFile::file("a/top.txt", b"top level", vec![AclEntry::user_r(user)]),
        FixtureFile::file("a/b/deep.txt", b"deep file", vec![AclEntry::user_rwx(user)]),
    ];

    let Some(fixture) = AclTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_acl_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "ACL roundtrip failed for deep tree:\n{}",
        result.mismatch_report()
    );
}
