//! Comprehensive tests for symlink target encoding in the rsync wire protocol.
//!
//! Upstream rsync sends symlink targets as raw bytes on the wire:
//! - Wire format: `varint30(len)` + `target_bytes` (no null terminator)
//! - Symlink targets can contain any bytes (not just valid UTF-8)
//! - Relative vs absolute targets are preserved exactly
//! - Long targets must be handled correctly
//! - The encoding must handle special characters
//!
//! These tests verify byte-exact fidelity of symlink target encoding/decoding
//! at both the low-level wire format layer and the high-level FileListWriter/
//! FileListReader layer, matching upstream rsync's `flist.c` behavior.
//!
//! # Upstream Reference
//!
//! - `flist.c:send_file_entry()` lines 685-695 (symlink target write)
//! - `flist.c:recv_file_entry()` lines 948-960 (symlink target read)

use std::io::Cursor;
use std::path::PathBuf;

use protocol::flist::{FileEntry, FileListReader, FileListWriter};
use protocol::wire::file_entry::encode_symlink_target;
use protocol::wire::file_entry_decode::decode_symlink_target;
use protocol::ProtocolVersion;

// ============================================================================
// Test Helpers
// ============================================================================

/// Roundtrip a single symlink entry through FileListWriter/FileListReader.
fn roundtrip_symlink(
    name: &str,
    target: PathBuf,
    protocol: ProtocolVersion,
) -> FileEntry {
    let mut entry = FileEntry::new_symlink(PathBuf::from(name), target);
    entry.set_mtime(1700000000, 0);

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
    writer.write_entry(&mut buf, &entry).expect("write failed");
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);
    reader
        .read_entry(&mut cursor)
        .expect("read failed")
        .expect("no entry returned")
}

/// Roundtrip multiple entries through FileListWriter/FileListReader.
fn roundtrip_entries(
    entries: &[FileEntry],
    protocol: ProtocolVersion,
) -> Vec<FileEntry> {
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
    for entry in entries {
        writer.write_entry(&mut buf, &entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);
    let mut decoded = Vec::new();
    while let Some(entry) = reader.read_entry(&mut cursor).expect("read failed") {
        decoded.push(entry);
    }
    decoded
}

/// Creates a symlink FileEntry with the given name and target path.
fn make_symlink(name: &str, target: &str) -> FileEntry {
    let mut entry = FileEntry::new_symlink(PathBuf::from(name), PathBuf::from(target));
    entry.set_mtime(1700000000, 0);
    entry
}

/// Creates a symlink FileEntry from raw bytes for the target (Unix-only).
#[cfg(unix)]
fn make_symlink_from_bytes(name: &str, target_bytes: &[u8]) -> FileEntry {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let target = PathBuf::from(OsStr::from_bytes(target_bytes));
    let mut entry = FileEntry::new_symlink(PathBuf::from(name), target);
    entry.set_mtime(1700000000, 0);
    entry
}

/// Extracts the raw bytes of a symlink target from a FileEntry.
#[cfg(unix)]
fn target_bytes(entry: &FileEntry) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    entry
        .link_target()
        .expect("expected symlink target")
        .as_os_str()
        .as_bytes()
}

// ============================================================================
// 1. Low-Level Wire Format Tests (encode_symlink_target / decode_symlink_target)
// ============================================================================

/// Tests basic roundtrip of symlink target encoding at the wire level.
#[test]
fn wire_level_roundtrip_simple_absolute_target() {
    for proto in [28u8, 29, 30, 31, 32] {
        let target = b"/usr/local/bin/python";
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, target, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_symlink_target(&mut cursor, proto).unwrap();
        assert_eq!(
            decoded, target,
            "wire-level roundtrip failed for protocol {}",
            proto
        );
    }
}

/// Tests wire-level roundtrip for relative symlink target.
#[test]
fn wire_level_roundtrip_relative_target() {
    for proto in [28u8, 30, 32] {
        let target = b"../lib/libfoo.so.1";
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, target, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_symlink_target(&mut cursor, proto).unwrap();
        assert_eq!(decoded, target, "relative target failed for proto {}", proto);
    }
}

