//! EDG-GOODBYE.4: receiver-side goodbye-phase timeout + disconnect tests.
//!
//! [`goodbye_partial_cutoff`](super::goodbye_partial_cutoff) pins the
//! receiver's reaction to sender misbehaviour using synchronous
//! `Cursor`-backed readers. Those tests prove the receiver surfaces a
//! typed `io::Error` on truncated input and rejects garbage NDXs, but
//! they cannot exercise the third failure mode upstream care about:
//! the underlying socket has a configured `--timeout`, the sender
//! delivers a partial NDX_DONE prefix, and the kernel-level read
//! deadline fires before any further bytes arrive.
//!
//! The receiver's `handle_goodbye` delegates timeout handling to its
//! `Read` impl (typically a `TcpStream` configured with
//! `set_read_timeout`). These tests simulate that surface with a
//! purpose-built `Read` that
//!
//!   - feeds a configurable prefix of bytes onto the wire,
//!   - then returns `io::ErrorKind::TimedOut` or `Ok(0)` instead of
//!     blocking, depending on the failure mode being exercised.
//!
//! The readers never sleep: they count the reads issued after the prefix,
//! so "tried to read" and "did not retry" are exact call counts rather than
//! wall-clock bands. The asserted error kinds map 1:1 to upstream rsync's
//! behaviour: `TimedOut` reaches `cleanup.c` as `RERR_TIMEOUT (30)`,
//! `UnexpectedEof` and a `ConnectionAborted`/disconnect both reach as
//! `RERR_STREAMIO (12)`.
//!
//! # Upstream Reference
//!
//! - `main.c:921-952` - `read_final_goodbye()` reads the final NDX and
//!   exits with `RERR_PROTOCOL` for a wrong value, propagates I/O
//!   errors for short reads.
//! - `io.c:read_timeout()` - upstream's socket-level read deadline
//!   that surfaces `RERR_TIMEOUT`. Equivalent on our side is the
//!   `io::ErrorKind::TimedOut` surfaced by `TcpStream` when the read
//!   timeout fires.
//! - `cleanup.c:exit_cleanup()` - exit-code mapping: timeout -> 30,
//!   stream I/O failure -> 12.

use std::ffi::OsString;
use std::io::{self, Read};

use protocol::ProtocolVersion;
use protocol::codec::{NDX_DONE_MODERN_BYTE, create_ndx_codec};

use super::super::super::ReceiverContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

fn handshake_for(protocol_version: u8) -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

fn receiver_for(protocol_version: u8) -> ReceiverContext {
    let handshake = handshake_for(protocol_version);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    ReceiverContext::new_for_test(&handshake, config)
}

/// Drives `handle_goodbye` against the supplied `Read` and discards
/// whatever the receiver writes back. Returns the full `io::Result`
/// so the test can assert on the error kind.
fn drive_handle_goodbye<R: Read>(ctx: &ReceiverContext, mut reader: R) -> io::Result<()> {
    let mut writer = Vec::<u8>::new();
    let mut ndx_write = create_ndx_codec(ctx.protocol.as_u8());
    let mut ndx_read = create_ndx_codec(ctx.protocol.as_u8());
    ctx.handle_goodbye(&mut reader, &mut writer, &mut ndx_write, &mut ndx_read)
}

/// What the peer's socket reports once the delivered prefix is drained.
#[derive(Clone, Copy)]
enum Stall {
    /// `set_read_timeout` fired: the read surfaces `TimedOut`.
    Timeout,
    /// The peer closed the socket: the read returns `Ok(0)`.
    Disconnect,
}

/// Reader that drains an in-memory prefix, then answers every further read
/// with its [`Stall`] outcome and counts those reads in `stalled_reads`.
///
/// Models a `TcpStream` whose peer delivered `prefix` bytes and then
/// stalled or hung up. `prefix` may be empty (immediate stall) or one byte
/// short of a frame (partial-byte cutoff).
struct StallingReader {
    prefix: Vec<u8>,
    cursor: usize,
    stall: Stall,
    stalled_reads: usize,
}

impl StallingReader {
    fn new(prefix: Vec<u8>, stall: Stall) -> Self {
        Self {
            prefix,
            cursor: 0,
            stall,
            stalled_reads: 0,
        }
    }
}

impl Read for StallingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cursor < self.prefix.len() {
            let remaining = &self.prefix[self.cursor..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.cursor += n;
            return Ok(n);
        }
        self.stalled_reads += 1;
        match self.stall {
            Stall::Timeout => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "simulated socket read timeout",
            )),
            Stall::Disconnect => Ok(0),
        }
    }
}

