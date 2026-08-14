//! Protocol 28/29 trailing `io_error` int after the file-list end marker.
//!
//! upstream: flist.c:2773-2777 - the sender writes `write_int(f, io_error)`
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
        ctx.flist_reader_io_error(),
        io_error_value,
        "io_error should be read from wire and reach the value the drivers use"
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
    assert_eq!(
        ctx.flist_reader_io_error(),
        0,
        "zero io_error should not set field"
    );
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
    assert_eq!(ctx.flist_reader_io_error(), 0);
}

/// The pre-30 trailer is the third place a peer hands us an `io_error`, and it
/// is decoded outside the file-list reader - so it needs its own mask. A value
/// of purely undefined bits must accumulate to nothing, and defined bits must
/// still survive.
///
/// upstream: flist.c:3068-3072 - `io_error |= err & IOERR_VALID_MASK`.
#[test]
fn receive_file_list_masks_hostile_io_error_for_proto28() {
    for (wire_value, expected) in [
        (0x7fff_fff8, 0),
        (0x0100_0002, protocol::IOERR_VANISHED),
        (-1, protocol::IOERR_VALID_MASK),
    ] {
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

        let mut wire = vec![0x00u8];
        wire.extend_from_slice(&i32::to_le_bytes(wire_value));

        let mut cursor = Cursor::new(wire);
        assert_eq!(ctx.receive_file_list(&mut cursor).unwrap(), 0);
        assert_eq!(
            ctx.flist_reader_io_error(),
            expected,
            "wire value {wire_value:#x} must be masked to the defined IOERR_* bits"
        );
    }
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
        ctx.flist_reader_io_error(),
        0,
        "ignore_errors should suppress io_error accumulation"
    );
}

/// `--ignore-errors` must suppress a PEER-supplied file-list trailer, and must
/// NOT suppress an error this receiver generated itself.
///
/// upstream keeps the two apart: `flist.c:2949`, `:2967` and `:3070` accumulate
/// the peer's trailer only `if (!ignore_errors)`, while `flist.c:841`'s
/// filename-transcode failure is accumulated with no such check.
///
/// This asserts the value the four transfer drivers actually read
/// (`TransferStats::io_error` is built from `flist_reader_io_error()`), not an
/// intermediate field. Before the peer/local split those drivers bypassed the
/// gate entirely: the value was filtered out of `flist_io_error` and then
/// re-admitted through the reader cache, so `--ignore-errors` did not suppress
/// it and the run still exited 23.
#[test]
fn ignore_errors_gates_the_peer_trailer_only() {
    for &(ignore_errors, want) in &[(false, 3), (true, 0)] {
        let handshake = test_handshake_with_protocol(28);
        let mut config = ServerConfig {
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
        config.deletion.ignore_errors = ignore_errors;
        let mut ctx = ReceiverContext::new_for_test(&handshake, config);

        let mut wire = vec![0x00u8];
        wire.extend_from_slice(&3i32.to_le_bytes());
        let mut cursor = Cursor::new(wire);
        ctx.receive_file_list(&mut cursor).unwrap();

        assert_eq!(
            ctx.flist_reader_io_error(),
            want,
            "peer trailer with --ignore-errors={ignore_errors}"
        );
    }
}

/// Decodes a peer-supplied file-list `io_error` at `protocol` and returns the
/// value the transfer drivers read, so the two protocol eras can be compared
/// on identical inputs.
///
/// Pre-30 carries the value as a fixed 4-byte LE trailer after the end marker
/// (`flist.c:3068-3072`); 30+ carries it in the end marker itself
/// (`flist.c:2960-2970`), which the file-list writer emits.
fn decode_peer_io_error(protocol: u8, value: i32, ignore_errors: bool) -> i32 {
    let handshake = test_handshake_with_protocol(protocol);
    let mut config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(protocol).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: NumericIds::Explicit,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    config.deletion.ignore_errors = ignore_errors;
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    let mut wire = Vec::new();
    if protocol < 30 {
        wire.push(0x00);
        wire.extend_from_slice(&value.to_le_bytes());
    } else {
        protocol::flist::FileListWriter::new(ctx.protocol())
            .write_end(&mut wire, Some(value))
            .unwrap();
    }

    let mut cursor = Cursor::new(wire);
    ctx.receive_file_list(&mut cursor).unwrap();
    ctx.flist_reader_io_error()
}

/// CROSS-IMPLEMENTATION: oc decodes the peer's file-list `io_error` in two
/// independent places - the pre-30 fixed trailer in `file_list/receive.rs` and
/// the 30+ end marker in `protocol::flist::read`. Upstream applies ONE rule to
/// both: `flist.c:2949`, `:2967` and `:3070` are three copies of
/// `if (!ignore_errors) io_error |= err & IOERR_VALID_MASK`.
///
/// So the two eras must agree cell for cell. The pre-30 decoder was already
/// correct, which makes it a free oracle for the 30+ one - no external binary
/// is needed, and the assertion keeps holding if either decoder is touched.
/// Pinning each era against its own hand-written expectation would let them
/// drift apart again without any test noticing.
#[test]
fn both_protocol_eras_apply_the_same_peer_trailer_rule() {
    for &ignore_errors in &[false, true] {
        let legacy = decode_peer_io_error(29, 3, ignore_errors);
        let modern = decode_peer_io_error(32, 3, ignore_errors);
        assert_eq!(
            legacy, modern,
            "protocol 29 and 32 must decode the same peer io_error \
             (--ignore-errors={ignore_errors})"
        );
    }
}

/// The masking half of the rule must also hold across both eras: upstream
/// applies `& IOERR_VALID_MASK` at every accumulation site, so an undefined
/// bit set by a hostile peer must vanish identically in each decoder.
#[test]
fn both_protocol_eras_mask_the_peer_trailer_identically() {
    for &value in &[0x7fff_fff8u32 as i32, 0x0100_0002, -1, 7] {
        assert_eq!(
            decode_peer_io_error(29, value, false),
            decode_peer_io_error(32, value, false),
            "protocol 29 and 32 must mask wire value {value:#x} identically"
        );
    }
}

/// A locally-generated file-list error survives `--ignore-errors`.
///
/// upstream: `flist.c:841` has no `ignore_errors` check, so a filename this
/// receiver could not transcode still exits 23. Before the split this was
/// folded in through the same gate as the peer trailer and was wrongly
/// suppressed.
#[test]
fn ignore_errors_does_not_suppress_a_local_error() {
    let handshake = test_handshake_with_protocol(28);
    let mut config = ServerConfig {
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
    config.deletion.ignore_errors = true;
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    let mut wire = vec![0x00u8];
    wire.extend_from_slice(&0i32.to_le_bytes());
    let mut cursor = Cursor::new(wire);
    ctx.receive_file_list(&mut cursor).unwrap();

    // The reader carries no local error in this stream, so the observable value
    // is zero; the guard is that the local half is combined UNGATED, which
    // `protocol::combine_flist_io_error` pins directly.
    assert_eq!(ctx.flist_reader_io_error(), 0);
    assert_eq!(
        protocol::combine_flist_io_error(0, protocol::IOERR_GENERAL, true),
        protocol::IOERR_GENERAL,
        "a local decode error is never gated on --ignore-errors"
    );
}
