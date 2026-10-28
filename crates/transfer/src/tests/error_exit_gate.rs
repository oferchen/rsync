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
        announce_error_exit(&mut writer, error, protocol_version, true, None);
    }
    wire
}

/// upstream: cleanup.c:244-253 + io.c:1066-1075 - a sender queues the frame
/// but only a socket daemon's `noop_io_until_death()` drains it, so over a
/// remote shell nothing reaches the peer and it sees the connection close.
#[test]
fn a_sender_announces_only_over_a_daemon_socket() {
    use crate::{ServerRole, error_exit_reaches_peer};
    assert!(!error_exit_reaches_peer(ServerRole::Generator, false));
    assert!(error_exit_reaches_peer(ServerRole::Generator, true));
    assert!(error_exit_reaches_peer(ServerRole::Receiver, false));
    assert!(error_exit_reaches_peer(ServerRole::Receiver, true));
    let error = io::Error::from(io::ErrorKind::PermissionDenied);
    let mut wire = Vec::new();
    {
        let mut writer = ServerWriter::new_plain(&mut wire)
            .activate_multiplex()
            .expect("activate multiplex");
        announce_error_exit(&mut writer, &error, 32, false, None);
    }
    assert!(wire.is_empty(), "an unreachable frame is not written");
}

/// upstream: log.c:357-367 rwrite() - a socket daemon's fatal cause and
/// log_exit() trailer are MSG_ERROR frames queued before the
/// `MSG_ERROR_EXIT` that exit_cleanup() sends after them.
#[test]
fn a_report_precedes_the_exit_frame_as_error_messages() {
    let error = io::Error::from(io::ErrorKind::PermissionDenied);
    let report = |_: &io::Error, code: i32| {
        vec![
            b"cause\n".to_vec(),
            format!("trailer {code}\n").into_bytes(),
        ]
    };
    let mut wire = Vec::new();
    {
        let mut writer = ServerWriter::new_plain(&mut wire)
            .activate_multiplex()
            .expect("activate multiplex");
        announce_error_exit(&mut writer, &error, 32, true, Some(&report));
    }
    let frame = |payload: &[u8]| {
        let mut out = vec![
            payload.len() as u8,
            0,
            0,
            MPLEX_BASE + MessageCode::Error.as_u8(),
        ];
        out.extend_from_slice(payload);
        out
    };
    let mut expected = frame(b"cause\n");
    expected.extend(frame(b"trailer 3\n"));
    expected.extend(expected_frame(3));
    assert_eq!(wire, expected);
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

/// upstream: io.c:1930 - receipt of `MSG_ERROR_EXIT` re-enters `_exit_cleanup`
/// with a negative line precisely so the code is not echoed straight back.
#[test]
fn an_exit_the_peer_asked_for_is_not_echoed_back() {
    let error = io::Error::other(RemoteExitError { code: 3 });
    assert!(emitted(&error, 32).is_empty());
}
