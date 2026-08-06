//! Classifies `quinn-proto` transport failures into [`std::io::Error`] values
//! whose [`io::ErrorKind`] - and, for QUIC/TLS protocol failures, the
//! [`protocol::ProtocolViolation`] tag - route through
//! `core::ExitCode::from_io_error` to the SAME exit code the TCP/`rsync://`
//! transport already uses for the analogous failure.
//!
//! QUIC is an oc-only transport with no upstream C equivalent, so this mirrors
//! oc's own `io::Error` -> `ExitCode` convention - the single shared bridge
//! every transport rides (an SSH stall becomes [`io::ErrorKind::TimedOut`],
//! which `core` maps to `ExitCode::Timeout`, and so on) - rather than any
//! upstream wire behaviour. The classifier never names `ExitCode` itself
//! because `rsync_io` sits below `core` in the crate graph; it selects the
//! exit code indirectly, by choosing the `ErrorKind`/tag that the shared mapper
//! keys on.
//!
//! # Parity table
//!
//! | QUIC failure                                             | `io::ErrorKind` / tag        | `core::ExitCode`  |
//! |----------------------------------------------------------|------------------------------|-------------------|
//! | idle / handshake timeout (`TimedOut`)                    | `TimedOut`                   | `Timeout`   (30)  |
//! | peer reset (`Reset`) / stream reset (`ReadError::Reset`) | `ConnectionReset`            | `SocketIo`  (10)  |
//! | CIDs exhausted (`CidsExhausted`)                         | `NotConnected`               | `SocketIo`  (10)  |
//! | peer app-close, non-zero code (`ApplicationClosed`)      | `ConnectionAborted`          | `SocketIo`  (10)  |
//! | version mismatch (`VersionMismatch`)                     | `ProtocolViolation`          | `Protocol`  (2)   |
//! | peer transport/TLS error, incl. cert-trust (`TransportError`) | `ProtocolViolation`     | `Protocol`  (2)   |
//! | peer QUIC abort frame (`ConnectionClosed`)               | `ProtocolViolation`          | `Protocol`  (2)   |
//! | connect setup failure (`ConnectError::*`)                | `AddrNotAvailable`/`NotConnected` | `SocketIo` (10) |
//! | unsupported QUIC version (`ConnectError::UnsupportedVersion`) | `ProtocolViolation`     | `Protocol`  (2)   |
//! | local UDP socket failure (driver `io::Error`)            | preserved kind               | per `from_io_error` |
//! | driver gone mid-stream                                   | `BrokenPipe`                 | `SocketIo`  (10)  |
//!
//! # Design note: TLS / certificate-trust failures
//!
//! A failed certificate-trust or TLS handshake surfaces from `quinn-proto` as a
//! [`ConnectionError::TransportError`] carrying a `CRYPTO_ERROR` (TLS alert)
//! code, not as a distinct variant. Upstream rsync's schema reserves
//! `RERR_STARTCLIENT` (5) for a failed client-server handshake, but the shared
//! `io::Error` -> `ExitCode` bridge cannot express code 5 (no `ErrorKind` maps
//! to it, and `rsync_io` cannot depend on `core` to bypass the bridge without
//! inverting the crate layering). A rejected secure handshake is, in substance,
//! a protocol failure, so these route to `RERR_PROTOCOL` (2) alongside every
//! other handshake/version violation - one policy, expressed once, through the
//! shared mapper.

use std::io;

use quinn_proto::{ConnectError, ConnectionError, VarInt};

/// A classified QUIC transport failure recorded by the driver.
///
/// Stored in place of a bare string so the blocking facade can rebuild a
/// [`std::io::Error`] carrying the [`io::ErrorKind`] - and, for protocol
/// violations, the [`protocol::ProtocolViolation`] tag - that
/// `core::ExitCode::from_io_error` maps to the parity exit code.
#[derive(Clone, Debug)]
pub struct TransportFault {
    kind: io::ErrorKind,
    protocol: bool,
    message: String,
}

impl TransportFault {
    fn new(kind: io::ErrorKind, protocol: bool, message: String) -> Self {
        Self {
            kind,
            protocol,
            message,
        }
    }

    /// Rebuilds the classified [`std::io::Error`] for the facade to return.
    ///
    /// Protocol violations are emitted through [`protocol::protocol_violation()`]
    /// so they carry the [`protocol::ProtocolViolation`] tag (mapped to
    /// `RERR_PROTOCOL` = 2); every other fault carries its selected
    /// [`io::ErrorKind`] verbatim.
    #[must_use]
    pub fn to_io_error(&self) -> io::Error {
        if self.protocol {
            protocol::protocol_violation(self.message.clone())
        } else {
            io::Error::new(self.kind, self.message.clone())
        }
    }
}

