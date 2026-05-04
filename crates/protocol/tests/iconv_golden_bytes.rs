// Golden byte tests for --iconv-converted filenames in the file-list wire format.
//
// These tests lock the byte-level encoding of filenames after iconv conversion
// is applied at flist write/read time, so future regressions in the sender or
// receiver filename pipeline are caught at the byte level.
//
// upstream: rsync-3.4.1/flist.c
//   - send: lines 1580-1602 - ic_send applied to file->dirname and file->basename
//           before they reach the wire (raw filename bytes are in remote charset).
//   - recv: lines 738-753 - ic_recv applied after read_sbuf() into thisname,
//           before clean_fname() and before the entry is materialized.
//
// Wire layout for a single entry on protocol 32 (no compatibility flags, no
// extended flags requested - matches FileListWriter::new):
//
//   [flags:1]                   - 1-byte xflags (XMIT_TOP_DIR set for non-dir
//                                  with otherwise-zero flags)
//   [suffix_len:1]              - length of the (possibly converted) name in
//                                  bytes (raw bytes, post-iconv)
//   [name_bytes:suffix_len]     - the iconv-converted filename, byte-for-byte
//   [size_varlong:variable]     - file size (varlong-encoded)
//   [mtime:4 LE]                - mtime
//   [mode:4 LE]                 - mode bits
//
// followed eventually by [end_marker:1] = 0x00.
//
// The contract verified here: when the writer has a configured
// FilenameConverter, the wire bytes of the name field equal the bytes that
// the converter emits via local_to_remote(). Symmetrically, the reader
// applies remote_to_local() and surfaces the converted bytes through
// FileEntry's raw-byte API.
//
// Tests are gated on `feature = "iconv"` because non-UTF-8 charsets require
// the encoding_rs backend. The identity (UTF-8 <-> UTF-8) cases are within
// the same gate for consistency with the rest of the test surface.

#![cfg(feature = "iconv")]
#![allow(clippy::identity_op)]

#[cfg(unix)]
use std::io::Cursor;
use std::path::PathBuf;

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

use protocol::ProtocolVersion;
#[cfg(unix)]
use protocol::flist::FileListReader;
use protocol::flist::{FileEntry, FileListWriter};
use protocol::iconv::FilenameConverter;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Protocol used by all tests in this file. Protocol 32 matches upstream
/// rsync 3.4.1's negotiated default and uses single-byte xflags + 1-byte
/// suffix_len for short names.
fn proto() -> ProtocolVersion {
    ProtocolVersion::try_from(32u8).unwrap()
}

/// Builds a sender-side writer that converts local UTF-8 filenames to the
/// given remote charset before emitting them to the wire.
fn sender_writer(remote_charset: &str) -> FileListWriter {
    let conv = FilenameConverter::new("UTF-8", remote_charset)
        .expect("encoding_rs must support the requested remote charset");
    FileListWriter::new(proto()).with_iconv(conv)
}

/// Builds a receiver-side reader that converts wire bytes from the given
/// remote charset into local UTF-8 before exposing the entry name.
#[cfg(unix)]
fn receiver_reader(remote_charset: &str) -> FileListReader {
    let conv = FilenameConverter::new("UTF-8", remote_charset)
        .expect("encoding_rs must support the requested remote charset");
    FileListReader::new(proto()).with_iconv(conv)
}

/// Builds an unconverted file entry from a local UTF-8 name. Mtime is fixed
/// so that wire bytes after the name field stay deterministic.
fn entry_from_local(name: &str) -> FileEntry {
    let mut entry = FileEntry::new_file(PathBuf::from(name), 0, 0o644);
    entry.set_mtime(0, 0);
    entry
}

