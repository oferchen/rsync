//! Protocol 28/29 trailing `io_error` int after the file-list end marker.
//!
//! upstream: flist.c:2738-2742 - the sender writes `write_int(f, io_error)`
//! after the id lists. Without this read, subsequent wire data is misaligned,
//! causing "received request to transfer non-regular file" errors.

use std::ffi::OsString;
use std::io::Cursor;

use protocol::ProtocolVersion;

use super::super::super::ReceiverContext;
use super::super::support::test_handshake_with_protocol;
use crate::config::ServerConfig;
use crate::flags::{NumericIds, ParsedServerFlags};
use crate::role::ServerRole;

/// Verifies that `receive_file_list` reads the 4-byte LE io_error flag
/// after the file list end marker for protocol < 30.
#[test]
fn receive_file_list_reads_io_error_for_proto28() {
    let handshake = test_handshake_with_protocol(28);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(28u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: NumericIds::Explicit,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Wire bytes: 0x00 end marker + 4-byte LE io_error (value 3 = IOERR_GENERAL | IOERR_DEL_LIMIT)
    let io_error_value: i32 = 3;
    let mut wire = vec![0x00u8]; // end marker
    wire.extend_from_slice(&io_error_value.to_le_bytes());

    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0, "empty file list should have 0 entries");
    assert_eq!(
        ctx.flist_io_error, io_error_value,
        "io_error should be read from wire"
    );
}

/// Verifies that `receive_file_list` reads io_error for protocol 29 (also < 30).
#[test]
fn receive_file_list_reads_io_error_for_proto29() {
    let handshake = test_handshake_with_protocol(29);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(29u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: NumericIds::Explicit,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Wire: end marker + io_error = 0 (no error)
    let mut wire = vec![0x00u8];
    wire.extend_from_slice(&0i32.to_le_bytes());

    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert_eq!(ctx.flist_io_error, 0, "zero io_error should not set field");
}

/// Verifies that protocol >= 30 does NOT read the 4-byte io_error (uses
/// MSG_IO_ERROR multiplexed frames instead).
#[test]
fn receive_file_list_skips_io_error_for_proto30() {
    let handshake = test_handshake_with_protocol(30);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(30u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: NumericIds::Explicit,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Wire: just end marker, no io_error bytes. If the code tried to read
    // 4 more bytes it would fail with UnexpectedEof.
    let wire = vec![0x00u8];
    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert_eq!(ctx.flist_io_error, 0);
}

/// Verifies that `ignore_errors` prevents accumulating the io_error flag.
#[test]
fn receive_file_list_ignore_errors_suppresses_io_error() {
    let handshake = test_handshake_with_protocol(28);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(28u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: NumericIds::Explicit,
            ..Default::default()
        },
        deletion: crate::config::DeletionConfig {
            ignore_errors: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Wire: end marker + io_error = 7
    let mut wire = vec![0x00u8];
    wire.extend_from_slice(&7i32.to_le_bytes());

    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        ctx.flist_io_error, 0,
        "ignore_errors should suppress io_error accumulation"
    );
}