/// Classifies a lost-connection reason.
///
/// `ApplicationClosed(0)` and `LocallyClosed` are clean session ends handled by
/// the caller ([`super::Terminal::from_loss`]); their arms here are defensive
/// and unreachable in practice.
#[must_use]
pub fn connection_fault(reason: &ConnectionError) -> TransportFault {
    let message = reason.to_string();
    match reason {
        // Idle or handshake timeout: parity with a TCP read/connect stall.
        ConnectionError::TimedOut => TransportFault::new(io::ErrorKind::TimedOut, false, message),
        // Peer vanished/restarted: a connection reset.
        ConnectionError::Reset => {
            TransportFault::new(io::ErrorKind::ConnectionReset, false, message)
        }
        // Ran out of connection-ID space: the connection could not be kept.
        ConnectionError::CidsExhausted => {
            TransportFault::new(io::ErrorKind::NotConnected, false, message)
        }
        // Peer's application closed with a non-zero code (code 0 is clean and
        // filtered upstream of here): the peer aborted the connection.
        ConnectionError::ApplicationClosed(_) | ConnectionError::LocallyClosed => {
            TransportFault::new(io::ErrorKind::ConnectionAborted, false, message)
        }
        // Version negotiation, a QUIC-spec/TLS transport error (including a
        // failed certificate-trust handshake), or a peer CONNECTION_CLOSE with
        // a transport error frame: all handshake/protocol failures.
        ConnectionError::VersionMismatch
        | ConnectionError::TransportError(_)
        | ConnectionError::ConnectionClosed(_) => {
            TransportFault::new(io::ErrorKind::InvalidData, true, message)
        }
    }
}

/// Classifies a pre-I/O [`ConnectError`] returned by `Endpoint::connect`.
///
/// A QUIC connection that cannot even be initiated is a connection failure
/// (`RERR_SOCKETIO` = 10), except a version the local endpoint cannot speak,
/// which is a protocol incompatibility (`RERR_PROTOCOL` = 2).
#[must_use]
pub fn connect_fault(err: &ConnectError) -> io::Error {
    let message = err.to_string();
    let kind = match err {
        ConnectError::UnsupportedVersion => return protocol::protocol_violation(message),
        // A malformed peer address or server name: the peer cannot be addressed.
        ConnectError::InvalidRemoteAddress(_) | ConnectError::InvalidServerName(_) => {
            io::ErrorKind::AddrNotAvailable
        }
        // The endpoint is unusable or out of connection-ID space: no connection.
        ConnectError::EndpointStopping
        | ConnectError::CidsExhausted
        | ConnectError::NoDefaultClientConfig => io::ErrorKind::NotConnected,
    };
    io::Error::new(kind, message)
}

/// Classifies a peer-initiated stream reset (`ReadError::Reset`) as a
/// connection reset, matching [`ConnectionError::Reset`].
#[must_use]
pub fn stream_reset(code: VarInt) -> TransportFault {
    TransportFault::new(
        io::ErrorKind::ConnectionReset,
        false,
        format!("stream reset by peer: code {code}"),
    )
}

/// Wraps a genuine local UDP-socket failure, preserving its [`io::ErrorKind`]
/// so the shared mapper keeps whatever classification the OS reported.
#[must_use]
pub fn io_fault(err: &io::Error) -> TransportFault {
    TransportFault::new(err.kind(), false, err.to_string())
}