/// Builds an entry from raw bytes (used to construct entries whose name is
/// already in the remote encoding, e.g. ISO-8859-1 bytes that are not valid
/// UTF-8).
#[cfg(unix)]
fn entry_from_remote_bytes(name_bytes: &[u8]) -> FileEntry {
    let path = PathBuf::from(OsStr::from_bytes(name_bytes));
    let mut entry = FileEntry::new_file(path, 0, 0o644);
    entry.set_mtime(0, 0);
    entry
}

/// Locates the wire bytes of the name field inside a single-entry buffer.
///
/// Returns `(name_offset, name_len)` so callers can slice
/// `&buf[name_offset..name_offset + name_len]` and assert byte-for-byte
/// equality.
///
/// Layout for the FileListWriter::new() configuration on protocol 32 used
/// throughout these tests:
///   buf[0]                = xflags (1 byte, no XMIT_EXTENDED_FLAGS for
///                            short names with no compression state).
///   buf[1]                = suffix_len (1 byte, no XMIT_LONG_NAME for
///                            names <= 255 bytes).
///   buf[2..2+suffix_len]  = name bytes (post-iconv on the sender).
fn locate_name(buf: &[u8]) -> (usize, usize) {
    assert!(buf.len() >= 2, "buffer too short to contain name header");
    let suffix_len = buf[1] as usize;
    assert!(
        buf.len() >= 2 + suffix_len,
        "buffer truncated mid-name: have {}, need {}",
        buf.len(),
        2 + suffix_len
    );
    (2, suffix_len)
}

/// Encodes a single entry on the sender, then returns the raw wire bytes
/// of just the name field.
fn sender_name_on_wire(writer: &mut FileListWriter, entry: &FileEntry) -> Vec<u8> {
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, entry).expect("write_entry");
    let (off, len) = locate_name(&buf);
    buf[off..off + len].to_vec()
}

// ===========================================================================
// Section 1: Sender - UTF-8 source name -> remote charset wire bytes
// ===========================================================================

/// `café` (UTF-8: 63 61 66 c3 a9, 5 bytes) emitted by a sender configured
/// for `--iconv=.,ISO-8859-1` must hit the wire as ISO-8859-1: 63 61 66 e9
/// (4 bytes). This is the canonical iconv test case from upstream's docs.
///
/// upstream: flist.c:1580-1602 - iconvbufs(ic_send, ...) on file->basename
/// before it is appended to the lastname-shared prefix and emitted by the
/// XMIT_SAME_NAME / suffix_len machinery.
#[test]
fn golden_sender_utf8_to_latin1_cafe() {
    let mut writer = sender_writer("ISO-8859-1");
    let entry = entry_from_local("café");

    let name_on_wire = sender_name_on_wire(&mut writer, &entry);

    // ISO-8859-1 representation of "café".
    assert_eq!(name_on_wire, vec![0x63, 0x61, 0x66, 0xe9]);
}

/// Cyrillic `файл` (UTF-8: d1 84 d0 b0 d0 b9 d0 bb, 8 bytes) emitted to a
/// KOI8-R-configured remote must use the KOI8-R encoding: c6 c1 ca cc
/// (4 bytes - one byte per Cyrillic glyph). KOI8-R places lowercase
/// Cyrillic in 0xc0-0xdf and uppercase in 0xe0-0xff.
#[test]
fn golden_sender_utf8_to_koi8r_cyrillic() {
    let mut writer = sender_writer("KOI8-R");
    let entry = entry_from_local("файл");

    let name_on_wire = sender_name_on_wire(&mut writer, &entry);

    // KOI8-R lowercase: ф=0xc6, а=0xc1, й=0xca, л=0xcc.
    assert_eq!(name_on_wire, vec![0xc6, 0xc1, 0xca, 0xcc]);
}