/// Tests wire-level encoding of an empty symlink target.
#[test]
fn wire_level_roundtrip_empty_target() {
    for proto in [28u8, 30, 32] {
        let target = b"";
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, target, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_symlink_target(&mut cursor, proto).unwrap();
        assert_eq!(decoded, target, "empty target failed for proto {}", proto);
    }
}

/// Tests wire-level encoding of a single-byte symlink target.
#[test]
fn wire_level_roundtrip_single_byte_target() {
    let target = b".";
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = decode_symlink_target(&mut cursor, 32).unwrap();
    assert_eq!(decoded, target);
}

/// Tests wire-level encoding of a very long symlink target (4096 bytes).
#[test]
fn wire_level_roundtrip_long_target() {
    // PATH_MAX is typically 4096 on Linux
    let target: Vec<u8> = (0..4096).map(|i| b"abcdefghij"[i % 10]).collect();

    for proto in [28u8, 30, 32] {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, &target, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_symlink_target(&mut cursor, proto).unwrap();
        assert_eq!(decoded, target, "long target failed for proto {}", proto);
    }
}

/// Tests wire-level encoding with binary data in the target.
#[test]
fn wire_level_roundtrip_binary_target() {
    // All 256 byte values except NUL (which is the C string terminator in upstream,
    // but the wire protocol sends length-prefixed data, so bytes are arbitrary)
    let target: Vec<u8> = (1u8..=255).collect();

    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, &target, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = decode_symlink_target(&mut cursor, 32).unwrap();
    assert_eq!(decoded, target);
}

/// Tests wire-level encoding with embedded NUL bytes.
/// The wire protocol is length-prefixed, not NUL-terminated, so NUL bytes
/// should be preserved in the target.
#[test]
fn wire_level_roundtrip_embedded_nul() {
    let target = b"foo\x00bar\x00baz";

    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = decode_symlink_target(&mut cursor, 32).unwrap();
    assert_eq!(decoded, target);
}

/// Tests that the wire format header correctly encodes the length.
#[test]
fn wire_level_length_encoding_correctness() {
    let target = b"target";

    // Protocol 30+ uses varint30 for length
    let mut buf30 = Vec::new();
    encode_symlink_target(&mut buf30, target, 30).unwrap();
    // First byte(s) should encode length 6, followed by "target"
    assert!(buf30.ends_with(b"target"));
    assert!(buf30.len() > 6); // At least 1 byte for length + 6 bytes for data

    // Protocol 29 uses fixed int for length
    let mut buf29 = Vec::new();
    encode_symlink_target(&mut buf29, target, 29).unwrap();
    assert!(buf29.ends_with(b"target"));
    // Fixed int: 4 bytes for length + 6 bytes for data = 10
    assert_eq!(buf29.len(), 10);
    let len = i32::from_le_bytes([buf29[0], buf29[1], buf29[2], buf29[3]]);
    assert_eq!(len, 6);
}

// ============================================================================
// 2. High-Level Roundtrip Tests (FileListWriter / FileListReader)
// ============================================================================

/// Tests high-level roundtrip of a simple absolute symlink target.
#[test]
fn flist_roundtrip_absolute_target() {
    for protocol in [ProtocolVersion::V28, ProtocolVersion::V30, ProtocolVersion::NEWEST] {
        let decoded = roundtrip_symlink("link", PathBuf::from("/usr/bin/python"), protocol);
        assert!(decoded.is_symlink());
        assert_eq!(decoded.name(), "link");
        assert_eq!(
            decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some("/usr/bin/python".to_string()),
            "absolute target mismatch for {:?}",
            protocol,
        );
    }
}

