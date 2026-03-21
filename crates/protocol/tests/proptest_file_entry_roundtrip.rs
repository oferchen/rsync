//! Property-based roundtrip tests for FileEntry wire format encode/decode.
//!
//! Verifies that arbitrary FileEntry values survive a write-then-read cycle
//! across all supported protocol versions (28-32) and file types (regular,
//! directory, symlink, block/char device, FIFO, socket).
//!
//! The roundtrip exercises `FileListWriter::write_entry` followed by
//! `FileListReader::read_entry`, ensuring the decoded entry matches the
//! original on every field that the wire format preserves.

use proptest::prelude::*;
use protocol::flist::{FileEntry, FileListReader, FileListWriter};
use protocol::{CompatibilityFlags, ProtocolVersion};
use std::io::Cursor;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Generates a valid ASCII filename component (1-64 bytes, no slashes or NUL).
///
/// Excludes `"."` and `".."` which are special path components that
/// `clean_and_validate_name` normalizes (removes `.`) or rejects (`..`),
/// causing roundtrip mismatches.
fn filename_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_.][a-zA-Z0-9_.\\-]{0,63}".prop_filter("exclude . and .. path components", |s| {
        s != "." && s != ".."
    })
}

/// Generates a single-component filename as a PathBuf.
///
/// The wire format splits paths into dirname + basename. After a roundtrip
/// through `write_entry`/`read_entry`, `name()` returns only the basename.
/// Using single-component names avoids false mismatches from this split.
fn basename_strategy() -> impl Strategy<Value = PathBuf> {
    filename_strategy().prop_map(PathBuf::from)
}

/// Generates a symlink target path.
fn symlink_target_strategy() -> impl Strategy<Value = PathBuf> {
    prop::collection::vec(filename_strategy(), 1..=3)
        .prop_map(|parts| PathBuf::from(parts.join("/")))
}

/// Generates Unix permission bits (0o000 - 0o777).
fn permissions_strategy() -> impl Strategy<Value = u32> {
    0u32..=0o777
}

/// Generates a file size that fits in the wire format.
/// Protocol 30+ uses varlong (up to i64::MAX), older uses 4-byte int for
/// small files. We keep values in a reasonable range to avoid overflow.
fn file_size_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        Just(0u64),
        1u64..=4096,
        4097u64..=1_000_000,
        1_000_001u64..=1_000_000_000,
    ]
}

/// Generates large file sizes that exercise varlong encoding (protocol 30+).
fn large_file_size_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        // Just above i32::MAX - tests varlong boundary
        (i32::MAX as u64)..=(i32::MAX as u64 + 1000),
        // Multi-GB range
        1_000_000_000u64..=10_000_000_000,
        // TB range
        1_000_000_000_000u64..=2_000_000_000_000,
    ]
}

/// Generates a modification time (seconds since epoch).
/// Upstream rsync encodes mtime as a signed 32-bit delta from the previous
/// entry's mtime. We stay within i32 range to be safe across all protocols.
fn mtime_strategy() -> impl Strategy<Value = i64> {
    0i64..=i32::MAX as i64
}

/// Generates a UID/GID value.
fn id_strategy() -> impl Strategy<Value = u32> {
    prop_oneof![Just(0u32), 1u32..=1000, 1001u32..=65535,]
}

/// Generates a non-zero UID/GID value.
///
/// Used in name roundtrip tests because XMIT_USER_NAME_FOLLOWS and
/// XMIT_GROUP_NAME_FOLLOWS are only set when the UID/GID differs from
/// the previous entry. The initial prev_uid/prev_gid is 0, so a
/// zero-valued ID triggers XMIT_SAME_UID/XMIT_SAME_GID and suppresses
/// the name - which is correct wire behavior but not what these tests verify.
fn nonzero_id_strategy() -> impl Strategy<Value = u32> {
    prop_oneof![1u32..=1000, 1001u32..=65535,]
}

/// Generates a device major/minor number.
fn rdev_strategy() -> impl Strategy<Value = (u32, u32)> {
    (0u32..=255, 0u32..=255)
}

/// Supported protocol versions.
fn protocol_version_strategy() -> impl Strategy<Value = u8> {
    prop::sample::select(vec![28u8, 29, 30, 31, 32])
}