/// Western European mixed-script "Ångström.txt" round-trips through
/// Windows-1252. Verifies that the wire bytes are the exact CP1252 encoding
/// (Å=0xc5, ö=0xf6) and that the suffix_len header reflects the post-iconv
/// length, not the source UTF-8 length.
#[test]
fn golden_sender_utf8_to_windows1252_angstrom() {
    let mut writer = sender_writer("windows-1252");
    let entry = entry_from_local("Ångström.txt");

    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    // suffix_len header must reflect 12 CP1252 bytes, not the 14-byte UTF-8
    // source length.
    assert_eq!(
        buf[1] as usize, 12,
        "suffix_len must be the post-iconv byte length"
    );

    let (off, len) = locate_name(&buf);
    let name_on_wire = &buf[off..off + len];
    assert_eq!(
        name_on_wire,
        &[
            0xc5, b'n', b'g', b's', b't', b'r', 0xf6, b'm', b'.', b't', b'x', b't'
        ]
    );
}

/// ASCII-only filenames must produce identical wire bytes regardless of
/// whether iconv is configured. The conversion path is exercised, but the
/// output must equal the input byte-for-byte.
#[test]
fn golden_sender_ascii_passthrough_under_iconv() {
    let mut iconv_writer = sender_writer("ISO-8859-1");
    let mut plain_writer = FileListWriter::new(proto());

    let entry = entry_from_local("plain.txt");

    let with_iconv = sender_name_on_wire(&mut iconv_writer, &entry);
    let without = sender_name_on_wire(&mut plain_writer, &entry);

    assert_eq!(with_iconv, b"plain.txt".to_vec());
    assert_eq!(with_iconv, without);
}

/// Identity converter (UTF-8 <-> UTF-8) is detected as is_identity() and the
/// wire bytes must equal the raw UTF-8 source - no double-encoding, no
/// allocation churn changing byte values.
#[test]
fn golden_sender_identity_converter_preserves_utf8() {
    let conv = FilenameConverter::identity();
    assert!(conv.is_identity());

    let mut writer = FileListWriter::new(proto()).with_iconv(conv);
    let entry = entry_from_local("café");

    let name_on_wire = sender_name_on_wire(&mut writer, &entry);

    // Raw UTF-8 bytes for "café".
    assert_eq!(name_on_wire, vec![0x63, 0x61, 0x66, 0xc3, 0xa9]);
}

/// A filename containing characters that ISO-8859-1 cannot represent (here:
/// the Greek letter alpha, U+03B1) must cause the sender to surface a
/// conversion error rather than silently transliterating or producing
/// truncated bytes. This mirrors upstream's `convert_error` path
/// (flist.c:1596-1602) which sets `IOERR_GENERAL` and rejects the name.
#[test]
fn golden_sender_unmappable_char_fails_conversion() {
    let mut writer = sender_writer("ISO-8859-1");
    let entry = entry_from_local("alpha-α.txt");

    let mut buf = Vec::new();
    let result = writer.write_entry(&mut buf, &entry);

    assert!(
        result.is_err(),
        "ISO-8859-1 cannot represent U+03B1 - sender must surface an error"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.to_string()
            .contains("filename encoding conversion failed"),
        "error message must identify the iconv failure: got {err}"
    );
}

// ===========================================================================
// Section 2: Receiver - remote charset wire bytes -> UTF-8 local name
// ===========================================================================

/// Builds a synthetic single-entry wire stream whose name field already
/// contains the given remote-charset bytes, then drives it through the
/// receiver. This bypasses the sender and lets us assert exactly the bytes
/// that arrive over a remote rsync's connection.
#[cfg(unix)]
fn build_wire_with_name(name_bytes: &[u8]) -> Vec<u8> {
    // Use a writer with NO iconv so it emits raw bytes verbatim. We feed the
    // remote-charset bytes through entry_from_remote_bytes so the writer's
    // own encoding pipeline does not touch them.
    let entry = entry_from_remote_bytes(name_bytes);

    let mut writer = FileListWriter::new(proto());
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).expect("write_entry");
    writer.write_end(&mut buf, None).expect("write_end");

    // Sanity-check that the writer emitted the bytes we expect (i.e. it did
    // not transform them). This protects the test from confusing failures
    // when the writer's wire layout changes.
    let (off, len) = locate_name(&buf);
    assert_eq!(
        &buf[off..off + len],
        name_bytes,
        "writer must emit the raw remote-charset bytes verbatim"
    );

    buf
}

