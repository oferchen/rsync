//! Xattr round-trip tests via oc-rsync local copy mode (`-aX`).
//!
//! Validates that oc-rsync preserves extended attributes during local-to-local
//! transfers. This is the simplest round-trip: no network, no upstream rsync
//! dependency. The harness stamps known xattrs on source files, runs oc-rsync
//! with `-aX`, then reads back the xattrs on the destination and asserts
//! equivalence.
//!
//! # Skip Conditions
//!
//! - `OC_RSYNC_XATTR_ROUNDTRIP` env var is not set to `1`.
//! - Filesystem does not support xattrs.
//!
//! # Upstream Reference
//!
//! - `xattrs.c:set_xattr()` - receiver applies xattrs from cache to destination.
//! - `xattrs.c:rsync_xal_get()` - sender reads xattrs from source files.

#[cfg(unix)]
mod integration;

#[cfg(unix)]
use integration::xattr_roundtrip::{
    FixtureFile, XattrEntry, XattrTestFixture, is_root, verify_xattr_roundtrip,
};

/// Single file with a single user-namespace xattr.
#[cfg(unix)]
#[test]
fn xattr_roundtrip_single_file_single_attr() {
    let entries = vec![FixtureFile::file(
        "simple.txt",
        b"hello world",
        vec![XattrEntry::user("test_attr", b"test_value")],
    )];

    let Some(fixture) = XattrTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_xattr_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "xattr roundtrip failed for single file single attr:\n{}",
        result.mismatch_report()
    );
}

/// Multiple files with different xattrs.
#[cfg(unix)]
#[test]
fn xattr_roundtrip_multiple_files_different_attrs() {
    let entries = vec![
        FixtureFile::file(
            "alpha.txt",
            b"alpha content",
            vec![XattrEntry::user("color", b"red")],
        ),
        FixtureFile::file(
            "beta.txt",
            b"beta content",
            vec![XattrEntry::user("color", b"blue")],
        ),
        FixtureFile::file(
            "gamma.txt",
            b"gamma content",
            vec![XattrEntry::user("priority", b"high")],
        ),
    ];

    let Some(fixture) = XattrTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_xattr_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "xattr roundtrip failed for multiple files:\n{}",
        result.mismatch_report()
    );
}

/// Single file with multiple xattr entries.
#[cfg(unix)]
#[test]
fn xattr_roundtrip_single_file_multiple_attrs() {
    let entries = vec![FixtureFile::file(
        "multi.dat",
        b"data with many attributes",
        vec![
            XattrEntry::user("author", b"test-harness"),
            XattrEntry::user("version", b"1"),
            XattrEntry::user("checksum", b"abc123def456"),
            XattrEntry::user("mime_type", b"application/octet-stream"),
        ],
    )];

    let Some(fixture) = XattrTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_xattr_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "xattr roundtrip failed for single file multiple attrs:\n{}",
        result.mismatch_report()
    );
}

/// Deep directory tree with xattrs at every level.
#[cfg(unix)]
#[test]
fn xattr_roundtrip_deep_tree() {
    let entries = vec![
        FixtureFile::dir("a", vec![XattrEntry::user("level", b"1")]),
        FixtureFile::dir("a/b", vec![XattrEntry::user("level", b"2")]),
        FixtureFile::dir("a/b/c", vec![XattrEntry::user("level", b"3")]),
        FixtureFile::file(
            "a/top.txt",
            b"top level file",
            vec![XattrEntry::user("pos", b"top")],
        ),
        FixtureFile::file(
            "a/b/mid.txt",
            b"mid level file",
            vec![XattrEntry::user("pos", b"mid")],
        ),
        FixtureFile::file(
            "a/b/c/deep.txt",
            b"deep level file",
            vec![
                XattrEntry::user("pos", b"deep"),
                XattrEntry::user("depth", b"3"),
            ],
        ),
    ];

    let Some(fixture) = XattrTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_xattr_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "xattr roundtrip failed for deep tree:\n{}",
        result.mismatch_report()
    );
}