/// Protocol versions that support varlong sizes (30+).
fn modern_protocol_version_strategy() -> impl Strategy<Value = u8> {
    prop::sample::select(vec![30u8, 31, 32])
}

/// Generates a valid checksum of the given length.
fn checksum_strategy(len: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), len..=len)
}

/// Generates a short ASCII owner/group name (1-16 chars).
fn owner_name_strategy() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,15}"
}

/// Generates nanoseconds for mtime (0-999_999_999).
fn nsec_strategy() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(0u32),
        1u32..=999,
        1_000u32..=999_999,
        1_000_000u32..=999_999_999,
    ]
}

// ---------------------------------------------------------------------------
// Roundtrip helpers
// ---------------------------------------------------------------------------

/// Writes an entry, appends the end marker, then reads it back.
/// Writer and reader must have matching preserve flags.
fn roundtrip(
    writer: &mut FileListWriter,
    reader: &mut FileListReader,
    entry: &FileEntry,
) -> FileEntry {
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    reader
        .read_entry(&mut cursor)
        .unwrap()
        .expect("expected an entry, got end-of-list")
}

/// Writes multiple entries, appends the end marker, then reads them all back.
fn roundtrip_sequence(
    writer: &mut FileListWriter,
    reader: &mut FileListReader,
    entries: &[FileEntry],
) -> Vec<FileEntry> {
    let mut buf = Vec::new();
    for entry in entries {
        writer.write_entry(&mut buf, entry).unwrap();
    }
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut decoded = Vec::new();
    while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
        decoded.push(entry);
    }
    decoded
}

/// Asserts core fields match between original and decoded entries.
fn assert_core_fields_match(original: &FileEntry, decoded: &FileEntry) {
    assert_eq!(decoded.name(), original.name(), "name mismatch");
    assert_eq!(decoded.size(), original.size(), "size mismatch");
    assert_eq!(decoded.mode(), original.mode(), "mode mismatch");
    assert_eq!(decoded.mtime(), original.mtime(), "mtime mismatch");
}

// ---------------------------------------------------------------------------
// Regular file roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Regular files roundtrip through all protocol versions.
    #[test]
    fn regular_file_roundtrip(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::new(protocol);
        let mut reader = FileListReader::new(protocol);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
    }

    /// Regular files with UID/GID preservation roundtrip correctly.
    #[test]
    fn regular_file_with_ownership_roundtrip(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        uid in id_strategy(),
        gid in id_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_uid(uid);
        entry.set_gid(gid);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.uid(), Some(uid), "uid mismatch");
        assert_eq!(decoded.gid(), Some(gid), "gid mismatch");
    }
}

// ---------------------------------------------------------------------------
// Directory roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Directories roundtrip through all protocol versions.
    #[test]
    fn directory_roundtrip(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_directory(name, perms);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::new(protocol);
        let mut reader = FileListReader::new(protocol);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert!(decoded.is_dir(), "decoded should be a directory");
    }
}

// ---------------------------------------------------------------------------
// Symlink roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Symlinks roundtrip with target when preserve_links is enabled.
    #[test]
    fn symlink_roundtrip(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        target in symlink_target_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let entry = FileEntry::new_symlink(name, target.clone());

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_links(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_links(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_eq!(decoded.name(), entry.name(), "symlink name mismatch");
        assert!(decoded.is_symlink(), "decoded should be a symlink");
        assert_eq!(
            decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some(target.to_string_lossy().into_owned()),
            "symlink target mismatch"
        );
    }
}

// ---------------------------------------------------------------------------
// Device file roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Block devices roundtrip with rdev when preserve_devices is enabled.
    #[test]
    fn block_device_roundtrip(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        perms in permissions_strategy(),
        (major, minor) in rdev_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_block_device(name, perms, major, minor);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_devices(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.rdev_major(), Some(major), "rdev major mismatch");
        assert_eq!(decoded.rdev_minor(), Some(minor), "rdev minor mismatch");
    }

    /// Character devices roundtrip with rdev when preserve_devices is enabled.
    #[test]
    fn char_device_roundtrip(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        perms in permissions_strategy(),
        (major, minor) in rdev_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_char_device(name, perms, major, minor);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_devices(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.rdev_major(), Some(major), "rdev major mismatch");
        assert_eq!(decoded.rdev_minor(), Some(minor), "rdev minor mismatch");
    }
}