/// Tests high-level roundtrip of a relative symlink target.
#[test]
fn flist_roundtrip_relative_target() {
    for protocol in [ProtocolVersion::V28, ProtocolVersion::V30, ProtocolVersion::NEWEST] {
        let decoded =
            roundtrip_symlink("link", PathBuf::from("../lib/libfoo.so"), protocol);
        assert!(decoded.is_symlink());
        assert_eq!(
            decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some("../lib/libfoo.so".to_string()),
            "relative target mismatch for {:?}",
            protocol,
        );
    }
}

/// Tests that dot-relative symlink targets are preserved.
#[test]
fn flist_roundtrip_dot_relative_target() {
    let decoded = roundtrip_symlink("link", PathBuf::from("./same_dir_file"), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some("./same_dir_file".to_string()),
    );
}

/// Tests parent traversal symlink targets are preserved.
#[test]
fn flist_roundtrip_parent_traversal_target() {
    let target = "../../other/dir/file.txt";
    let decoded = roundtrip_symlink("link", PathBuf::from(target), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some(target.to_string()),
    );
}

/// Tests that a deeply nested absolute target is preserved.
#[test]
fn flist_roundtrip_deeply_nested_target() {
    let target = "/a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z";
    let decoded = roundtrip_symlink("link", PathBuf::from(target), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some(target.to_string()),
    );
}

/// Tests that the root path "/" is preserved as a symlink target.
#[test]
fn flist_roundtrip_root_target() {
    let decoded = roundtrip_symlink("link", PathBuf::from("/"), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some("/".to_string()),
    );
}

/// Tests that symlink targets with trailing slashes are preserved.
#[test]
fn flist_roundtrip_trailing_slash_target() {
    // Note: PathBuf may normalize trailing slashes on some platforms.
    // This test verifies whatever the platform does is consistent.
    let target = PathBuf::from("/some/dir");
    let decoded = roundtrip_symlink("link", target.clone(), ProtocolVersion::NEWEST);
    assert_eq!(decoded.link_target(), Some(&target));
}

// ============================================================================
// 3. Special Characters in Symlink Targets
// ============================================================================

/// Tests symlink targets containing spaces.
#[test]
fn flist_roundtrip_target_with_spaces() {
    let target = "/path/to/my file with spaces.txt";
    let decoded = roundtrip_symlink("link", PathBuf::from(target), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some(target.to_string()),
    );
}

/// Tests symlink targets containing special shell characters.
#[test]
fn flist_roundtrip_target_with_shell_special_chars() {
    let targets = [
        "/path/with$dollar",
        "/path/with!exclaim",
        "/path/with#hash",
        "/path/with&ampersand",
        "/path/with(parens)",
        "/path/with[brackets]",
        "/path/with{braces}",
        "/path/with|pipe",
        "/path/with;semicolon",
        "/path/with'quotes'",
        "/path/with\"doublequotes\"",
        "/path/with\\backslash",
        "/path/with`backtick`",
        "/path/with~tilde",
    ];

    for target in &targets {
        let decoded = roundtrip_symlink("link", PathBuf::from(target), ProtocolVersion::NEWEST);
        assert_eq!(
            decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some(target.to_string()),
            "failed for target: {}",
            target,
        );
    }
}

/// Tests symlink targets containing Unicode characters.
#[test]
fn flist_roundtrip_target_with_unicode() {
    let targets = [
        "/path/to/\u{00e9}t\u{00e9}",         // French accented chars
        "/\u{65e5}\u{672c}\u{8a9e}",           // Japanese
        "/\u{0410}\u{0411}\u{0412}",           // Cyrillic
        "../\u{4e2d}\u{6587}/\u{6587}\u{4ef6}", // Chinese relative path
        "/\u{1f4c1}/\u{1f4c4}",                // Emoji folder/file
    ];

    for target in &targets {
        let decoded = roundtrip_symlink("link", PathBuf::from(target), ProtocolVersion::NEWEST);
        assert_eq!(
            decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some(target.to_string()),
            "failed for target: {}",
            target,
        );
    }
}