/// ISO-8859-1 bytes `63 61 66 e9` ("café") arriving over the wire must be
/// converted by an iconv-enabled receiver into UTF-8 `63 61 66 c3 a9`.
///
/// upstream: flist.c:738-753 - ic_recv invoked on thisname after the raw
/// bytes have been read into the buffer, before clean_fname runs.
#[cfg(unix)]
#[test]
fn golden_receiver_latin1_to_utf8_cafe() {
    let wire = build_wire_with_name(&[0x63, 0x61, 0x66, 0xe9]);

    let mut reader = receiver_reader("ISO-8859-1");
    let mut cursor = Cursor::new(&wire[..]);

    let entry = reader
        .read_entry(&mut cursor)
        .expect("read_entry")
        .expect("entry must be present");

    // The entry's raw bytes (post-iconv) must be UTF-8 "café".
    assert_eq!(&*entry.name_bytes(), "café".as_bytes());
}

/// KOI8-R bytes `c6 c1 ca cc` (lowercase "файл") arriving on the wire must
/// round-trip to UTF-8 `d1 84 d0 b0 d0 b9 d0 bb`.
#[cfg(unix)]
#[test]
fn golden_receiver_koi8r_to_utf8_cyrillic() {
    let wire = build_wire_with_name(&[0xc6, 0xc1, 0xca, 0xcc]);

    let mut reader = receiver_reader("KOI8-R");
    let mut cursor = Cursor::new(&wire[..]);

    let entry = reader
        .read_entry(&mut cursor)
        .expect("read_entry")
        .expect("entry must be present");

    assert_eq!(&*entry.name_bytes(), "файл".as_bytes());
}

/// Windows-1252 bytes for "Ångström.txt" arrive as 12 bytes on the wire and
/// must surface as the 14-byte UTF-8 form on the receiver.
#[cfg(unix)]
#[test]
fn golden_receiver_windows1252_to_utf8_angstrom() {
    let wire = build_wire_with_name(&[
        0xc5, b'n', b'g', b's', b't', b'r', 0xf6, b'm', b'.', b't', b'x', b't',
    ]);

    let mut reader = receiver_reader("windows-1252");
    let mut cursor = Cursor::new(&wire[..]);

    let entry = reader
        .read_entry(&mut cursor)
        .expect("read_entry")
        .expect("entry must be present");

    assert_eq!(&*entry.name_bytes(), "Ångström.txt".as_bytes());
}

/// ASCII-only wire bytes pass through receiver iconv unchanged. The byte
/// sequence emitted by the writer must equal the bytes surfaced by the
/// reader.
#[cfg(unix)]
#[test]
fn golden_receiver_ascii_passthrough_under_iconv() {
    let wire = build_wire_with_name(b"plain.txt");

    let mut reader = receiver_reader("ISO-8859-1");
    let mut cursor = Cursor::new(&wire[..]);

    let entry = reader
        .read_entry(&mut cursor)
        .expect("read_entry")
        .expect("entry must be present");

    assert_eq!(&*entry.name_bytes(), b"plain.txt");
}

