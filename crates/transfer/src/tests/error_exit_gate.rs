//! Gate coverage for the `MSG_ERROR_EXIT` producer.
//!
//! upstream: cleanup.c:242-258 - `_exit_cleanup()` writes the exit code onto the
//! multiplexed stream only when the code is not one of the four transport-class
//! failures, the peer negotiated protocol 31 or later, and this exit was not
//! itself caused by a `MSG_ERROR_EXIT` the peer sent.

use std::io;

use protocol::{MPLEX_BASE, MessageCode};

use crate::RemoteExitError;
use crate::announce_error_exit;
use crate::writer::ServerWriter;

/// Runs the producer over a multiplexed writer and returns the bytes it emitted.
fn emitted(error: &io::Error, protocol_version: u8) -> Vec<u8> {
    let mut wire = Vec::new();
    {
        let mut writer = ServerWriter::new_plain(&mut wire)
            .activate_multiplex()
            .expect("activate multiplex");
        announce_error_exit(&mut writer, error, protocol_version);
    }
    wire
}

/// The frame upstream's `send_msg_int(MSG_ERROR_EXIT, code)` produces: the
/// tag/length header followed by the 4-byte little-endian code.
fn expected_frame(code: i32) -> Vec<u8> {
    let mut frame = Vec::with_capacity(8);
    frame.extend_from_slice(&[4, 0, 0, MPLEX_BASE + MessageCode::ErrorExit.as_u8()]);
    frame.extend_from_slice(&code.to_le_bytes());
    frame
}

#[test]
fn a_file_select_failure_is_announced_at_protocol_31() {
    let error = io::Error::from(io::ErrorKind::PermissionDenied);
    assert_eq!(emitted(&error, 31), expected_frame(3));
    assert_eq!(emitted(&error, 32), expected_frame(3));
}

/// upstream: cleanup.c:244 - below protocol 31 nothing is written to the wire;
/// the surviving `am_receiver` arm routes to the forked receiver's sibling
/// channel, which a single-process port does not have.
#[test]
fn protocol_30_announces_nothing() {
    let error = io::Error::from(io::ErrorKind::PermissionDenied);
    assert!(emitted(&error, 30).is_empty());
}

/// upstream: cleanup.c:242-243 - `RERR_SOCKETIO`, `RERR_STREAMIO`, `RERR_SIGNAL1`
/// and `RERR_TIMEOUT` are the transport-class codes; announcing one down the
/// transport that just failed is pointless, so upstream skips the send.
#[test]
fn transport_class_codes_are_not_announced() {
    for kind in [
        io::ErrorKind::ConnectionReset, // RERR_SOCKETIO
        io::ErrorKind::UnexpectedEof,   // RERR_STREAMIO
        io::ErrorKind::TimedOut,        // RERR_TIMEOUT
    ] {
        let error = io::Error::from(kind);
        assert!(
            emitted(&error, 32).is_empty(),
            "{kind:?} is transport-class and must not be announced"
        );
    }
}

/// upstream: io.c:1892 - receipt of `MSG_ERROR_EXIT` re-enters `_exit_cleanup`
/// with a negative line precisely so the code is not echoed straight back.
#[test]
fn an_exit_the_peer_asked_for_is_not_echoed_back() {
    let error = io::Error::other(RemoteExitError { code: 3 });
    assert!(emitted(&error, 32).is_empty());
}