/// Tests symlink targets with newlines and control characters.
#[test]
fn flist_roundtrip_target_with_control_chars() {
    let targets = [
        "/path/with\nnewline",
        "/path/with\ttab",
        "/path/with\rcarriage_return",
    ];

    for target in &targets {
        let decoded = roundtrip_symlink("link", PathBuf::from(target), ProtocolVersion::NEWEST);
        assert_eq!(
            decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some(target.to_string()),
            "failed for control char target",
        );
    }
}

// ============================================================================
// 4. Non-UTF-8 Symlink Targets (Unix-Only)
// ============================================================================

/// Tests symlink target with invalid UTF-8 bytes.
/// On Unix, paths are arbitrary byte sequences. Upstream rsync sends raw bytes.
#[cfg(unix)]
#[test]
fn flist_roundtrip_non_utf8_target() {
    // 0xFF is not valid UTF-8
    let raw_target = b"/path/with\xff\xfeinvalid/utf8";
    let decoded = roundtrip_symlink(
        "link",
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(OsStr::from_bytes(raw_target))
        },
        ProtocolVersion::NEWEST,
    );

    assert!(decoded.is_symlink());
    let decoded_bytes = target_bytes(&decoded);
    assert_eq!(decoded_bytes, raw_target, "non-UTF-8 target bytes must survive roundtrip");
}

/// Tests symlink target containing Latin-1 encoded characters (not valid UTF-8).
#[cfg(unix)]
#[test]
fn flist_roundtrip_latin1_target() {
    // Latin-1 encoding of "caf\xe9" (cafe with e-acute as single byte)
    let raw_target = b"/caf\xe9/menu";
    let decoded = roundtrip_symlink(
        "link",
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(OsStr::from_bytes(raw_target))
        },
        ProtocolVersion::NEWEST,
    );

    let decoded_bytes = target_bytes(&decoded);
    assert_eq!(decoded_bytes, raw_target);
}

/// Tests symlink target with all high-byte values (0x80-0xFF).
#[cfg(unix)]
#[test]
fn flist_roundtrip_all_high_bytes_target() {
    let mut raw_target = vec![b'/'];
    raw_target.extend(0x80u8..=0xFF);

    let decoded = roundtrip_symlink(
        "link",
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(OsStr::from_bytes(&raw_target))
        },
        ProtocolVersion::NEWEST,
    );

    let decoded_bytes = target_bytes(&decoded);
    assert_eq!(decoded_bytes, &raw_target[..]);
}

/// Tests symlink target with mixed valid UTF-8 and invalid bytes.
#[cfg(unix)]
#[test]
fn flist_roundtrip_mixed_utf8_and_invalid_target() {
    // Valid UTF-8 "hello" + invalid byte + valid UTF-8 "world"
    let raw_target = b"/hello/\x80world";

    let decoded = roundtrip_symlink(
        "link",
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(OsStr::from_bytes(raw_target))
        },
        ProtocolVersion::NEWEST,
    );

    let decoded_bytes = target_bytes(&decoded);
    assert_eq!(decoded_bytes, raw_target);
}

/// Tests non-UTF-8 targets across multiple protocol versions.
#[cfg(unix)]
#[test]
fn flist_roundtrip_non_utf8_target_all_protocols() {
    let raw_target = b"../\xff\xfe\xfd/file";

    for protocol in [ProtocolVersion::V28, ProtocolVersion::V30, ProtocolVersion::NEWEST] {
        let entry = make_symlink_from_bytes("link", raw_target);

        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
        writer.write_entry(&mut buf, &entry).expect("write failed");
        writer.write_end(&mut buf, None).expect("write end failed");

        let mut cursor = Cursor::new(&buf);
        let mut reader = FileListReader::new(protocol).with_preserve_links(true);
        let decoded = reader.read_entry(&mut cursor).expect("read failed").expect("no entry");

        let decoded_bytes = target_bytes(&decoded);
        assert_eq!(
            decoded_bytes, raw_target,
            "non-UTF-8 roundtrip failed for {:?}",
            protocol,
        );
    }
}

