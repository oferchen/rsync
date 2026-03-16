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
fn filename_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_.][a-zA-Z0-9_.\\-]{0,63}"
}

/// Generates a valid relative path with 1-3 components.
fn path_strategy() -> impl Strategy<Value = PathBuf> {
    prop::collection::vec(filename_strategy(), 1..=3).prop_map(|parts| {
        let joined = parts.join("/");
        PathBuf::from(joined)
    })
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

/// Generates a device major/minor number.
fn rdev_strategy() -> impl Strategy<Value = (u32, u32)> {
    (0u32..=255, 0u32..=255)
}

/// Supported protocol versions.
fn protocol_version_strategy() -> impl Strategy<Value = u8> {
    prop::sample::select(vec![28u8, 29, 30, 31, 32])
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
        name in path_strategy(),
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
        name in path_strategy(),
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
        name in path_strategy(),
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
        name in path_strategy(),
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
        name in path_strategy(),
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
        name in path_strategy(),
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
        name in path_strategy(),
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
        name in path_strategy(),
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

        // Build entries sharing the same directory prefix
        let entries: Vec<FileEntry> = (0..count)
            .map(|i| {
                let path = PathBuf::from(format!("{}/file{}.dat", dir, i));
                let size = (i as u64 + 1) * 100;
                let mut e = FileEntry::new_file(path, size, base_perms);
                e.set_mtime(base_mtime, 0);
                e
            })
            .collect();

        // Encode all entries then the end marker
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);
        for entry in &entries {
            writer.write_entry(&mut buf, entry).unwrap();
        }
        writer.write_end(&mut buf, None).unwrap();

        // Decode all entries
        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);
        let mut decoded = Vec::new();
        while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
            decoded.push(entry);
        }

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

        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_links(true);
        for entry in &entries {
            writer.write_entry(&mut buf, entry).unwrap();
        }
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_links(true);
        let mut decoded = Vec::new();
        while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
            decoded.push(entry);
        }

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
        name in path_strategy(),
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
        name in path_strategy(),
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
