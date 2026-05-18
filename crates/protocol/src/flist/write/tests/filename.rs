use super::*;

/// Regression for #1905 / #1939.
///
/// On every platform, the wire-encoded filename must NEVER contain a backslash
/// byte. POSIX peers expect `/` as the only separator (upstream `flist.c`
/// writes filename bytes verbatim, and upstream's only Windows port runs
/// under Cygwin which presents `/`-separated paths to the writer).
#[test]
fn wire_encoded_filename_never_contains_backslash_byte() {
    use std::path::PathBuf;

    // Construct a path the same way an enumeration loop would on the host:
    // `PathBuf::push` uses the native separator on Windows.
    let mut path = PathBuf::from("subdir");
    path.push("file.txt");

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol());
    let entry = FileEntry::new_file(path, 100, 0o644);

    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        !buf.contains(&b'\\'),
        "wire-encoded entry must not contain a `\\` byte, got: {buf:?}",
    );
    let needle = b"subdir/file.txt";
    assert!(
        buf.windows(needle.len()).any(|w| w == needle),
        "wire-encoded entry must contain the forward-slash form `subdir/file.txt`, got: {buf:?}",
    );
}

/// Regression for #1905 / #1939: roundtrip via the reader must yield a
/// `/`-separated name regardless of the host platform that produced the bytes.
#[test]
fn wire_filename_roundtrip_yields_forward_slashes() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;
    use std::path::PathBuf;

    let mut path = PathBuf::from("a");
    path.push("b");
    path.push("c.txt");

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    let entry = FileEntry::new_file(path, 42, 0o644);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);
    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read_entry.name(), "a/b/c.txt");
    assert_eq!(read_entry.size(), 42);
}

/// Tracker #1912: when `--iconv=remote,local` is in effect, the sender's
/// filename must be transcoded from local to remote charset before being
/// written to the wire. A UTF-8 source name must appear on the wire as
/// ISO-8859-1 bytes when the converter is configured `(local=UTF-8,
/// remote=ISO-8859-1)`.
///
/// upstream: flist.c send_file_entry() iconv_buf(ic_send, ...)
#[cfg(feature = "iconv")]
#[test]
fn write_entry_transcodes_filename_with_iconv_to_remote_charset() {
    use crate::iconv::FilenameConverter;

    // Local UTF-8 "café.txt": 0x63 0x61 0x66 0xc3 0xa9 0x2e 0x74 0x78 0x74
    // Remote ISO-8859-1 "café.txt": 0x63 0x61 0x66 0xe9 0x2e 0x74 0x78 0x74
    let utf8_name = "café.txt";
    let latin1_bytes: &[u8] = &[0x63, 0x61, 0x66, 0xe9, 0x2e, 0x74, 0x78, 0x74];

    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").unwrap();
    let mut writer = FileListWriter::new(test_protocol()).with_iconv(converter);

    let entry = FileEntry::new_file(utf8_name.into(), 100, 0o644);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    // The transcoded ISO-8859-1 bytes must appear on the wire.
    assert!(
        buf.windows(latin1_bytes.len()).any(|w| w == latin1_bytes),
        "wire bytes must contain ISO-8859-1 form of the filename, got: {buf:?}",
    );

    // The original UTF-8 multi-byte sequence (0xc3 0xa9 for é) must NOT be
    // present, otherwise the converter was bypassed.
    let utf8_e_acute: &[u8] = &[0xc3, 0xa9];
    assert!(
        !buf.windows(utf8_e_acute.len()).any(|w| w == utf8_e_acute),
        "wire bytes must not contain UTF-8 form of é (0xc3 0xa9), got: {buf:?}",
    );
}

/// Tracker #1912: with no converter configured the sender must emit the
/// raw filename bytes unchanged, preserving current behaviour for all
/// transfers that do not pass `--iconv`.
#[test]
fn write_entry_without_iconv_emits_raw_filename_bytes() {
    let utf8_name = "café.txt";
    let utf8_bytes = utf8_name.as_bytes();

    let mut writer = FileListWriter::new(test_protocol());
    let entry = FileEntry::new_file(utf8_name.into(), 100, 0o644);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        buf.windows(utf8_bytes.len()).any(|w| w == utf8_bytes),
        "without --iconv, wire bytes must match the original filename, got: {buf:?}",
    );
}
