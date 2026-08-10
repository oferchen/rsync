//! EDG-GOODBYE.4: receiver-side goodbye partial-cutoff + timeout tests.
//!
//! Locks in the receiver-side half of the EDG-GOODBYE contract. While
//! EDG-GOODBYE.1/.2/.3 (see `crates/protocol/tests/goodbye_contract.rs`)
//! pin the *sender*-emitted wire shape, these tests pin the *receiver*'s
//! behaviour when the sender misbehaves:
//!
//! - Sender cuts the socket mid-byte inside the goodbye frame.
//! - Sender cuts the socket before emitting any goodbye byte.
//! - Sender sends a non-`NDX_DONE` NDX in place of the goodbye sentinel.
//! - Sender hangs without sending anything (timeout / no-progress).
//!
//! In every case the receiver must return a typed `io::Error` rather
//! than panic. Upstream rsync handles the same conditions in
//! `main.c:893-924 read_final_goodbye()` by either propagating an I/O
//! error from `read_int` / `read_ndx_and_attrs` or by hitting the
//! `if (i != NDX_DONE)` branch that calls `exit_cleanup(RERR_PROTOCOL)`.
//! Our equivalents are `io::ErrorKind::UnexpectedEof` (propagated from
//! `read_exact`) and `io::ErrorKind::InvalidData` (emitted by
//! [`ReceiverContext::read_expected_ndx_done`]).
//!
//! # Upstream Reference
//!
//! - `main.c:893-924` - `read_final_goodbye()` reads the final NDX and
//!   exits with `RERR_PROTOCOL` if the value is not `NDX_DONE`.
//! - `io.c:read_ndx()` - the upstream parser likewise treats short reads
//!   as fatal.

use std::ffi::OsString;
use std::io::{self, Cursor, Read};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use protocol::ProtocolVersion;
use protocol::codec::{NDX_DONE_LEGACY_BYTES, NDX_DONE_MODERN_BYTE, NdxCodec, create_ndx_codec};

use super::super::super::ReceiverContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

/// Builds a `HandshakeResult` for the given protocol version.
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

/// Builds a `ReceiverContext` ready to drive the goodbye handshake.
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

/// Drives `handle_goodbye` against the supplied sender bytes and discards
/// whatever the receiver writes back. Returns the receiver's `io::Result`
/// so the caller can assert on the error kind.
fn drive_handle_goodbye(ctx: &ReceiverContext, sender_bytes: Vec<u8>) -> io::Result<()> {
    let mut reader = Cursor::new(sender_bytes);
    let mut writer = Vec::<u8>::new();
    let mut ndx_write = create_ndx_codec(ctx.protocol.as_u8());
    let mut ndx_read = create_ndx_codec(ctx.protocol.as_u8());
    ctx.handle_goodbye(&mut reader, &mut writer, &mut ndx_write, &mut ndx_read)
}

// Protocol-coverage note: `handle_goodbye` only *reads* a goodbye echo on
// protocols >= 31 (the extended-goodbye gate). On protocol 28/29 the
// receiver-side `handle_goodbye` is a pure write; the equivalent
// read-side site is `read_expected_ndx_done`, which is called by
// `exchange_phase_done` to consume each per-phase / per-segment / final
// NDX_DONE the sender emits. The legacy-codec EOF tests therefore drive
// `read_expected_ndx_done` directly; the modern-codec tests drive
// `handle_goodbye` directly.

/// Legacy NDX_DONE is a 4-byte LE `-1`. Supplying only 1-3 bytes of that
/// sequence must surface `UnexpectedEof` from the underlying `read_exact`,
/// not a panic.
///
/// Upstream behaviour: `main.c:902` calls `read_int(f_in)` which short-
/// reads to a fatal I/O error. Same shape on our side.
#[test]
fn read_expected_ndx_done_proto29_eof_mid_legacy_ndx_done() {
    let ctx = receiver_for(29);
    // Send only the first 3 of 4 NDX_DONE bytes - mid-frame cutoff.
    let truncated = NDX_DONE_LEGACY_BYTES[..3].to_vec();
    let mut reader = Cursor::new(truncated);
    let mut codec = create_ndx_codec(29);

    let err = ctx
        .read_expected_ndx_done(&mut codec, &mut reader, "mid-byte legacy cutoff")
        .expect_err("truncated legacy goodbye must surface an I/O error");
    assert_eq!(
        err.kind(),
        io::ErrorKind::UnexpectedEof,
        "mid-byte cutoff in legacy NDX_DONE must surface UnexpectedEof, got {err:?}",
    );
}

