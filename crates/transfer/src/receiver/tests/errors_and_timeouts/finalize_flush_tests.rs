//! Tests for the post-goodbye flush in `finalize_transfer`.
//!
//! The receiver-role finalization sequence ends with `handle_goodbye`
//! followed by an explicit `writer.flush()` so any NDX_DONE (and any
//! trailing multiplexed MSG_INFO frames) leave the userspace buffer
//! before the transport FIN. Without this flush the upstream-rsync
//! `reverse-daemon-delta` interop scenario (oc-rsync client pushing to
//! an upstream daemon receiver) hangs at the test timeout because the
//! peer awaits a final NDX_DONE echo that is still sitting in the
//! writer.
//!
//! Mirrors the symmetric flush in `generator::transfer::orchestrator::run`
//! (UTS-15.c, commit a3bc2cbd2) at the bottom of the generator role.
//!
//! # Upstream Reference
//!
//! - `main.c:1085` - `do_recv()` child path `io_flush(FULL_FLUSH)` after the
//!   receiver writes its final NDX_DONE.
//! - `main.c:1135` / `main.c:1141` - `do_recv()` parent path
//!   `io_flush(FULL_FLUSH)` after `handle_stats(-1)` and after the final
//!   NDX_DONE write.

use std::ffi::OsString;
use std::io::{self, Cursor, Write};

use protocol::ProtocolVersion;

use super::super::super::ReceiverContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

/// NDX_DONE as 4-byte little-endian (-1 = 0xFFFFFFFF).
const NDX_DONE_LE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

/// Writer that records every `write` and `flush` so tests can assert that
/// upstream's `io_flush(FULL_FLUSH)` contract is honoured before
/// `finalize_transfer` returns. Mirrors the generator-side
/// `FlushTrackingWriter` used by `handle_goodbye_proto31_flushes_ndx_done_before_close`.
#[derive(Default)]
struct FlushTrackingWriter {
    buffer: Vec<u8>,
    flushes: usize,
    last_op_was_flush: bool,
}

impl Write for FlushTrackingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        self.last_op_was_flush = false;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        self.last_op_was_flush = true;
        Ok(())
    }
}

/// Writer that succeeds for the first `succeed_count` flushes and then
/// fails every subsequent flush with `kind`. Used to make sure the
/// final post-goodbye flush is the one that hits the error path, so the
/// early-close tolerance branch in `finalize_transfer` is the unit under
/// test rather than a mid-handshake flush.
struct TailFlushFailingWriter {
    succeed_count: usize,
    flushes: usize,
    kind: io::ErrorKind,
}

impl TailFlushFailingWriter {
    fn new(succeed_count: usize, kind: io::ErrorKind) -> Self {
        Self {
            succeed_count,
            flushes: 0,
            kind,
        }
    }
}

impl Write for TailFlushFailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if self.flushes <= self.succeed_count {
            Ok(())
        } else {
            Err(io::Error::from(self.kind))
        }
    }
}

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

fn receiver_server_mode(protocol_version: u8) -> ReceiverContext {
    let handshake = handshake_for(protocol_version);
    let mut config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    // Server mode skips the `receive_stats` call inside `finalize_transfer`
    // so the test does not have to feed stats bytes onto the wire.
    config.connection.client_mode = false;
    ReceiverContext::new_for_test(&handshake, config)
}

/// Sender bytes for a protocol-28 receiver-side finalization:
/// - one NDX_DONE echo for the phase 1 -> 2 transition
/// - one NDX_DONE for the sender's final post-loop marker
///
/// Protocol 28 has `supports_goodbye_exchange` but not extended goodbye,
/// so `handle_goodbye` writes one NDX_DONE and reads nothing.
fn proto28_sender_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8);
    bytes.extend_from_slice(&NDX_DONE_LE);
    bytes.extend_from_slice(&NDX_DONE_LE);
    bytes
}