/// Binary xattr values - ensures non-UTF-8 data survives the round-trip.
#[cfg(unix)]
#[test]
fn xattr_roundtrip_binary_values() {
    let binary_val: Vec<u8> = (0..=255).collect();
    let entries = vec![FixtureFile::file(
        "binary.bin",
        b"file with binary xattr value",
        vec![
            XattrEntry::user("binary_data", &binary_val),
            XattrEntry::user("null_bytes", b"\x00\x00\x01\x02"),
        ],
    )];

    let Some(fixture) = XattrTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_xattr_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "xattr roundtrip failed for binary values:\n{}",
        result.mismatch_report()
    );
}

/// Directory with xattrs - validates that directory xattrs are preserved.
#[cfg(unix)]
#[test]
fn xattr_roundtrip_directory_xattr() {
    let entries = vec![
        FixtureFile::dir(
            "tagged_dir",
            vec![
                XattrEntry::user("description", b"a tagged directory"),
                XattrEntry::user("category", b"test"),
            ],
        ),
        FixtureFile::file(
            "tagged_dir/child.txt",
            b"child of tagged dir",
            vec![XattrEntry::user("parent", b"tagged_dir")],
        ),
    ];

    let Some(fixture) = XattrTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_xattr_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "xattr roundtrip failed for directory xattr:\n{}",
        result.mismatch_report()
    );
}

/// Linux `security.*` namespace xattrs - requires root.
///
/// The `security.*` namespace is only writable by root. This test skips
/// cleanly on non-root runners. On CI with elevated permissions, it validates
/// that oc-rsync preserves security namespace attributes.
#[cfg(target_os = "linux")]
#[test]
fn xattr_roundtrip_security_namespace_linux() {
    if !is_root() {
        eprintln!("skip: security.* namespace xattrs require root");
        return;
    }

    let entries = vec![FixtureFile::file(
        "secure.dat",
        b"security-tagged content",
        vec![
            XattrEntry::security("test_label", b"confidential"),
            XattrEntry::user("public_tag", b"visible"),
        ],
    )];

    let Some(fixture) = XattrTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_xattr_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "xattr roundtrip failed for security namespace:\n{}",
        result.mismatch_report()
    );
}

/// macOS-style extended attributes with reverse-DNS naming.
///
/// Validates that macOS convention attribute names (e.g.,
/// `com.example.metadata`) survive the round-trip. On Linux these are
/// stamped as `user.com.example.metadata` since the `user.*` namespace
/// prefix is required.
#[cfg(target_os = "macos")]
#[test]
fn xattr_roundtrip_macos_reverse_dns() {
    let entries = vec![
        FixtureFile::file(
            "tagged.txt",
            b"macos tagged content",
            vec![
                XattrEntry::macos("com.example.metadata", b"example-value"),
                XattrEntry::macos("com.example.version", b"2"),
            ],
        ),
        FixtureFile::file(
            "quarantine.txt",
            b"quarantined file content",
            // com.apple.quarantine has a specific format: AAAA;BBBBBBBB;CCCC;DDDD
            // Use a realistic-looking value.
            vec![XattrEntry::macos(
                "com.apple.quarantine",
                b"0083;66a7b89e;Safari;F1234567-89AB-CDEF-0123-456789ABCDEF",
            )],
        ),
    ];

    let Some(fixture) = XattrTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_xattr_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "xattr roundtrip failed for macOS reverse-DNS xattrs:\n{}",
        result.mismatch_report()
    );
}

/// Empty xattr value - validates that zero-length values survive the
/// round-trip. Some implementations treat missing vs empty differently.
#[cfg(unix)]
#[test]
fn xattr_roundtrip_empty_value() {
    let entries = vec![FixtureFile::file(
        "empty_val.txt",
        b"file with empty xattr value",
        vec![XattrEntry::user("marker", b"")],
    )];

    let Some(fixture) = XattrTestFixture::try_build(entries.clone()) else {
        return;
    };

    fixture.transfer().expect("transfer should succeed");

    let result = verify_xattr_roundtrip(&fixture.src, &fixture.dst, &entries);
    assert!(
        result.all_match(),
        "xattr roundtrip failed for empty value:\n{}",
        result.mismatch_report()
    );
}