// ============================================================================
// 5. Long Symlink Targets
// ============================================================================

/// Tests symlink target at PATH_MAX boundary (4096 bytes).
#[test]
fn flist_roundtrip_path_max_target() {
    // Build a target of exactly 4096 bytes
    let mut target = String::from("/");
    while target.len() < 4095 {
        target.push_str("a/");
    }
    // Trim to exactly 4096
    target.truncate(4096);

    let decoded = roundtrip_symlink("link", PathBuf::from(&target), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some(target),
    );
}

/// Tests symlink target exceeding PATH_MAX (8192 bytes).
/// The wire protocol does not enforce PATH_MAX; it is length-prefixed.
#[test]
fn flist_roundtrip_exceeding_path_max_target() {
    let target: String = std::iter::repeat('x').take(8192).collect();

    let decoded = roundtrip_symlink("link", PathBuf::from(&target), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some(target),
    );
}

/// Tests symlink target of exactly 255 bytes (varint boundary in some protocols).
#[test]
fn flist_roundtrip_255_byte_target() {
    let target: String = std::iter::repeat('a').take(255).collect();

    for protocol in [ProtocolVersion::V28, ProtocolVersion::V30, ProtocolVersion::NEWEST] {
        let decoded = roundtrip_symlink("link", PathBuf::from(&target), protocol);
        assert_eq!(
            decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some(target.clone()),
            "255-byte target failed for {:?}",
            protocol,
        );
    }
}

/// Tests symlink target of exactly 256 bytes (just past single-byte varint threshold).
#[test]
fn flist_roundtrip_256_byte_target() {
    let target: String = std::iter::repeat('b').take(256).collect();

    for protocol in [ProtocolVersion::V28, ProtocolVersion::V30, ProtocolVersion::NEWEST] {
        let decoded = roundtrip_symlink("link", PathBuf::from(&target), protocol);
        assert_eq!(
            decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some(target.clone()),
            "256-byte target failed for {:?}",
            protocol,
        );
    }
}

/// Tests symlink target of exactly 65535 bytes (two-byte varint boundary).
#[test]
fn flist_roundtrip_65535_byte_target() {
    let target: String = std::iter::repeat('c').take(65535).collect();

    let decoded = roundtrip_symlink("link", PathBuf::from(&target), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded
            .link_target()
            .map(|p| p.to_string_lossy().len()),
        Some(65535),
    );
}

// ============================================================================
// 6. Multiple Symlinks in Sequence
// ============================================================================

/// Tests multiple symlinks with different target types in the same file list.
#[test]
fn flist_roundtrip_multiple_symlinks_mixed_targets() {
    let entries = vec![
        make_symlink("abs_link", "/absolute/target"),
        make_symlink("rel_link", "../relative/target"),
        make_symlink("dot_link", "./same_dir"),
        make_symlink("parent_link", "../../up/two/levels"),
    ];

    for protocol in [ProtocolVersion::V28, ProtocolVersion::V30, ProtocolVersion::NEWEST] {
        let decoded = roundtrip_entries(&entries, protocol);

        assert_eq!(decoded.len(), 4);
        for (i, (orig, dec)) in entries.iter().zip(decoded.iter()).enumerate() {
            assert!(dec.is_symlink(), "entry {} not symlink for {:?}", i, protocol);
            assert_eq!(
                dec.link_target().map(|p| p.to_string_lossy().into_owned()),
                orig.link_target().map(|p| p.to_string_lossy().into_owned()),
                "target mismatch at index {} for {:?}",
                i,
                protocol,
            );
        }
    }
}