/// Builds the error surfaced when the driver thread exited without recording a
/// terminal - an abnormal teardown that leaves the transport pipe broken.
#[must_use]
pub fn driver_gone(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, message.to_owned())
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use quinn_proto::{ApplicationClose, ConnectionClose, TransportError, TransportErrorCode};

    use super::*;

    fn app_close(code: u32) -> ConnectionError {
        ConnectionError::ApplicationClosed(ApplicationClose {
            error_code: VarInt::from_u32(code),
            reason: Bytes::new(),
        })
    }

    fn conn_close() -> ConnectionError {
        ConnectionError::ConnectionClosed(ConnectionClose {
            error_code: TransportErrorCode::PROTOCOL_VIOLATION,
            frame_type: None,
            reason: Bytes::new(),
        })
    }

    fn transport_err() -> ConnectionError {
        ConnectionError::TransportError(TransportError {
            code: TransportErrorCode::crypto(0x2a),
            frame: None,
            reason: "bad certificate".to_owned(),
        })
    }

    fn is_protocol(err: &io::Error) -> bool {
        err.get_ref()
            .is_some_and(|inner| inner.is::<protocol::ProtocolViolation>())
    }

    /// WHY: an idle/handshake timeout must reach `ExitCode::Timeout` (30) - the
    /// same code a TCP read stall reaches - so a wedged QUIC peer is not
    /// misreported as a generic file/socket error. The selector for that code
    /// is `ErrorKind::TimedOut`; assert it equals the kind a real TCP timeout
    /// carries (parity, not a hard-coded constant).
    #[test]
    fn timeout_maps_like_a_tcp_stall() {
        let quic = connection_fault(&ConnectionError::TimedOut).to_io_error();
        let tcp = io::Error::new(io::ErrorKind::TimedOut, "read timed out");
        assert_eq!(quic.kind(), tcp.kind());
        assert_eq!(quic.kind(), io::ErrorKind::TimedOut);
        assert!(!is_protocol(&quic));
    }

    /// WHY: a peer reset (connection or stream) is a socket-level loss and must
    /// reach `ExitCode::SocketIo` (10), in parity with a TCP `ConnectionReset`.
    #[test]
    fn peer_reset_maps_like_a_tcp_reset() {
        let tcp = io::Error::from(io::ErrorKind::ConnectionReset);
        for quic in [
            connection_fault(&ConnectionError::Reset).to_io_error(),
            stream_reset(VarInt::from_u32(7)).to_io_error(),
        ] {
            assert_eq!(quic.kind(), tcp.kind());
            assert_eq!(quic.kind(), io::ErrorKind::ConnectionReset);
            assert!(!is_protocol(&quic));
        }
    }

    /// WHY: the socket-loss class (`ExitCode::SocketIo` = 10) is selected by
    /// several `ErrorKind`s; each of these QUIC faults must land in that class,
    /// never in the default `FileIo` (11) bucket that an unclassified
    /// `ErrorKind::Other` would fall into.
    #[test]
    fn connection_losses_stay_in_the_socket_class() {
        let socket_kinds = [
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::AddrInUse,
            io::ErrorKind::AddrNotAvailable,
            io::ErrorKind::NotConnected,
        ];
        for quic in [
            connection_fault(&ConnectionError::CidsExhausted).to_io_error(),
            connection_fault(&app_close(99)).to_io_error(),
            driver_gone("driver exited mid-stream"),
        ] {
            assert!(
                socket_kinds.contains(&quic.kind()),
                "{:?} is not a socket-class kind",
                quic.kind()
            );
            assert!(!is_protocol(&quic));
        }
    }

    /// WHY: version negotiation, a QUIC-spec/TLS transport error (which is how a
    /// certificate-trust rejection arrives), and a peer transport-error close
    /// are handshake/protocol failures and must reach `ExitCode::Protocol` (2).
    /// That code is selected only by the `ProtocolViolation` tag, so assert the
    /// tag is present - a plain `InvalidData` would silently degrade to
    /// `StreamIo` (12).
    #[test]
    fn handshake_and_protocol_failures_carry_the_protocol_tag() {
        for quic in [
            connection_fault(&ConnectionError::VersionMismatch).to_io_error(),
            connection_fault(&transport_err()).to_io_error(),
            connection_fault(&conn_close()).to_io_error(),
        ] {
            assert_eq!(quic.kind(), io::ErrorKind::InvalidData);
            assert!(
                is_protocol(&quic),
                "protocol failure must carry the ProtocolViolation tag"
            );
        }
    }

    /// WHY: a certificate-trust failure has no dedicated variant - it is a
    /// `TransportError` with a `CRYPTO_ERROR` code - and the shared bridge
    /// cannot express `StartClient` (5), so it must map to `Protocol` (2) like
    /// any other rejected handshake. This pins that documented decision.
    #[test]
    fn cert_trust_failure_maps_to_protocol() {
        let quic = connection_fault(&transport_err()).to_io_error();
        assert!(is_protocol(&quic));
    }

    /// WHY: a connection that cannot be initiated is a connection failure
    /// (`SocketIo` = 10), except an unsupported QUIC version, which is a
    /// protocol incompatibility (`Protocol` = 2).
    #[test]
    fn connect_errors_split_socket_vs_protocol() {
        let socket = connect_fault(&ConnectError::InvalidRemoteAddress(
            "0.0.0.0:0".parse().expect("addr"),
        ));
        assert_eq!(socket.kind(), io::ErrorKind::AddrNotAvailable);
        assert!(!is_protocol(&socket));

        let stopping = connect_fault(&ConnectError::EndpointStopping);
        assert_eq!(stopping.kind(), io::ErrorKind::NotConnected);

        let version = connect_fault(&ConnectError::UnsupportedVersion);
        assert_eq!(version.kind(), io::ErrorKind::InvalidData);
        assert!(is_protocol(&version));
    }

    /// WHY: a genuine local UDP-socket error must keep the OS-reported kind so
    /// the shared mapper classifies it exactly as it would for any other
    /// transport, rather than being flattened to `ErrorKind::Other`.
    #[test]
    fn local_socket_failure_preserves_its_kind() {
        let original = io::Error::new(io::ErrorKind::PermissionDenied, "sendto: EPERM");
        let fault = io_fault(&original).to_io_error();
        assert_eq!(fault.kind(), io::ErrorKind::PermissionDenied);
        assert!(!is_protocol(&fault));
    }
}