// ---------------------------------------------------------------------------
// Special file roundtrips (FIFO, socket)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// FIFOs roundtrip when preserve_specials is enabled.
    #[test]
    fn fifo_roundtrip(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_fifo(name, perms);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_specials(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_specials(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
    }

    /// Sockets roundtrip when preserve_specials is enabled.
    #[test]
    fn socket_roundtrip(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_socket(name, perms);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_specials(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_specials(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
    }
}

// ---------------------------------------------------------------------------
// Multi-entry sequence roundtrips (cross-entry compression)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// A sequence of regular files in the same directory roundtrips correctly.
    ///
    /// This exercises the cross-entry delta compression that the wire format
    /// uses (XMIT_SAME_NAME, XMIT_SAME_MODE, XMIT_SAME_TIME, etc.).
    #[test]
    fn multi_file_sequence_roundtrip(
        proto in protocol_version_strategy(),
        dir in filename_strategy(),
        count in 2usize..=6,
        base_perms in permissions_strategy(),
        base_mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();

        // Build entries with single-component names to avoid dirname/basename
        // split issues on roundtrip. The cross-entry delta compression is still
        // exercised because entries share mode, mtime, and similar name prefixes.
        let entries: Vec<FileEntry> = (0..count)
            .map(|i| {
                let path = PathBuf::from(format!("{dir}_file{i}.dat"));
                let size = (i as u64 + 1) * 100;
                let mut e = FileEntry::new_file(path, size, base_perms);
                e.set_mtime(base_mtime, 0);
                e
            })
            .collect();

        let mut writer = FileListWriter::new(protocol);
        let mut reader = FileListReader::new(protocol);
        let decoded = roundtrip_sequence(&mut writer, &mut reader, &entries);

        prop_assert_eq!(decoded.len(), entries.len(), "entry count mismatch");
        for (orig, dec) in entries.iter().zip(decoded.iter()) {
            assert_core_fields_match(orig, dec);
        }
    }

    /// A mixed-type sequence roundtrips correctly.
    ///
    /// Exercises type transitions (file -> dir -> symlink -> file) which
    /// affect how mode and flags differ from the previous entry.
    #[test]
    fn mixed_type_sequence_roundtrip(
        proto in protocol_version_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();

        let mut file_entry = FileEntry::new_file("alpha.txt".into(), 500, perms);
        file_entry.set_mtime(mtime, 0);
        let mut dir_entry = FileEntry::new_directory("beta".into(), perms);
        dir_entry.set_mtime(mtime, 0);
        let symlink_entry = FileEntry::new_symlink("gamma".into(), "alpha.txt".into());
        let mut file2_entry = FileEntry::new_file("delta.bin".into(), 1000, perms);
        file2_entry.set_mtime(mtime, 0);

        let entries = vec![file_entry, dir_entry, symlink_entry, file2_entry];

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_links(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_links(true);
        let decoded = roundtrip_sequence(&mut writer, &mut reader, &entries);

        prop_assert_eq!(decoded.len(), entries.len(), "entry count mismatch");
        for (orig, dec) in entries.iter().zip(decoded.iter()) {
            assert_eq!(dec.name(), orig.name(), "name mismatch");
            assert_eq!(dec.mode(), orig.mode(), "mode mismatch");
            if orig.is_symlink() {
                prop_assert_eq!(
                    dec.link_target().map(|p| p.to_string_lossy().into_owned()),
                    orig.link_target().map(|p| p.to_string_lossy().into_owned()),
                    "symlink target mismatch"
                );
            } else {
                assert_eq!(dec.size(), orig.size(), "size mismatch");
                assert_eq!(dec.mtime(), orig.mtime(), "mtime mismatch");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Protocol-version-specific encoding roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Varint flist flags (protocol 32 with compat flag) roundtrip correctly.
    ///
    /// Protocol 32 with VARINT_FLIST_FLAGS encodes the flags byte as a varint
    /// instead of a fixed 1-2 byte sequence.
    #[test]
    fn varint_flist_flags_roundtrip(
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let compat = CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::with_compat_flags(protocol, compat);
        let mut reader = FileListReader::with_compat_flags(protocol, compat);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
    }

    /// Protocol 28-29 use fixed-width encoding for sizes and lengths.
    /// Protocol 30+ use varint encoding. Both must roundtrip.
    #[test]
    fn protocol_encoding_format_roundtrip(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        uid in id_strategy(),
        gid in id_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_uid(uid);
        entry.set_gid(gid);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.uid(), Some(uid), "uid mismatch");
        assert_eq!(decoded.gid(), Some(gid), "gid mismatch");
    }
}

// ---------------------------------------------------------------------------
// Checksum roundtrips (--checksum mode)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Regular files with checksums roundtrip when always_checksum is enabled.
    ///
    /// The checksum is a fixed-length byte sequence appended after all other
    /// metadata. Length depends on the protocol's checksum algorithm.
    #[test]
    fn checksum_roundtrip(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        checksum in checksum_strategy(16),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let csum_len = 16;
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_checksum(checksum.clone());

        let mut writer = FileListWriter::new(protocol)
            .with_always_checksum(csum_len);
        let mut reader = FileListReader::new(protocol)
            .with_always_checksum(csum_len);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(
            decoded.checksum(),
            Some(checksum.as_slice()),
            "checksum mismatch"
        );
    }

    /// Directories do not carry checksums in protocol >= 28.
    /// The reader should not produce a checksum for non-regular files.
    #[test]
    fn directory_no_checksum_proto28_plus(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let csum_len = 16;
        let mut entry = FileEntry::new_directory(name, perms);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::new(protocol)
            .with_always_checksum(csum_len);
        let mut reader = FileListReader::new(protocol)
            .with_always_checksum(csum_len);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_eq!(decoded.name(), entry.name(), "name mismatch");
        assert!(decoded.is_dir(), "decoded should be a directory");
        assert_eq!(decoded.checksum(), None, "dirs should have no checksum");
    }
}

// ---------------------------------------------------------------------------
// Mtime nanoseconds roundtrip (protocol 31+)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Mtime nanoseconds are preserved on protocol 31+ when non-zero.
    ///
    /// The XMIT_MOD_NSEC flag signals that a varint nsec value follows
    /// the mtime on the wire. Protocol < 31 does not support nsec.
    #[test]
    fn mtime_nsec_roundtrip(
        proto in prop::sample::select(vec![31u8, 32]),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        nsec in nsec_strategy().prop_filter("non-zero nsec", |&n| n > 0),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, nsec);

        let mut writer = FileListWriter::new(protocol);
        let mut reader = FileListReader::new(protocol);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.mtime_nsec(), nsec, "mtime nsec mismatch");
    }

    /// Zero nsec is not transmitted (no XMIT_MOD_NSEC flag set).
    #[test]
    fn zero_nsec_not_transmitted(
        proto in prop::sample::select(vec![31u8, 32]),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::new(protocol);
        let mut reader = FileListReader::new(protocol);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.mtime_nsec(), 0, "zero nsec should remain zero");
    }
}