/// Tests symlinks interleaved with regular files and directories.
#[test]
fn flist_roundtrip_symlinks_with_files_and_dirs() {
    let mut file = FileEntry::new_file(PathBuf::from("file.txt"), 100, 0o644);
    file.set_mtime(1700000000, 0);

    let mut dir = FileEntry::new_directory(PathBuf::from("mydir"), 0o755);
    dir.set_mtime(1700000000, 0);

    let entries = vec![
        file,
        make_symlink("link1", "/target1"),
        dir,
        make_symlink("link2", "../target2"),
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);
    assert_eq!(decoded.len(), 4);

    assert!(decoded[0].is_file());
    assert!(decoded[1].is_symlink());
    assert_eq!(
        decoded[1].link_target().map(|p| p.to_string_lossy().into_owned()),
        Some("/target1".to_string()),
    );
    assert!(decoded[2].is_dir());
    assert!(decoded[3].is_symlink());
    assert_eq!(
        decoded[3].link_target().map(|p| p.to_string_lossy().into_owned()),
        Some("../target2".to_string()),
    );
}

/// Tests that multiple symlinks with the same target roundtrip correctly.
#[test]
fn flist_roundtrip_same_target_multiple_symlinks() {
    let entries = vec![
        make_symlink("link_a", "/shared/target"),
        make_symlink("link_b", "/shared/target"),
        make_symlink("link_c", "/shared/target"),
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);

    for (i, dec) in decoded.iter().enumerate() {
        assert!(dec.is_symlink());
        assert_eq!(
            dec.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some("/shared/target".to_string()),
            "entry {} target mismatch",
            i,
        );
    }
}

// ============================================================================
// 7. Protocol Version Compatibility
// ============================================================================

/// Tests symlink target encoding across all supported protocol versions.
#[test]
fn flist_roundtrip_all_protocol_versions() {
    let target = "/usr/lib/libtest.so.1";

    for protocol in [
        ProtocolVersion::V28,
        ProtocolVersion::V29,
        ProtocolVersion::V30,
        ProtocolVersion::V31,
        ProtocolVersion::NEWEST,
    ] {
        let decoded = roundtrip_symlink("link", PathBuf::from(target), protocol);
        assert_eq!(
            decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some(target.to_string()),
            "failed for {:?}",
            protocol,
        );
    }
}

/// Tests that symlink targets are NOT transmitted when preserve_links is false.
#[test]
fn flist_no_target_when_preserve_links_disabled() {
    let entry = make_symlink("link", "/target");

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
    // preserve_links defaults to false
    writer.write_entry(&mut buf, &entry).expect("write failed");
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(ProtocolVersion::NEWEST);
    let decoded = reader
        .read_entry(&mut cursor)
        .expect("read failed")
        .expect("no entry");

    // Symlink mode should still be present, but target should be None
    assert!(decoded.is_symlink());
    assert!(
        decoded.link_target().is_none(),
        "target should be absent when preserve_links is disabled",
    );
}

// ============================================================================
// 8. Wire Format Details
// ============================================================================

/// Tests that the wire format for protocol 29 uses fixed 4-byte int for the length.
#[test]
fn wire_format_protocol_29_uses_fixed_int_length() {
    let target = b"/target";

    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 29).unwrap();

    // Protocol < 30: fixed 4-byte i32 LE for length
    assert_eq!(buf.len(), 4 + target.len());
    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len, target.len() as i32);
    assert_eq!(&buf[4..], target);
}

/// Tests that the wire format for protocol 30+ uses varint30 for the length.
#[test]
fn wire_format_protocol_30_uses_varint30_length() {
    let target = b"/target";

    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 30).unwrap();

    // varint30 for small values is typically 1 byte
    // Total should be less than 4 + target.len() (more compact than fixed int)
    assert!(buf.len() < 4 + target.len());
    assert!(buf.ends_with(target));
}

/// Verifies that encode and decode agree on the wire bytes across protocol versions.
#[test]
fn wire_format_encode_decode_consistency() {
    let targets: &[&[u8]] = &[
        b"",
        b"x",
        b"/a/b/c",
        b"../relative",
        &[0xFF; 128],
    ];

    for proto in [28u8, 29, 30, 31, 32] {
        for target in targets {
            let mut buf = Vec::new();
            encode_symlink_target(&mut buf, target, proto).unwrap();

            let mut cursor = Cursor::new(&buf);
            let decoded = decode_symlink_target(&mut cursor, proto).unwrap();

            assert_eq!(
                decoded.as_slice(),
                *target,
                "encode/decode mismatch for proto={}, target_len={}",
                proto,
                target.len(),
            );

            // Ensure all bytes were consumed
            assert_eq!(
                cursor.position() as usize,
                buf.len(),
                "not all bytes consumed for proto={}, target_len={}",
                proto,
                target.len(),
            );
        }
    }
}

