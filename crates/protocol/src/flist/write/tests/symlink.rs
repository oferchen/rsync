use super::*;

#[test]
fn write_symlink_entry_with_preserve_links() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

    let entry = FileEntry::new_symlink("link".into(), "/target/path".into());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "link");
    assert!(read_entry.is_symlink());
    assert_eq!(
        read_entry
            .link_target()
            .map(|p| p.to_string_lossy().into_owned()),
        Some("/target/path".to_string())
    );
}

#[test]
fn write_symlink_entry_without_preserve_links_omits_target() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let entry = FileEntry::new_symlink("link".into(), "/target/path".into());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "link");
    assert!(read_entry.is_symlink());
    assert!(read_entry.link_target().is_none());
}

#[test]
fn write_symlink_round_trip_protocol_30_varint() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 30+ encodes the symlink-target length as a varint30.
    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

    let entry = FileEntry::new_symlink("mylink".into(), "../relative/path".into());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "mylink");
    assert!(read_entry.is_symlink());
    assert_eq!(
        read_entry
            .link_target()
            .map(|p| p.to_string_lossy().into_owned()),
        Some("../relative/path".to_string())
    );
}

#[test]
fn write_symlink_round_trip_protocol_29_fixed_int() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 29 encodes the symlink-target length as a fixed 4-byte int.
    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

    let entry = FileEntry::new_symlink("oldlink".into(), "/old/target".into());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "oldlink");
    assert!(read_entry.is_symlink());
    assert_eq!(
        read_entry
            .link_target()
            .map(|p| p.to_string_lossy().into_owned()),
        Some("/old/target".to_string())
    );
}

/// Regression for #1905 / #1939: symlink targets must also be normalised.
/// A Windows-side relative symlink target like `sub\target.txt` would emit
/// raw bytes containing `\` without normalisation; this asserts the helper is
/// applied on the symlink-target write path too.
#[test]
fn wire_encoded_symlink_target_never_contains_backslash_byte() {
    use std::path::PathBuf;

    let mut target = PathBuf::from("sub");
    target.push("target.txt");

    let mut entry = FileEntry::new_symlink("link".into(), target);
    entry.set_mtime(0, 0);

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_links(true);
    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        !buf.contains(&b'\\'),
        "wire-encoded symlink entry must not contain a `\\` byte, got: {buf:?}",
    );
    let needle = b"sub/target.txt";
    assert!(
        buf.windows(needle.len()).any(|w| w == needle),
        "symlink target must be forward-slash form `sub/target.txt`, got: {buf:?}",
    );
}

/// Symlink targets are transcoded by the same `ic_send` converter as
/// filenames ONLY when `--iconv=LOCAL,REMOTE` is in effect AND CF_SYMLINK_ICONV
/// has been negotiated (`with_symlink_iconv(true)`). A UTF-8 local target must
/// appear on the wire as ISO-8859-1 bytes when the converter is configured
/// `(local=UTF-8, remote=ISO-8859-1)`.
///
/// upstream: flist.c:1642 send_file1() - `sender_symlink_iconv` path.
#[cfg(feature = "iconv")]
#[test]
fn write_symlink_target_transcodes_with_iconv_to_remote_charset() {
    use crate::iconv::FilenameConverter;

    let utf8_target = "café";
    let latin1_bytes: &[u8] = &[0x63, 0x61, 0x66, 0xe9];

    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").unwrap();
    let mut writer = FileListWriter::new(test_protocol())
        .with_preserve_links(true)
        .with_iconv(converter)
        .with_symlink_iconv(true);

    let mut entry = FileEntry::new_symlink("link".into(), utf8_target.into());
    entry.set_mtime(1_700_000_000, 0);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        buf.windows(latin1_bytes.len()).any(|w| w == latin1_bytes),
        "wire bytes must contain ISO-8859-1 form of the symlink target, got: {buf:?}",
    );
    let utf8_target_bytes = utf8_target.as_bytes();
    assert!(
        !buf.windows(utf8_target_bytes.len())
            .any(|w| w == utf8_target_bytes),
        "wire bytes must not contain UTF-8 form of the symlink target, got: {buf:?}",
    );
}

/// A converter WITHOUT negotiated CF_SYMLINK_ICONV (e.g. a proto-30 / rsync
/// 3.0.x peer) must leave symlink targets as raw local bytes even while
/// filenames are transcoded. This is the `else` branch at upstream flist.c:1659
/// where `sender_symlink_iconv` is 0.
///
/// upstream: compat.c:765-767 - `sender_symlink_iconv = iconv_opt &&
/// (... CF_SYMLINK_ICONV)`; flist.c:1642 gate.
#[cfg(feature = "iconv")]
#[test]
fn write_symlink_target_without_negotiated_flag_passes_raw_local_bytes() {
    use crate::iconv::FilenameConverter;

    let utf8_target = "café";
    let utf8_bytes = utf8_target.as_bytes();
    let latin1_bytes: &[u8] = &[0x63, 0x61, 0x66, 0xe9];

    // Converter is attached (filenames would transcode) but symlink_iconv is
    // left at its default `false`, modelling a peer that did not advertise 's'.
    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").unwrap();
    let mut writer = FileListWriter::new(test_protocol())
        .with_preserve_links(true)
        .with_iconv(converter);

    let mut entry = FileEntry::new_symlink("link".into(), utf8_target.into());
    entry.set_mtime(1_700_000_000, 0);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        buf.windows(utf8_bytes.len()).any(|w| w == utf8_bytes),
        "target must stay raw UTF-8 local bytes without CF_SYMLINK_ICONV, got: {buf:?}",
    );
    assert!(
        !buf.windows(latin1_bytes.len()).any(|w| w == latin1_bytes),
        "target must NOT be transcoded to ISO-8859-1 without CF_SYMLINK_ICONV, got: {buf:?}",
    );
}

/// Without a converter the symlink target is written verbatim. This pins
/// the no-iconv default so the new transcoding hook does not regress
/// existing transfers.
#[test]
fn write_symlink_target_without_iconv_emits_raw_bytes() {
    let utf8_target = "café";
    let utf8_bytes = utf8_target.as_bytes();

    let mut writer = FileListWriter::new(test_protocol()).with_preserve_links(true);
    let mut entry = FileEntry::new_symlink("link".into(), utf8_target.into());
    entry.set_mtime(1_700_000_000, 0);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        buf.windows(utf8_bytes.len()).any(|w| w == utf8_bytes),
        "without --iconv, symlink target wire bytes must match the original, got: {buf:?}",
    );
}
