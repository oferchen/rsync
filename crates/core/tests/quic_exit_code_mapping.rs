#![cfg(feature = "quic")]
//! Forced-failure exit-code mapping for the QUIC transport.
//!
//! `rsync_io::quic` classifies each `quinn-proto` transport failure into an
//! `io::Error` whose `ErrorKind` - and, for protocol failures, its
//! `protocol::ProtocolViolation` tag - routes through
//! `core::ExitCode::from_io_error` to a parity exit code. `rsync_io` sits below
//! `core` in the crate graph, so its own unit tests can only assert the
//! selected `ErrorKind`/tag; they cannot name `ExitCode`. These tests close
//! that loop: each one forces a distinct failure class and asserts the EXACT
//! `ExitCode` the composed mapping yields, pinning the parity table documented
//! in `crates/rsync_io/src/quic/error.rs`.
//!
//! No live QUIC endpoint is involved. Every failure is synthesized directly as
//! the `quinn-proto` error the driver would surface, then run through the real
//! classifier and the real `ExitCode::from_io_error` - so a regression in
//! either half is caught here.

use core::exit_code::ExitCode;
use std::io;
use std::net::SocketAddr;

use rsync_io::quic::{
    ConnectError, ConnectionError, TransportError, TransportErrorCode, VarInt, connect_fault,
    connection_fault, driver_gone, io_fault, stream_reset,
};

/// Composes the two real halves of the mapping under test: the `rsync_io`
/// classifier output and `core::ExitCode::from_io_error`.
fn exit_code_of(err: &io::Error) -> ExitCode {
    ExitCode::from_io_error(err)
}

/// A peer transport error carrying a `CRYPTO_ERROR` (TLS alert) code - the
/// exact shape a rejected certificate-trust handshake takes, since `quinn-proto`
/// has no dedicated cert-failure variant.
fn crypto_transport_error() -> ConnectionError {
    ConnectionError::TransportError(TransportError {
        code: TransportErrorCode::crypto(0x2a),
        frame: None,
        reason: "the handshake certificate was not trusted".to_owned(),
    })
}

/// WHY: an idle or handshake timeout must exit `Timeout` (30) - the same code a
/// TCP read stall reaches - so a wedged QUIC peer is reported as a timeout, not
/// misclassified as a generic file or socket error.
#[test]
fn idle_or_handshake_timeout_exits_timeout() {
    let err = connection_fault(&ConnectionError::TimedOut).to_io_error();
    assert_eq!(exit_code_of(&err), ExitCode::Timeout);
}

/// WHY: a peer connection reset and a peer stream reset are both socket-level
/// losses and must exit `SocketIo` (10), in parity with a TCP `ConnectionReset`.
#[test]
fn peer_and_stream_reset_exit_socket_io() {
    let conn = connection_fault(&ConnectionError::Reset).to_io_error();
    let stream = stream_reset(VarInt::from_u32(7)).to_io_error();
    assert_eq!(exit_code_of(&conn), ExitCode::SocketIo);
    assert_eq!(exit_code_of(&stream), ExitCode::SocketIo);
}

/// WHY: exhausting connection IDs drops the connection and must exit `SocketIo`
/// (10), never fall through to the default `FileIo` (11) bucket an
/// unclassified kind would land in.
#[test]
fn cids_exhausted_exits_socket_io() {
    let err = connection_fault(&ConnectionError::CidsExhausted).to_io_error();
    assert_eq!(exit_code_of(&err), ExitCode::SocketIo);
}

/// WHY: the driver thread exiting without recording a terminal leaves the
/// transport pipe broken; that `BrokenPipe` must exit `SocketIo` (10).
#[test]
fn driver_gone_exits_socket_io() {
    let err = driver_gone("driver exited mid-stream");
    assert_eq!(exit_code_of(&err), ExitCode::SocketIo);
}

/// WHY: QUIC version negotiation failure is a protocol incompatibility and must
/// exit `Protocol` (2), carried by the `ProtocolViolation` tag rather than
/// degrading to `StreamIo` (12) as a bare `InvalidData` would.
#[test]
fn version_mismatch_exits_protocol() {
    let err = connection_fault(&ConnectionError::VersionMismatch).to_io_error();
    assert_eq!(exit_code_of(&err), ExitCode::Protocol);
}

/// WHY: a certificate-trust / TLS handshake rejection arrives as a
/// `TransportError` with a `CRYPTO_ERROR` code. The shared `io::Error` bridge
/// cannot express `StartClient` (5), so a rejected secure handshake must exit
/// `Protocol` (2) like every other handshake failure. This pins that documented
/// decision end-to-end.
#[test]
fn cert_trust_or_tls_failure_exits_protocol() {
    let err = connection_fault(&crypto_transport_error()).to_io_error();
    assert_eq!(exit_code_of(&err), ExitCode::Protocol);
}

/// WHY: a connection that cannot even be initiated is a connection failure and
/// must exit `SocketIo` (10) - whether the peer address is unusable or the
/// local endpoint is shutting down.
#[test]
fn connect_setup_failures_exit_socket_io() {
    let bad_addr = connect_fault(&ConnectError::InvalidRemoteAddress(
        "0.0.0.0:0".parse::<SocketAddr>().expect("parse addr"),
    ));
    let stopping = connect_fault(&ConnectError::EndpointStopping);
    assert_eq!(exit_code_of(&bad_addr), ExitCode::SocketIo);
    assert_eq!(exit_code_of(&stopping), ExitCode::SocketIo);
}

/// WHY: a QUIC version the local endpoint cannot speak is a protocol
/// incompatibility at connect time and must exit `Protocol` (2), splitting it
/// from the socket-level connect failures above.
#[test]
fn connect_unsupported_version_exits_protocol() {
    let err = connect_fault(&ConnectError::UnsupportedVersion);
    assert_eq!(exit_code_of(&err), ExitCode::Protocol);
}

/// WHY: a genuine local UDP-socket error must keep the OS-reported kind so it
/// classifies exactly as on any other transport. `PermissionDenied` is a
/// file-selection error, so it exits `FileSelect` (3) - proving the classifier
/// preserves the kind rather than flattening every fault into a socket or
/// protocol bucket.
#[test]
fn local_socket_failure_preserves_its_exit_code() {
    let original = io::Error::new(io::ErrorKind::PermissionDenied, "sendto: EPERM");
    let err = io_fault(&original).to_io_error();
    assert_eq!(exit_code_of(&err), ExitCode::FileSelect);
}