/// On the modern codec (protocol >= 30) a negative NDX starts with a `0xFF`
/// prefix and requires a second byte. Supplying only the `0xFF` and then
/// EOF must surface `UnexpectedEof` from `read_exact`, not a panic.
#[test]
fn handle_goodbye_proto32_eof_mid_modern_negative_prefix() {
    // Protocol 32 hits the extended-goodbye branch: receiver writes its own
    // NDX_DONE, then reads the sender's echo. Supplying just `0xFF` makes
    // the modern codec demand a second byte that never arrives.
    let ctx = receiver_for(32);
    let truncated = vec![0xFFu8];

    let err = drive_handle_goodbye(&ctx, truncated)
        .expect_err("0xFF without follow-up byte must surface an I/O error");
    assert_eq!(
        err.kind(),
        io::ErrorKind::UnexpectedEof,
        "mid-byte cutoff after modern 0xFF prefix must surface UnexpectedEof, got {err:?}",
    );
}

/// Sender closes the connection without ever sending a goodbye byte.
/// Receiver's first `read_exact` must surface `UnexpectedEof` rather than
/// hang or panic.
#[test]
fn handle_goodbye_proto32_immediate_eof() {
    let ctx = receiver_for(32);
    let err = drive_handle_goodbye(&ctx, Vec::new())
        .expect_err("immediate EOF must surface an I/O error");
    assert_eq!(
        err.kind(),
        io::ErrorKind::UnexpectedEof,
        "immediate EOF before goodbye must surface UnexpectedEof, got {err:?}",
    );
}

/// Legacy `read_expected_ndx_done` on empty input - mirrors the same
/// invariant for protocol 28/29 where `exchange_phase_done` is the
/// read-side caller of the goodbye exchange.
#[test]
fn read_expected_ndx_done_proto29_immediate_eof() {
    let ctx = receiver_for(29);
    let mut reader = Cursor::new(Vec::<u8>::new());
    let mut codec = create_ndx_codec(29);

    let err = ctx
        .read_expected_ndx_done(&mut codec, &mut reader, "immediate EOF on legacy")
        .expect_err("immediate EOF must surface an I/O error");
    assert_eq!(
        err.kind(),
        io::ErrorKind::UnexpectedEof,
        "immediate EOF before legacy goodbye must surface UnexpectedEof, got {err:?}",
    );
}

/// Sender sends a perfectly framed but wrong-valued NDX (a positive file
/// index) instead of `NDX_DONE`. The receiver must reject it with
/// `InvalidData`, mirroring upstream's `RERR_PROTOCOL` exit.
///
/// Upstream: `main.c:919-923` - `if (i != NDX_DONE) ... exit_cleanup(RERR_PROTOCOL);`.
#[test]
fn handle_goodbye_proto32_rejects_garbage_in_place_of_ndx_done() {
    let ctx = receiver_for(32);

    // Build a stream that *parses* as a valid modern NDX but is NOT
    // NDX_DONE: a positive file index (5). The codec accepts it, but
    // `read_expected_ndx_done` rejects any value != -1.
    let mut bytes = Vec::new();
    let mut codec = create_ndx_codec(32);
    codec.write_ndx(&mut bytes, 5).unwrap();

    let err = drive_handle_goodbye(&ctx, bytes).expect_err("non-NDX_DONE value must be rejected");
    assert_eq!(
        err.kind(),
        io::ErrorKind::InvalidData,
        "non-NDX_DONE garbage must surface InvalidData, got {err:?}",
    );
    // The error message must be informative and reference the role
    // trailer so upstream-style log scraping keeps working.
    let msg = err.to_string();
    assert!(
        msg.contains("expected goodbye NDX_DONE"),
        "error message should mention the expected NDX_DONE; got: {msg}"
    );
}

/// Legacy protocol equivalent: a 4-byte LE non-`-1` integer in place of
/// the goodbye `NDX_DONE`. Legacy receiver's only goodbye step is its own
/// write followed by no further read (the legacy `handle_goodbye` does
/// not consume an echo), so we exercise the more sensitive
/// `read_expected_ndx_done` path directly.
#[test]
fn read_expected_ndx_done_proto29_rejects_garbage() {
    let ctx = receiver_for(29);
    // 4-byte LE integer = 42 (definitely not NDX_DONE = -1).
    let mut reader = Cursor::new(42i32.to_le_bytes().to_vec());
    let mut codec = create_ndx_codec(29);

    let err = ctx
        .read_expected_ndx_done(&mut codec, &mut reader, "test garbage")
        .expect_err("non-NDX_DONE value must be rejected");
    assert_eq!(
        err.kind(),
        io::ErrorKind::InvalidData,
        "non-NDX_DONE garbage on legacy path must surface InvalidData, got {err:?}",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("test garbage"),
        "error message should include the context string; got: {msg}"
    );
}

/// A blocking reader that never produces bytes. Mimics a sender that
/// connects, completes the transfer, and then hangs forever instead of
/// emitting goodbye. The block here is unconditional - we only ever
/// drive this reader on a worker thread we can drop without joining.
struct BlockingReader;

impl Read for BlockingReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        // Park forever. The harness below bounds the test wall-clock
        // with mpsc::recv_timeout so even if the worker is parked we
        // still finish the test quickly. Spurious unparks loop back to
        // parking - the parser must never see synthetic bytes.
        loop {
            thread::park();
        }
    }
}