/// Wire bytes that are invalid in the declared remote charset must be
/// rejected with an InvalidData error. UTF-8 strictly rejects an isolated
/// continuation byte (0x80 with no leading byte) and bare lead bytes
/// without continuations - both encoding_rs and upstream iconv flag these.
/// This mirrors upstream's `IOERR_GENERAL` flag at flist.c:746 when
/// iconvbufs() returns < 0 on the receiver.
#[cfg(unix)]
#[test]
fn golden_receiver_invalid_remote_bytes_fail() {
    // [0xc3, 0x28] is invalid UTF-8: c3 begins a 2-byte sequence but 28 is
    // not a valid continuation byte (continuations must be 80-bf).
    let wire = build_wire_with_name(&[0xc3, 0x28]);

    // local=ISO-8859-1, remote=UTF-8 - a non-identity converter that makes
    // the receiver actually run UTF-8 decode on the wire bytes.
    let conv = FilenameConverter::new("ISO-8859-1", "UTF-8")
        .expect("ISO-8859-1 + UTF-8 must be supported");
    assert!(!conv.is_identity());
    let mut reader = FileListReader::new(proto()).with_iconv(conv);

    let mut cursor = Cursor::new(&wire[..]);
    let result = reader.read_entry(&mut cursor);

    assert!(
        result.is_err(),
        "[0xc3, 0x28] is invalid UTF-8 - receiver must surface an error"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.to_string()
            .contains("filename encoding conversion failed"),
        "error message must identify the iconv failure: got {err}"
    );
}

// ===========================================================================
// Section 3: Round-trip - sender + receiver yields identical local names
// ===========================================================================

/// Drives an entry through sender (UTF-8 -> remote) and then receiver
/// (remote -> UTF-8) and asserts the recovered name equals the original.
#[cfg(unix)]
fn round_trip(name: &str, remote_charset: &str) -> String {
    let mut writer = sender_writer(remote_charset);
    let mut reader = receiver_reader(remote_charset);

    let entry = entry_from_local(name);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).expect("write_entry");
    writer.write_end(&mut buf, None).expect("write_end");

    let mut cursor = Cursor::new(&buf[..]);
    let recovered = reader
        .read_entry(&mut cursor)
        .expect("read_entry")
        .expect("entry must be present");

    String::from_utf8(recovered.name_bytes().into_owned())
        .expect("receiver must produce valid UTF-8 for representable inputs")
}

/// Round-trip preserves "café" through UTF-8 -> ISO-8859-1 -> UTF-8.
#[cfg(unix)]
#[test]
fn golden_round_trip_cafe_via_latin1() {
    let recovered = round_trip("café", "ISO-8859-1");
    assert_eq!(recovered, "café");
}

/// Round-trip preserves Cyrillic content via KOI8-R.
#[cfg(unix)]
#[test]
fn golden_round_trip_cyrillic_via_koi8r() {
    let recovered = round_trip("файл", "KOI8-R");
    assert_eq!(recovered, "файл");
}

/// Round-trip preserves the full 12-byte CP1252 form of "Ångström.txt".
#[cfg(unix)]
#[test]
fn golden_round_trip_angstrom_via_windows1252() {
    let recovered = round_trip("Ångström.txt", "windows-1252");
    assert_eq!(recovered, "Ångström.txt");
}

/// Round-trip preserves a directory-prefixed name. The XMIT_SAME_NAME
/// compression path (which shares a byte-prefix with the previous entry) is
/// applied AFTER iconv conversion - the prefix bytes compared on both sides
/// are the post-iconv bytes - so this also verifies the sender and receiver
/// agree on the iconv-then-compress order.
#[cfg(unix)]
#[test]
fn golden_round_trip_dir_then_file_compressed_after_iconv() {
    let mut writer = sender_writer("ISO-8859-1");
    let mut reader = receiver_reader("ISO-8859-1");

    let entry1 = entry_from_local("café/one.txt");
    let entry2 = entry_from_local("café/two.txt");

    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry1).expect("write 1");
    writer.write_entry(&mut buf, &entry2).expect("write 2");
    writer.write_end(&mut buf, None).expect("write_end");

    let mut cursor = Cursor::new(&buf[..]);
    let r1 = reader
        .read_entry(&mut cursor)
        .expect("read 1")
        .expect("entry 1");
    let r2 = reader
        .read_entry(&mut cursor)
        .expect("read 2")
        .expect("entry 2");

    assert_eq!(&*r1.name_bytes(), "café/one.txt".as_bytes());
    assert_eq!(&*r2.name_bytes(), "café/two.txt".as_bytes());
}