/// The sender delivers a single byte of the modern goodbye frame (the
/// `0xFF` negative prefix that announces a negative NDX) and then
/// stalls. The receiver must surface `io::ErrorKind::TimedOut` after
/// exactly one stalled read: zero would mean it never waited on the
/// socket, more would mean it retried past the kernel deadline and
/// multiplied the configured `--timeout`.
///
/// Mirrors upstream's `read_timeout()` firing inside `read_ndx_and_attrs`
/// at the final goodbye site. The mapped exit code on the typed kind
/// is `RERR_TIMEOUT (30)` once the receiver translates the I/O error
/// at the top of the transfer loop.
#[test]
fn recv_goodbye_partial_byte_returns_timeout_error() {
    let ctx = receiver_for(32);
    // Modern NDX_DONE is one varint byte (0x7E); 0xFF then silence is the
    // canonical partial-byte cutoff for the modern codec.
    let mut reader = StallingReader::new(vec![0xFFu8], Stall::Timeout);
    let err = drive_handle_goodbye(&ctx, &mut reader)
        .expect_err("partial-byte cutoff with no follow-up must surface a typed error");
    assert_eq!(
        err.kind(),
        io::ErrorKind::TimedOut,
        "partial-byte cutoff after a single 0xFF prefix must surface TimedOut, got {err:?}",
    );
    assert_eq!(
        reader.stalled_reads, 1,
        "the receiver must wait on the socket once and not retry past TimedOut",
    );
}

/// The sender delivers zero bytes - the socket is connected but the
/// peer is hung. The receiver must still surface `TimedOut` after one
/// stalled read, the no-progress variant of the partial-byte cutoff.
#[test]
fn recv_goodbye_immediate_stall_returns_timeout_error() {
    let ctx = receiver_for(32);
    let mut reader = StallingReader::new(Vec::new(), Stall::Timeout);
    let err = drive_handle_goodbye(&ctx, &mut reader)
        .expect_err("zero-byte stall must surface a typed error");
    assert_eq!(
        err.kind(),
        io::ErrorKind::TimedOut,
        "zero-byte stall with simulated socket timeout must surface TimedOut, got {err:?}",
    );
    assert_eq!(
        reader.stalled_reads, 1,
        "the receiver must not retry past TimedOut"
    );
}

/// The sender closes the socket after sending the modern negative
/// prefix but before delivering the rest of NDX_DONE. The receiver
/// must surface a typed `Err`, not `Ok(())`, classified as a stream-I/O
/// failure (`UnexpectedEof`), and must give up on the first `Ok(0)`
/// rather than spinning on a closed socket.
///
/// Upstream maps this condition to `RERR_STREAMIO (12)` once the
/// receiver translates the I/O error.
#[test]
fn recv_goodbye_socket_disconnect_returns_typed_error() {
    let ctx = receiver_for(32);
    let mut reader = StallingReader::new(vec![0xFFu8], Stall::Disconnect);
    let err = drive_handle_goodbye(&ctx, &mut reader)
        .expect_err("socket disconnect mid-frame must surface a typed error");
    assert_eq!(
        err.kind(),
        io::ErrorKind::UnexpectedEof,
        "mid-frame socket disconnect must surface UnexpectedEof, got {err:?}",
    );
    assert_eq!(
        reader.stalled_reads, 1,
        "a closed socket must end the read on the first Ok(0)"
    );
}

/// Sender closes the socket without ever sending a byte. The receiver
/// must surface a typed `Err`, not `Ok(())`. Covers the case where the
/// peer crashed before the goodbye phase even started.
#[test]
fn recv_goodbye_immediate_disconnect_returns_typed_error() {
    let ctx = receiver_for(32);
    let mut reader = StallingReader::new(Vec::new(), Stall::Disconnect);
    let err = drive_handle_goodbye(&ctx, &mut reader)
        .expect_err("immediate socket disconnect must surface a typed error");
    assert_eq!(
        err.kind(),
        io::ErrorKind::UnexpectedEof,
        "immediate disconnect before any goodbye byte must surface UnexpectedEof, got {err:?}",
    );
}

/// Positive control: the full modern NDX_DONE byte arrives and the
/// receiver completes cleanly without issuing a single read past it.
/// Guards against over-correction in the error path - a regression that
/// reads ahead after the goodbye would block on a real socket, and one
/// that turns every short read into an error fails the `Ok`.
#[test]
fn recv_goodbye_completes_normally_when_full_bytes_arrive() {
    let ctx = receiver_for(32);
    // Protocol 32 extended goodbye reads exactly one varint NDX_DONE echo.
    let mut reader = StallingReader::new(vec![NDX_DONE_MODERN_BYTE], Stall::Timeout);
    drive_handle_goodbye(&ctx, &mut reader)
        .expect("well-formed NDX_DONE byte must drive handle_goodbye to Ok(())");
    assert_eq!(
        reader.stalled_reads, 0,
        "the receiver must stop reading once NDX_DONE is observed"
    );
}