// ---------------------------------------------------------------------------
// Access time roundtrips (--atimes)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Access times roundtrip for regular files when preserve_atimes is enabled.
    ///
    /// Atime is only preserved for non-directory entries. Uses varlong(4) encoding.
    #[test]
    fn atime_roundtrip(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        atime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_atime(atime);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_atimes(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_atimes(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.atime(), atime, "atime mismatch");
    }

    /// Atime nanoseconds are preserved on protocol 32 when preserve_atimes is enabled.
    #[test]
    fn atime_nsec_roundtrip_proto32(
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        atime in mtime_strategy(),
        atime_nsec in nsec_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_atime(atime);
        entry.set_atime_nsec(atime_nsec);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_atimes(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_atimes(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.atime(), atime, "atime mismatch");
        assert_eq!(decoded.atime_nsec(), atime_nsec, "atime nsec mismatch");
    }

    /// Directories do not carry atime even when preserve_atimes is enabled.
    #[test]
    fn directory_no_atime(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_directory(name, perms);
        entry.set_mtime(mtime, 0);
        // Set atime on the entry - it should be ignored for directories
        entry.set_atime(12345);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_atimes(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_atimes(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_eq!(decoded.name(), entry.name(), "name mismatch");
        assert!(decoded.is_dir(), "decoded should be a directory");
        // Atime is not preserved for directories
        assert_eq!(decoded.atime(), 0, "dirs should have no atime");
    }
}

// ---------------------------------------------------------------------------
// Creation time roundtrips (--crtimes)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Creation times roundtrip when preserve_crtimes is enabled.
    ///
    /// When crtime equals mtime, the XMIT_CRTIME_EQ_MTIME flag is set
    /// and crtime is not written separately.
    #[test]
    fn crtime_roundtrip(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        crtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_crtime(crtime);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_crtimes(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_crtimes(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.crtime(), crtime, "crtime mismatch");
    }

    /// When crtime equals mtime, XMIT_CRTIME_EQ_MTIME optimization applies.
    /// The decoder should reconstruct crtime from mtime.
    #[test]
    fn crtime_eq_mtime_optimization(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_crtime(mtime); // crtime == mtime

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_crtimes(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_crtimes(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.crtime(), mtime, "crtime should equal mtime");
    }
}

// ---------------------------------------------------------------------------
// User/group name roundtrips (protocol 30+)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// User and group names roundtrip on protocol 30+ when UID/GID are preserved.
    ///
    /// Names are only sent when the UID/GID differs from the previous entry
    /// (XMIT_USER_NAME_FOLLOWS / XMIT_GROUP_NAME_FOLLOWS flags).
    #[test]
    fn user_group_name_roundtrip(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        uid in nonzero_id_strategy(),
        gid in nonzero_id_strategy(),
        user_name in owner_name_strategy(),
        group_name in owner_name_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_uid(uid);
        entry.set_gid(gid);
        entry.set_user_name(user_name.clone());
        entry.set_group_name(group_name.clone());

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.uid(), Some(uid), "uid mismatch");
        assert_eq!(decoded.gid(), Some(gid), "gid mismatch");
        assert_eq!(decoded.user_name(), Some(user_name.as_str()), "user name mismatch");
        assert_eq!(decoded.group_name(), Some(group_name.as_str()), "group name mismatch");
    }
}

// ---------------------------------------------------------------------------
// Large file size roundtrips (varlong encoding, protocol 30+)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Large file sizes (> 2 GB) roundtrip on protocol 30+ using varlong encoding.
    ///
    /// Protocol 28-29 use fixed 32-bit or 64-bit encoding. Protocol 30+ uses
    /// varlong which is more compact for typical file sizes.
    #[test]
    fn large_file_size_roundtrip(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        size in large_file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::new(protocol);
        let mut reader = FileListReader::new(protocol);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
    }
}

// ---------------------------------------------------------------------------
// Hardlink leader roundtrips (protocol 30+)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Hardlink leaders (first occurrence) roundtrip with XMIT_HLINK_FIRST.
    ///
    /// A leader has hardlink_idx == u32::MAX, which signals "this is the first
    /// occurrence." The writer sets both XMIT_HLINKED and XMIT_HLINK_FIRST.
    /// Full metadata is written (unlike followers).
    #[test]
    fn hardlink_leader_roundtrip(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_hardlink_idx(u32::MAX); // Leader marker

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_hard_links(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_hard_links(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.hardlink_idx(), Some(u32::MAX), "leader should have u32::MAX idx");
    }

    /// Hardlink followers reference a leader by index.
    ///
    /// A follower has hardlink_idx < u32::MAX. The writer sets XMIT_HLINKED
    /// but NOT XMIT_HLINK_FIRST, and skips all metadata after the index.
    /// The decoded entry will have zeroed metadata fields.
    #[test]
    fn hardlink_follower_roundtrip(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        idx in 0u32..1000,
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, 500, 0o644);
        entry.set_mtime(1000, 0);
        entry.set_hardlink_idx(idx);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_hard_links(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_hard_links(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_eq!(decoded.name(), entry.name(), "name mismatch");
        assert_eq!(decoded.hardlink_idx(), Some(idx), "follower idx mismatch");
        // Followers have zeroed metadata on the wire
        assert_eq!(decoded.size(), 0, "follower size should be 0");
        assert_eq!(decoded.mode(), 0, "follower mode should be 0");
    }
}

// ---------------------------------------------------------------------------
// Hardlink dev/ino roundtrips (protocol 28-29)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Hardlink dev/ino pairs roundtrip on protocol 28-29.
    ///
    /// In older protocols, hardlinks are identified by (dev, ino) pairs
    /// instead of indices. Dev is encoded as dev+1 (0 is reserved sentinel).
    #[test]
    fn hardlink_dev_ino_roundtrip(
        proto in prop::sample::select(vec![28u8, 29]),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        dev in 0i64..=1_000_000,
        ino in 1i64..=10_000_000,
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_hardlink_dev(dev);
        entry.set_hardlink_ino(ino);

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_hard_links(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_hard_links(true);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.hardlink_dev(), Some(dev), "hardlink dev mismatch");
        assert_eq!(decoded.hardlink_ino(), Some(ino), "hardlink ino mismatch");
    }
}

// ---------------------------------------------------------------------------
// Content directory flag roundtrips (protocol 30+)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Directories with content_dir=false roundtrip with XMIT_NO_CONTENT_DIR.
    ///
    /// Protocol 30+ supports marking directories whose contents should not be
    /// transferred (implied or content-less directories).
    #[test]
    fn no_content_dir_roundtrip(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_directory(name, perms);
        entry.set_mtime(mtime, 0);
        entry.set_content_dir(false);

        let mut writer = FileListWriter::new(protocol);
        let mut reader = FileListReader::new(protocol);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_eq!(decoded.name(), entry.name(), "name mismatch");
        assert!(decoded.is_dir(), "decoded should be a directory");
        assert!(!decoded.content_dir(), "content_dir should be false");
    }

    /// Directories default to content_dir=true.
    #[test]
    fn content_dir_true_roundtrip(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_directory(name, perms);
        entry.set_mtime(mtime, 0);

        let mut writer = FileListWriter::new(protocol);
        let mut reader = FileListReader::new(protocol);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert!(decoded.is_dir(), "decoded should be a directory");
        assert!(decoded.content_dir(), "content_dir should default to true");
    }
}