/// The receiver's goodbye reader must not return a value or panic when
/// the sender hangs. We bound the test wall-clock with `recv_timeout` so
/// a regression that causes the receiver to silently succeed on a hung
/// sender fails this test loudly within 2 seconds.
///
/// This is the receiver-side equivalent of upstream's expectation that
/// `read_int` / `read_ndx_and_attrs` block until either bytes arrive,
/// EOF arrives, or the user-configured `--timeout` fires. The test does
/// not assert a typed timeout error here (the production path takes that
/// from the I/O layer's `--timeout` handling, exercised by separate
/// timeout tests); it only asserts the receiver does not silently
/// declare success on a stalled stream.
#[test]
fn handle_goodbye_does_not_silently_complete_on_hung_sender() {
    let (tx, rx) = mpsc::channel::<io::Result<()>>();

    // Spawn the receiver-side goodbye handshake on a worker. We use
    // `recv_timeout` to bound the test runtime; the worker stays parked
    // until the test exits (the JoinHandle is intentionally dropped - a
    // bounded leak of a parked thread is acceptable in a unit test
    // bounded to <5s).
    let _ = thread::Builder::new()
        .name("edg-goodbye-4-hung-sender".to_owned())
        .spawn(move || {
            let ctx = receiver_for(32);
            let mut reader = BlockingReader;
            let mut writer = Vec::<u8>::new();
            let mut ndx_write = create_ndx_codec(32);
            let mut ndx_read = create_ndx_codec(32);
            let result =
                ctx.handle_goodbye(&mut reader, &mut writer, &mut ndx_write, &mut ndx_read);
            // The send may fail if the test already returned; that is
            // fine - we are not asserting on the value sent here, only
            // that the goodbye reader did not pop a fake success while
            // the sender produced zero bytes.
            let _ = tx.send(result);
        })
        .expect("spawn goodbye worker");

    // Bound the test wall-clock. A regression that lets the receiver
    // declare success on zero input would deliver a value on this
    // channel; the assertion below fails that fast. A correct receiver
    // blocks forever waiting for bytes, so the timeout elapses.
    match rx.recv_timeout(Duration::from_secs(2)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Expected: receiver blocked waiting for the sender. The
            // worker thread stays parked until process exit; that is
            // safe and intended.
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!(
                "goodbye worker disconnected without sending a result - the worker thread \
                 must either block on the BlockingReader or send a typed io::Result back",
            );
        }
        Ok(Ok(())) => {
            panic!(
                "handle_goodbye silently returned Ok(()) on a hung sender; receiver must not \
                 declare goodbye complete without observing NDX_DONE on the wire",
            );
        }
        Ok(Err(err)) => {
            // If a future change wires an internal read deadline into
            // `handle_goodbye`, the surfaced error must still be a
            // typed I/O error (TimedOut or UnexpectedEof) rather than a
            // panic. Accept either without asserting an exact kind so
            // this test does not lock implementation choice.
            assert!(
                matches!(
                    err.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::UnexpectedEof
                ),
                "if handle_goodbye returns Err on a hung sender, the kind must be TimedOut or \
                 UnexpectedEof; got {err:?}",
            );
        }
    }
}

/// Protocol 32 receiver accepts an `NDX_DEL_STATS` sentinel in
/// `handle_goodbye` and then drains five varints before reading
/// `NDX_DONE`. A sender that emits `NDX_DEL_STATS` and then cuts the
/// socket *inside* the varint payload must surface `UnexpectedEof`, not
/// panic and not silently succeed.
///
/// This guards the path where a future refactor of `DeleteStats::read_from`
/// might forget to propagate short-read errors.
#[test]
fn handle_goodbye_proto32_eof_inside_del_stats_payload() {
    use protocol::codec::NDX_DEL_STATS;

    let ctx = receiver_for(32);

    // Emit a valid NDX_DEL_STATS sentinel, then immediately stop. The
    // receiver will try to read five varints and short-read on the
    // first byte of the first varint.
    let mut bytes = Vec::new();
    let mut codec = create_ndx_codec(32);
    codec.write_ndx(&mut bytes, NDX_DEL_STATS).unwrap();
    // No payload bytes after the sentinel - sender cuts the socket.

    let err = drive_handle_goodbye(&ctx, bytes)
        .expect_err("missing del-stats payload must surface an I/O error");
    assert_eq!(
        err.kind(),
        io::ErrorKind::UnexpectedEof,
        "EOF inside del-stats payload must surface UnexpectedEof, got {err:?}",
    );
}

/// Sanity check: a well-formed single `NDX_DONE` byte from the sender
/// drives `handle_goodbye` cleanly on protocol 32. This guards against
/// a regression where over-eager error-path validation accidentally
/// rejects the happy path.
#[test]
fn handle_goodbye_proto32_accepts_well_formed_ndx_done() {
    let ctx = receiver_for(32);
    let bytes = vec![NDX_DONE_MODERN_BYTE];
    drive_handle_goodbye(&ctx, bytes).expect("well-formed goodbye must succeed");
}
