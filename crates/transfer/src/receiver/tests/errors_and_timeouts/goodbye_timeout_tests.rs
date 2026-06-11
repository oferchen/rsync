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
//! Each test bounds its own wall-clock with [`std::time::Instant`] so
//! the suite stays deterministic and finishes well under 2 seconds
//! even on CI runners with noisy schedulers. The asserted error kinds
//! map 1:1 to upstream rsync's behaviour: `TimedOut` reaches
//! `cleanup.c` as `RERR_TIMEOUT (30)`, `UnexpectedEof` and a
//! `ConnectionAborted`/disconnect both reach as `RERR_STREAMIO (12)`.
//!
//! # Upstream Reference
//!
//! - `main.c:893-924` - `read_final_goodbye()` reads the final NDX and
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
use std::time::{Duration, Instant};

use protocol::ProtocolVersion;
use protocol::codec::{NDX_DONE_MODERN_BYTE, create_ndx_codec};

use super::super::super::ReceiverContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Wall-clock ceiling for any single test in this module. All readers
/// honour an inner deadline well below this so a regression that breaks
/// the typed-error surface fails fast instead of stalling CI.
const TEST_WALL_CLOCK_CEILING: Duration = Duration::from_millis(1500);

/// Inner deadline the synthetic readers use to simulate the kernel's
/// read-timeout firing. Chosen so a correctly-behaved receiver returns
/// well within `TEST_WALL_CLOCK_CEILING`, leaving ~6x headroom for
/// scheduling noise.
const SIMULATED_TIMEOUT: Duration = Duration::from_millis(250);

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

// ---------------------------------------------------------------------------
// Synthetic `Read` impls
// ---------------------------------------------------------------------------

/// Reader that drains an in-memory prefix then surfaces
/// `io::ErrorKind::TimedOut` once the configured deadline elapses.
///
/// Models a `TcpStream` with `set_read_timeout(Some(D))` where the peer
/// delivered `prefix` bytes and then stalled. The receiver's
/// `read_exact` will issue one or more reads against this `Read`: the
/// first satisfies the prefix; subsequent reads block until the
/// internal deadline fires.
///
/// `prefix` may be any length, including zero (immediate timeout) or
/// `prefix.len() == intended_frame_len - 1` (partial-byte cutoff).
struct TimeoutAfterPrefixReader {
    prefix: Vec<u8>,
    cursor: usize,
    deadline: Instant,
}

impl TimeoutAfterPrefixReader {
    fn new(prefix: Vec<u8>, timeout: Duration) -> Self {
        Self {
            prefix,
            cursor: 0,
            deadline: Instant::now() + timeout,
        }
    }
}

impl Read for TimeoutAfterPrefixReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Drain the prefix synchronously; this is the part that arrives
        // before the sender stalls.
        if self.cursor < self.prefix.len() {
            let remaining = &self.prefix[self.cursor..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.cursor += n;
            return Ok(n);
        }

        // Prefix drained: spin in short sleeps until the deadline fires,
        // then surface the typed timeout error a real TcpStream would.
        // The wait is bounded by `deadline` so the test always finishes
        // even if a future change wraps this reader in something that
        // retries past the first TimedOut.
        let now = Instant::now();
        if now < self.deadline {
            // Park for the remaining slice so the test doesn't busy-spin.
            let remaining = self.deadline - now;
            std::thread::sleep(remaining);
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "simulated socket read timeout",
        ))
    }
}

/// Reader that drains an in-memory prefix then returns `Ok(0)` to
/// signal EOF, simulating a peer that closed the socket mid-frame.
/// `read_exact` translates `Ok(0)` into `io::ErrorKind::UnexpectedEof`,
/// which is the receiver-side mapping of upstream's "stream ended
/// before goodbye" condition.
struct DisconnectAfterPrefixReader {
    prefix: Vec<u8>,
    cursor: usize,
}

impl DisconnectAfterPrefixReader {
    fn new(prefix: Vec<u8>) -> Self {
        Self { prefix, cursor: 0 }
    }
}

impl Read for DisconnectAfterPrefixReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cursor < self.prefix.len() {
            let remaining = &self.prefix[self.cursor..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.cursor += n;
            return Ok(n);
        }
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// EDG-GOODBYE.4: timeout-band tests
// ---------------------------------------------------------------------------

/// The sender delivers a single byte of the modern goodbye frame (the
/// `0xFF` negative prefix that announces a negative NDX) and then
/// stalls. The receiver must surface `io::ErrorKind::TimedOut` within
/// 2-3x the configured timeout, never block forever, and never
/// silently report success.
///
/// Mirrors upstream's `read_timeout()` firing inside `read_ndx_and_attrs`
/// at the final goodbye site. The mapped exit code on the typed kind
/// is `RERR_TIMEOUT (30)` once the receiver translates the I/O error
/// at the top of the transfer loop.
#[test]
fn recv_goodbye_partial_byte_returns_timeout_error() {
    let ctx = receiver_for(32);
    // Modern NDX_DONE is one varint byte (0x7E). A peer that delivered
    // the 0xFF negative prefix and stalled is the canonical
    // partial-byte cutoff for the modern codec; supplying nothing at
    // all is the immediate-stall variant covered by
    // `recv_goodbye_immediate_stall_returns_timeout_error` below.
    let reader = TimeoutAfterPrefixReader::new(vec![0xFFu8], SIMULATED_TIMEOUT);

    let start = Instant::now();
    let err = drive_handle_goodbye(&ctx, reader)
        .expect_err("partial-byte cutoff with no follow-up must surface a typed error");
    let elapsed = start.elapsed();

    // Typed-error surface: the kernel's TimedOut must propagate through
    // `read_exact` -> `handle_goodbye` -> caller without being
    // swallowed or downgraded to `Ok(())`.
    assert_eq!(
        err.kind(),
        io::ErrorKind::TimedOut,
        "partial-byte cutoff after a single 0xFF prefix must surface TimedOut, got {err:?}",
    );

    // Wall-clock band: the receiver must wait at least one full timeout
    // window (proving it tried to read) but must NOT loop past the
    // ceiling. The lower bound is generous to absorb scheduler jitter
    // on busy CI runners.
    assert!(
        elapsed >= SIMULATED_TIMEOUT.saturating_sub(Duration::from_millis(50)),
        "receiver returned too fast ({elapsed:?}); should have waited at least one timeout \
         window before surfacing TimedOut",
    );
    assert!(
        elapsed < TEST_WALL_CLOCK_CEILING,
        "receiver took {elapsed:?} to return, exceeds {TEST_WALL_CLOCK_CEILING:?} ceiling - \
         a regression that retries past the first TimedOut would fail this assertion",
    );
}

/// The sender delivers zero bytes - the socket is connected but the
/// peer is hung. The receiver must still surface `TimedOut` within the
/// configured window. This is the no-progress variant of the
/// partial-byte cutoff above and covers the case where a regression
/// might short-circuit on a zero-byte buffer instead of blocking.
#[test]
fn recv_goodbye_immediate_stall_returns_timeout_error() {
    let ctx = receiver_for(32);
    let reader = TimeoutAfterPrefixReader::new(Vec::new(), SIMULATED_TIMEOUT);

    let start = Instant::now();
    let err =
        drive_handle_goodbye(&ctx, reader).expect_err("zero-byte stall must surface a typed error");
    let elapsed = start.elapsed();

    assert_eq!(
        err.kind(),
        io::ErrorKind::TimedOut,
        "zero-byte stall with simulated socket timeout must surface TimedOut, got {err:?}",
    );
    assert!(
        elapsed < TEST_WALL_CLOCK_CEILING,
        "receiver took {elapsed:?} to return on immediate stall, exceeds \
         {TEST_WALL_CLOCK_CEILING:?} ceiling",
    );
}

/// The sender closes the socket after sending the modern negative
/// prefix but before delivering the rest of NDX_DONE. The receiver
/// must surface a typed `Err`, not `Ok(())`, and the error kind must
/// classify as a stream-I/O failure (`UnexpectedEof`).
///
/// This guards the path where a future change might accidentally
/// downgrade an `Ok(0)` short read into `Ok(())` and silently produce
/// the wrong exit code. Upstream maps this condition to
/// `RERR_STREAMIO (12)` once the receiver translates the I/O error.
#[test]
fn recv_goodbye_socket_disconnect_returns_typed_error() {
    let ctx = receiver_for(32);
    // 0xFF negative prefix delivered, then socket closes. The modern
    // codec demands a second varint byte; `read_exact` short-reads on
    // `Ok(0)` and surfaces UnexpectedEof.
    let reader = DisconnectAfterPrefixReader::new(vec![0xFFu8]);

    let start = Instant::now();
    let err = drive_handle_goodbye(&ctx, reader)
        .expect_err("socket disconnect mid-frame must surface a typed error");
    let elapsed = start.elapsed();

    assert_eq!(
        err.kind(),
        io::ErrorKind::UnexpectedEof,
        "mid-frame socket disconnect must surface UnexpectedEof, got {err:?}",
    );
    assert!(
        elapsed < Duration::from_millis(100),
        "disconnect path took {elapsed:?}; should return immediately on Ok(0), not block",
    );
}

/// Sender closes the socket without ever sending a byte. The receiver
/// must surface a typed `Err`, not `Ok(())`. Covers the case where the
/// peer crashed before the goodbye phase even started.
#[test]
fn recv_goodbye_immediate_disconnect_returns_typed_error() {
    let ctx = receiver_for(32);
    let reader = DisconnectAfterPrefixReader::new(Vec::new());

    let err = drive_handle_goodbye(&ctx, reader)
        .expect_err("immediate socket disconnect must surface a typed error");
    assert_eq!(
        err.kind(),
        io::ErrorKind::UnexpectedEof,
        "immediate disconnect before any goodbye byte must surface UnexpectedEof, got {err:?}",
    );
}

/// Positive control: the same fixture wiring delivers the full modern
/// NDX_DONE byte (no timeout, no disconnect) and the receiver completes
/// cleanly. Guards against over-correction in the error path - a
/// regression that turns every short read into `TimedOut` would still
/// have to keep this happy path working.
///
/// Uses the same synthetic-reader scaffolding so the success and
/// failure cases share their drive path. Confirms that
/// `handle_goodbye` flushes its own write side and returns `Ok(())`
/// once the sender's NDX_DONE byte is observed.
#[test]
fn recv_goodbye_completes_normally_when_full_bytes_arrive() {
    let ctx = receiver_for(32);
    // Single modern NDX_DONE byte - protocol 32 extended goodbye
    // reads exactly one varint NDX_DONE echo from the sender.
    let reader = DisconnectAfterPrefixReader::new(vec![NDX_DONE_MODERN_BYTE]);

    let start = Instant::now();
    drive_handle_goodbye(&ctx, reader)
        .expect("well-formed NDX_DONE byte must drive handle_goodbye to Ok(())");
    let elapsed = start.elapsed();

    // Happy path must be fast; if a regression accidentally wired the
    // simulated timeout into the success path, this would slow to ~250 ms.
    assert!(
        elapsed < Duration::from_millis(100),
        "happy path took {elapsed:?}; should complete immediately on a complete NDX_DONE",
    );
}