// ---------------------------------------------------------------------------
// Encoding determinism
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Encoding the same entry twice produces identical byte sequences.
    ///
    /// Verifies that write_entry is deterministic - important for reproducible
    /// transfers and debugging.
    #[test]
    fn encoding_is_deterministic(
        proto in protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);

        let mut buf1 = Vec::new();
        let mut writer1 = FileListWriter::new(protocol);
        writer1.write_entry(&mut buf1, &entry).unwrap();

        let mut buf2 = Vec::new();
        let mut writer2 = FileListWriter::new(protocol);
        writer2.write_entry(&mut buf2, &entry).unwrap();

        prop_assert_eq!(&buf1, &buf2, "encoding should be deterministic");
    }
}

// ---------------------------------------------------------------------------
// Cross-entry delta compression stress
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Entries that share all delta-compressible fields still roundtrip.
    ///
    /// When consecutive entries have identical mode, mtime, uid, gid, and
    /// similar name prefixes, maximum compression is applied. This exercises
    /// all XMIT_SAME_* flags simultaneously.
    #[test]
    fn max_delta_compression_roundtrip(
        proto in protocol_version_strategy(),
        prefix in filename_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        uid in id_strategy(),
        gid in id_strategy(),
        count in 3usize..=8,
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();

        // All entries share mode, mtime, uid, gid, and name prefix
        let entries: Vec<FileEntry> = (0..count)
            .map(|i| {
                let path = PathBuf::from(format!("{prefix}_{i:04}.dat"));
                let mut e = FileEntry::new_file(path, (i as u64 + 1) * 256, perms);
                e.set_mtime(mtime, 0);
                e.set_uid(uid);
                e.set_gid(gid);
                e
            })
            .collect();

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);
        let decoded = roundtrip_sequence(&mut writer, &mut reader, &entries);

        prop_assert_eq!(decoded.len(), entries.len(), "entry count mismatch");
        for (orig, dec) in entries.iter().zip(decoded.iter()) {
            assert_core_fields_match(orig, dec);
            assert_eq!(dec.uid(), Some(uid), "uid mismatch");
            assert_eq!(dec.gid(), Some(gid), "gid mismatch");
        }
    }

    /// Entries with no shared fields exercise the minimal compression path.
    ///
    /// Every field differs between consecutive entries, so no XMIT_SAME_*
    /// flags are set. This is the maximum-bytes-per-entry scenario.
    #[test]
    fn no_delta_compression_roundtrip(
        proto in protocol_version_strategy(),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();

        // Deliberately different names, sizes, modes, mtimes, uid, gid
        let mut e1 = FileEntry::new_file("aardvark.rs".into(), 100, 0o644);
        e1.set_mtime(1_000_000, 0);
        e1.set_uid(1000);
        e1.set_gid(1000);

        let mut e2 = FileEntry::new_file("zebra.py".into(), 999_999, 0o755);
        e2.set_mtime(2_000_000, 0);
        e2.set_uid(2000);
        e2.set_gid(2000);

        let mut e3 = FileEntry::new_file("mango.txt".into(), 50_000, 0o600);
        e3.set_mtime(3_000_000, 0);
        e3.set_uid(3000);
        e3.set_gid(3000);

        let entries = vec![e1, e2, e3];

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);
        let decoded = roundtrip_sequence(&mut writer, &mut reader, &entries);

        prop_assert_eq!(decoded.len(), entries.len(), "entry count mismatch");
        for (orig, dec) in entries.iter().zip(decoded.iter()) {
            assert_core_fields_match(orig, dec);
            assert_eq!(dec.uid(), orig.uid(), "uid mismatch");
            assert_eq!(dec.gid(), orig.gid(), "gid mismatch");
        }
    }
}

