//! Regression coverage for the receiver-side `--iconv` ordering invariant.
//!
//! When the receiver has an active `FilenameConverter`, the NDX-addressed
//! `file_list` array must remain in sender wire-emit order, not be reshuffled
//! into local-charset byte order. Re-sorting after iconv transcoding caused
//! "received request to transfer non-regular file" aborts on pulls from
//! upstream daemons whenever the converted bytes sorted differently from the
//! wire bytes.
//!
//! # Upstream Reference
//!
//! - `options.c:2069-2074` - sets `need_unsorted_flist = 1` when `--iconv`
//!   is in effect (and not disabled by `--iconv=-`).
//! - `flist.c:2496-2498` - "both sides keep an unsorted file-list array
//!   because the names will differ on the sending and receiving sides".
//! - `flist.c:2184-2188` - allocates a separate `flist->sorted[]` pointer
//!   array so `flist->files[]` (NDX-addressed) stays in scan order.

use std::ffi::OsString;
use std::io::Cursor;

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListWriter};

use super::super::super::ReceiverContext;
use super::super::support::test_handshake;
use crate::config::{ConnectionConfig, ServerConfig};
use crate::role::ServerRole;

/// Wire bytes for the regression scenario: three entries in scan order
/// where the receiver's natural file-before-directory comparator would
/// permute them. The sort permutation models what `sort_file_list` would
/// do to the NDX-addressed array, breaking subsequent generator requests.
fn build_wire_bytes(entries: &[FileEntry]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(ProtocolVersion::try_from(32u8).unwrap());
    for entry in entries {
        writer.write_entry(&mut data, entry).unwrap();
    }
    writer.write_end(&mut data, None).unwrap();
    data
}

fn build_iconv_config() -> ServerConfig {
    let converter = protocol::iconv::FilenameConverter::new("UTF-8", "ISO-8859-1")
        .expect("UTF-8/LATIN1 converter must construct on every platform");
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.iLsf".to_owned(),
        args: vec![OsString::from(".")],
        connection: ConnectionConfig {
            iconv: Some(converter),
            ..ConnectionConfig::default()
        },
        ..Default::default()
    }
}

fn build_no_iconv_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    }
}

/// When iconv is active, the receiver must preserve sender wire-emit order
/// in `file_list`. Otherwise the receiver re-sorts and subsequent generator
/// requests resolve to the wrong entry (e.g., asking to transfer a directory
/// at an index that should point to a regular file).
#[test]
fn iconv_active_preserves_wire_order_for_ndx_lookup() {
    // Sender scan order: directory, file, file-in-directory. Under the
    // default protocol-29+ comparator that buckets files before directories,
    // `sort_file_list` would reorder this to ["zebra.txt", "alpha",
    // "alpha/inner.txt"], breaking the wire NDX -> flat-index identity.
    let wire_entries = vec![
        FileEntry::new_directory("alpha".into(), 0o755),
        FileEntry::new_file("zebra.txt".into(), 42, 0o644),
        FileEntry::new_file("alpha/inner.txt".into(), 7, 0o644),
    ];
    let data = build_wire_bytes(&wire_entries);

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, build_iconv_config());
    let mut cursor = Cursor::new(data);
    let count = ctx.receive_file_list(&mut cursor).unwrap();

    assert_eq!(count, 3);
    let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
    assert_eq!(
        names,
        vec!["alpha", "zebra.txt", "alpha/inner.txt"],
        "iconv-active receiver must keep entries in sender wire-emit order \
         so generator NDX requests resolve to the right entry"
    );
}

/// Without iconv, the receiver still sorts file_list to match the sender
/// (sender wire-emit was unsorted scan order; both sides re-sort by the same
/// comparator on the same bytes so the result matches). This pins the
/// non-iconv path unchanged so the iconv fix does not regress it.
#[test]
fn no_iconv_path_still_sorts_entries() {
    let wire_entries = vec![
        FileEntry::new_directory("alpha".into(), 0o755),
        FileEntry::new_file("zebra.txt".into(), 42, 0o644),
        FileEntry::new_file("alpha/inner.txt".into(), 7, 0o644),
    ];
    let data = build_wire_bytes(&wire_entries);

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, build_no_iconv_config());
    let mut cursor = Cursor::new(data);
    let count = ctx.receive_file_list(&mut cursor).unwrap();

    assert_eq!(count, 3);
    let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
    // protocol-29+ comparator: files before directories at the same level,
    // then directory contents follow the directory entry.
    assert_eq!(
        names,
        vec!["zebra.txt", "alpha", "alpha/inner.txt"],
        "non-iconv receiver must keep the existing sort so the NDX lookup \
         continues to match the upstream sender's sorted view"
    );
}

/// An iconv converter whose local and remote encodings are identical
/// performs no transcoding, so the NDX-addressed array cannot diverge
/// from the sender's wire view. Mirrors upstream's check at
/// `options.c:2071` that nulls out `iconv_opt` on `--iconv=-`, leaving
/// `need_unsorted_flist` unset.
#[test]
fn identity_iconv_does_not_suppress_reorder() {
    let identity = protocol::iconv::FilenameConverter::identity();
    let mut config = build_no_iconv_config();
    config.connection.iconv = Some(identity);

    let wire_entries = vec![
        FileEntry::new_directory("alpha".into(), 0o755),
        FileEntry::new_file("zebra.txt".into(), 42, 0o644),
        FileEntry::new_file("alpha/inner.txt".into(), 7, 0o644),
    ];
    let data = build_wire_bytes(&wire_entries);

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut cursor = Cursor::new(data);
    let count = ctx.receive_file_list(&mut cursor).unwrap();

    assert_eq!(count, 3);
    let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
    assert_eq!(
        names,
        vec!["zebra.txt", "alpha", "alpha/inner.txt"],
        "identity iconv must behave like no iconv: the sort still runs"
    );
}