#[test]
fn finalize_transfer_proto28_flushes_after_goodbye() {
    // UTS-REVDD regression: the upstream `reverse-daemon-delta` test
    // hangs at 300s because the receiver-role finalization does not
    // flush the buffered NDX_DONE before yielding to socket close.
    // Assert the writer is flushed at least once and that the LAST
    // operation before `finalize_transfer` returns is a flush, not a
    // partial write left in the userspace buffer.
    let mut ctx = receiver_server_mode(28);
    ctx.advance_pipeline_to_delta_transfer_for_test();

    let mut reader = Cursor::new(proto28_sender_bytes());
    let mut writer = FlushTrackingWriter::default();

    ctx.finalize_transfer(&mut reader, &mut writer)
        .expect("finalize_transfer completes");

    // 2 NDX_DONEs from `exchange_phase_done` + 1 NDX_DONE from
    // `handle_goodbye` = 12 wire bytes.
    assert_eq!(
        writer.buffer.len(),
        12,
        "expected 3 NDX_DONE markers on the wire, got {} bytes",
        writer.buffer.len(),
    );
    assert!(
        writer.buffer.ends_with(&NDX_DONE_LE),
        "wire output must end with NDX_DONE: {:?}",
        writer.buffer,
    );

    // The flush after `handle_goodbye` is what closes the UTS-REVDD
    // hang. `exchange_phase_done` flushes after each NDX_DONE write
    // (2 flushes), `handle_goodbye` flushes after its NDX_DONE write
    // (1 flush), and the post-goodbye tail flush this test guards is
    // the fourth. Assert >= 4 so we cover the full sequence.
    assert!(
        writer.flushes >= 4,
        "expected >= 4 flushes (phases x2 + goodbye + tail), got {}",
        writer.flushes,
    );

    // The crucial assertion: the LAST operation before return is a
    // flush. Without the new tail flush in `finalize_transfer`, the
    // last op would be the NDX_DONE write from `handle_goodbye`.
    assert!(
        writer.last_op_was_flush,
        "the final operation before finalize_transfer returns must be a flush, \
         not a write - this guards the UTS-REVDD hang",
    );
}

#[test]
fn finalize_transfer_tolerates_broken_pipe_on_tail_flush() {
    // Mirror the generator-side tolerance: if the peer has already
    // closed the socket by the time we flush the tail, return cleanly
    // instead of surfacing a spurious error. The mid-handshake flushes
    // (2 in `exchange_phase_done` + 1 in `handle_goodbye` = 3) succeed;
    // only the 4th tail flush fails with `BrokenPipe`, which is
    // classified as an early close by `crate::is_early_close_error`.
    let mut ctx = receiver_server_mode(28);
    ctx.advance_pipeline_to_delta_transfer_for_test();

    let mut reader = Cursor::new(proto28_sender_bytes());
    let mut writer = TailFlushFailingWriter::new(3, io::ErrorKind::BrokenPipe);

    ctx.finalize_transfer(&mut reader, &mut writer)
        .expect("BrokenPipe on tail flush is tolerated");
    assert_eq!(
        writer.flushes, 4,
        "expected exactly 4 flush attempts (phases x2 + goodbye + tail), got {}",
        writer.flushes,
    );
}

#[test]
fn finalize_transfer_surfaces_non_close_errors_on_tail_flush() {
    // Defense-in-depth: a non-close flush error on the tail (e.g. an
    // unexpected `Other` kind from a wrapped writer) must still be
    // surfaced. This guards against accidentally swallowing real I/O
    // failures in the new tail-flush branch.
    let mut ctx = receiver_server_mode(28);
    ctx.advance_pipeline_to_delta_transfer_for_test();

    let mut reader = Cursor::new(proto28_sender_bytes());
    let mut writer = TailFlushFailingWriter::new(3, io::ErrorKind::Other);

    let err = ctx
        .finalize_transfer(&mut reader, &mut writer)
        .expect_err("non-close tail flush failure surfaces");
    assert_eq!(
        err.kind(),
        io::ErrorKind::Other,
        "non-close flush error must propagate unchanged, got {err:?}",
    );
    assert!(
        !crate::is_early_close_error(&err),
        "tail-flush error must NOT be classified as early close: {err:?}",
    );
}