// ---------------------------------------------------------------------------
// Combined preserve flags roundtrip
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Full-featured roundtrip with all preserve flags enabled simultaneously.
    ///
    /// Exercises the complete wire format path with uid, gid, atimes, crtimes,
    /// user/group names, and checksums all active at once.
    #[test]
    fn all_preserve_flags_roundtrip(
        proto in modern_protocol_version_strategy(),
        name in basename_strategy(),
        size in file_size_strategy(),
        perms in permissions_strategy(),
        mtime in mtime_strategy(),
        uid in nonzero_id_strategy(),
        gid in nonzero_id_strategy(),
        user_name in owner_name_strategy(),
        group_name in owner_name_strategy(),
        atime in mtime_strategy(),
        crtime in mtime_strategy(),
        checksum in checksum_strategy(16),
    ) {
        let protocol = ProtocolVersion::try_from(proto).unwrap();
        let csum_len = 16;
        let mut entry = FileEntry::new_file(name, size, perms);
        entry.set_mtime(mtime, 0);
        entry.set_uid(uid);
        entry.set_gid(gid);
        entry.set_user_name(user_name.clone());
        entry.set_group_name(group_name.clone());
        entry.set_atime(atime);
        entry.set_crtime(crtime);
        entry.set_checksum(checksum.clone());

        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true)
            .with_preserve_atimes(true)
            .with_preserve_crtimes(true)
            .with_always_checksum(csum_len);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true)
            .with_preserve_atimes(true)
            .with_preserve_crtimes(true)
            .with_always_checksum(csum_len);

        let decoded = roundtrip(&mut writer, &mut reader, &entry);
        assert_core_fields_match(&entry, &decoded);
        assert_eq!(decoded.uid(), Some(uid), "uid mismatch");
        assert_eq!(decoded.gid(), Some(gid), "gid mismatch");
        assert_eq!(decoded.user_name(), Some(user_name.as_str()), "user name mismatch");
        assert_eq!(decoded.group_name(), Some(group_name.as_str()), "group name mismatch");
        assert_eq!(decoded.atime(), atime, "atime mismatch");
        assert_eq!(decoded.crtime(), crtime, "crtime mismatch");
        assert_eq!(decoded.checksum(), Some(checksum.as_slice()), "checksum mismatch");
    }
}