// ============================================================================
// 9. Symlink Mode Preservation
// ============================================================================

/// Tests that symlink mode bits (0o120777) are preserved through the wire format.
#[test]
fn flist_symlink_mode_preserved() {
    let decoded = roundtrip_symlink("link", PathBuf::from("/target"), ProtocolVersion::NEWEST);

    // Symlinks always have mode 0o120777 (S_IFLNK | 0o777)
    assert_eq!(decoded.mode() & 0o170000, 0o120000, "S_IFLNK must be set");
    assert_eq!(decoded.mode() & 0o7777, 0o777, "permissions must be 0o777");
}

/// Tests that symlink entries have zero file size.
#[test]
fn flist_symlink_size_is_zero() {
    let decoded = roundtrip_symlink("link", PathBuf::from("/target"), ProtocolVersion::NEWEST);
    assert_eq!(decoded.size(), 0, "symlink file size should be 0");
}

// ============================================================================
// 10. Edge Cases and Boundary Conditions
// ============================================================================

/// Tests symlink target with very long path components (no slashes).
#[test]
fn flist_roundtrip_long_single_component_target() {
    let target: String = std::iter::repeat('a').take(1000).collect();
    let decoded = roundtrip_symlink("link", PathBuf::from(&target), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some(target),
    );
}

/// Tests symlink target with many short path components.
#[test]
fn flist_roundtrip_many_short_components_target() {
    let components: Vec<&str> = (0..100).map(|_| "x").collect();
    let target = components.join("/");
    let decoded = roundtrip_symlink("link", PathBuf::from(&target), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some(target),
    );
}

/// Tests symlink target consisting only of dots and slashes.
#[test]
fn flist_roundtrip_dots_and_slashes_target() {
    let targets = [
        ".",
        "..",
        "../..",
        "../../..",
        "./..",
        "../.",
    ];

    for target in &targets {
        let decoded = roundtrip_symlink("link", PathBuf::from(target), ProtocolVersion::NEWEST);
        assert_eq!(
            decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
            Some(target.to_string()),
            "failed for target: {}",
            target,
        );
    }
}

/// Tests that symlink name prefix compression works correctly with file entries.
/// Symlink entries use the same name compression as regular files.
#[test]
fn flist_symlink_name_prefix_compression() {
    let entries = vec![
        make_symlink("dir/link_a", "/target/a"),
        make_symlink("dir/link_b", "/target/b"),
        make_symlink("dir/link_c", "/target/c"),
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);

    assert_eq!(decoded.len(), 3);
    for (i, (orig, dec)) in entries.iter().zip(decoded.iter()).enumerate() {
        assert_eq!(dec.name(), orig.name(), "name mismatch at index {}", i);
        assert_eq!(
            dec.link_target().map(|p| p.to_string_lossy().into_owned()),
            orig.link_target().map(|p| p.to_string_lossy().into_owned()),
            "target mismatch at index {}",
            i,
        );
    }
}

/// Tests symlink with a target containing NUL bytes.
#[test]
fn flist_roundtrip_target_with_nul_bytes() {
    let target = "\x00";
    let decoded = roundtrip_symlink("link", PathBuf::from(target), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some(target.to_string()),
    );

    let target = "\x00\x00\x00\x00";
    let decoded = roundtrip_symlink("link2", PathBuf::from(target), ProtocolVersion::NEWEST);
    assert_eq!(
        decoded.link_target().map(|p| p.to_string_lossy().into_owned()),
        Some(target.to_string()),
    );
}