// ===========================================================================
// Section 4: Sender and receiver wire bytes must match exactly
// ===========================================================================

/// What the iconv-configured sender writes for "café" must equal what an
/// ISO-8859-1 raw-bytes wire stream would carry. This locks the sender's
/// output format against any drift in upstream's wire encoding for converted
/// filenames.
#[cfg(unix)]
#[test]
fn golden_sender_wire_equals_raw_remote_bytes_for_cafe() {
    let mut iconv_writer = sender_writer("ISO-8859-1");
    let utf8_entry = entry_from_local("café");

    let mut iconv_buf = Vec::new();
    iconv_writer
        .write_entry(&mut iconv_buf, &utf8_entry)
        .expect("write");
    iconv_writer.write_end(&mut iconv_buf, None).expect("end");

    // Reference: the same wire bytes a non-iconv writer would emit if it
    // received an entry whose raw name was already in ISO-8859-1.
    let raw_reference = build_wire_with_name(&[0x63, 0x61, 0x66, 0xe9]);

    assert_eq!(
        iconv_buf, raw_reference,
        "sender + iconv must produce byte-identical output to an entry \
         whose raw name is already in the remote charset"
    );
}

/// Symmetrically: what the iconv-configured receiver decodes from raw
/// ISO-8859-1 wire bytes must equal what an iconv-disabled receiver would
/// see after the bytes were re-encoded into UTF-8 on the wire.
#[cfg(unix)]
#[test]
fn golden_receiver_post_iconv_equals_utf8_native_decode() {
    // Drive remote bytes through the iconv receiver.
    let remote_wire = build_wire_with_name(&[0x63, 0x61, 0x66, 0xe9]);
    let mut iconv_reader = receiver_reader("ISO-8859-1");
    let mut cursor = Cursor::new(&remote_wire[..]);
    let from_iconv = iconv_reader
        .read_entry(&mut cursor)
        .expect("iconv read")
        .expect("entry");

    // Drive the equivalent UTF-8 bytes through a plain receiver.
    let utf8_wire = build_wire_with_name("café".as_bytes());
    let mut plain_reader = FileListReader::new(proto());
    let mut cursor = Cursor::new(&utf8_wire[..]);
    let from_plain = plain_reader
        .read_entry(&mut cursor)
        .expect("plain read")
        .expect("entry");

    assert_eq!(from_iconv.name_bytes(), from_plain.name_bytes());
}

// ===========================================================================
// Section 5: suffix_len header reflects post-iconv length
// ===========================================================================

/// `café` is 5 bytes in UTF-8 but 4 bytes in ISO-8859-1. The wire's
/// suffix_len byte must be 4, not 5 - i.e. the writer measures the name
/// AFTER iconv conversion.
#[test]
fn golden_sender_suffix_len_is_post_iconv_for_shrinking_conversion() {
    let mut writer = sender_writer("ISO-8859-1");
    let entry = entry_from_local("café");

    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    assert_eq!(
        buf[1], 4,
        "suffix_len must reflect the 4-byte ISO-8859-1 form, not the \
         5-byte UTF-8 source"
    );
}

/// Cyrillic `файл` shrinks from 8 UTF-8 bytes to 4 KOI8-R bytes. Same
/// invariant - suffix_len must be the post-iconv length.
#[test]
fn golden_sender_suffix_len_is_post_iconv_for_cyrillic() {
    let mut writer = sender_writer("KOI8-R");
    let entry = entry_from_local("файл");

    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    assert_eq!(
        buf[1], 4,
        "suffix_len must reflect the 4-byte KOI8-R form, not the 8-byte \
         UTF-8 source"
    );
}