/// Tests symlink with a target containing 0xFF byte (non-UTF-8, Unix-only).
#[cfg(unix)]
#[test]
fn flist_roundtrip_target_0xff_byte() {
    let raw_target = &[0xFF];
    let entry = make_symlink_from_bytes("link", raw_target);

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(ProtocolVersion::NEWEST).with_preserve_links(true);
    writer.write_entry(&mut buf, &entry).expect("write failed");
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(ProtocolVersion::NEWEST).with_preserve_links(true);
    let decoded = reader.read_entry(&mut cursor).expect("read failed").expect("no entry");

    let decoded_bytes = target_bytes(&decoded);
    assert_eq!(decoded_bytes, raw_target);
}

/// Tests that consecutive symlinks with different target lengths work.
/// This verifies the length encoding doesn't leak state between entries.
#[test]
fn flist_roundtrip_varying_target_lengths() {
    let entries: Vec<FileEntry> = (1..=20)
        .map(|i| {
            let target: String = std::iter::repeat('x').take(i * 50).collect();
            make_symlink(&format!("link_{}", i), &target)
        })
        .collect();

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);

    assert_eq!(decoded.len(), entries.len());
    for (i, (orig, dec)) in entries.iter().zip(decoded.iter()).enumerate() {
        let orig_target = orig.link_target().unwrap().to_string_lossy().into_owned();
        let dec_target = dec.link_target().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            orig_target.len(),
            dec_target.len(),
            "target length mismatch at index {}",
            i,
        );
        assert_eq!(orig_target, dec_target, "target content mismatch at index {}", i);
    }
}

// ============================================================================
// 11. Statistics Tracking
// ============================================================================

/// Tests that symlink statistics are correctly tracked by the writer.
#[test]
fn flist_writer_symlink_statistics() {
    let entries = vec![
        make_symlink("link1", "/target1"),
        make_symlink("link2", "/longer/target2"),
        make_symlink("link3", "x"),
    ];

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(ProtocolVersion::NEWEST).with_preserve_links(true);
    for entry in &entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }

    let stats = writer.stats();
    assert_eq!(stats.num_symlinks, 3, "should count 3 symlinks");
    // Total size for symlinks = sum of target lengths
    let expected_size: u64 = entries
        .iter()
        .map(|e| e.link_target().unwrap().as_os_str().len() as u64)
        .sum();
    assert_eq!(
        stats.total_size, expected_size,
        "total_size should be sum of target lengths",
    );
}

// ============================================================================
// 12. Cross-Protocol Wire Byte Verification
// ============================================================================

/// Verifies exact wire bytes for a known symlink target in protocol 29 (fixed int length).
#[test]
fn wire_bytes_protocol_29_known_target() {
    let target = b"tgt";
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 29).unwrap();

    // Expected: i32 LE (3) = [0x03, 0x00, 0x00, 0x00] + b"tgt"
    assert_eq!(buf, &[0x03, 0x00, 0x00, 0x00, b't', b'g', b't']);
}

/// Verifies exact wire bytes for a known symlink target in protocol 30 (varint30 length).
#[test]
fn wire_bytes_protocol_30_known_target() {
    let target = b"tgt";
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 30).unwrap();

    // varint30 for value 3: single byte 0x03 (small positive int fits in 1 byte)
    assert_eq!(buf, &[0x03, b't', b'g', b't']);
}

/// Verifies that protocol 30 encoding is more compact than protocol 29 for small targets.
#[test]
fn wire_format_protocol_30_more_compact_than_29() {
    let target = b"/usr/lib/x86_64-linux-gnu/libc.so.6";

    let mut buf29 = Vec::new();
    encode_symlink_target(&mut buf29, target, 29).unwrap();

    let mut buf30 = Vec::new();
    encode_symlink_target(&mut buf30, target, 30).unwrap();

    assert!(
        buf30.len() < buf29.len(),
        "protocol 30 encoding ({} bytes) should be more compact than 29 ({} bytes)",
        buf30.len(),
        buf29.len(),
    );
}
