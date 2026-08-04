//! Discard-sink writer for the batch local-replay receiver path.

use std::io::{self, Write};

use super::msg_info::MsgInfoSender;

/// A zero-cost writer that discards every byte and every multiplexed protocol
/// message the receiver emits.
///
/// On a normal network receive the generator's request output - signature
/// blocks, per-file NDX requests, `MSG_*` frames - flows back to a live sender
/// that consumes it. When applying a recorded batch there is no such peer:
/// upstream opens the batch file as the receiver's `f_in` and points the
/// generator's `f_out` at one end of a self-pipe whose read end
/// (`batch_gen_fd`) is never drained, so the requests simply go nowhere
/// (`main.c:635-651`, and the `!read_batch` gate at `main.c:1359-1366` that
/// leaves that stream unmultiplexed). `DiscardSink` is that dead end - it
/// swallows the generator's outbound stream without allocating or blocking, so
/// the real receiver can drive to completion off a one-way pre-recorded input.
///
/// Every [`MsgInfoSender`] method uses the trait's default no-op, and the
/// [`Write`] path reports every byte as accepted so callers never observe a
/// short write.
#[derive(Debug, Default, Clone, Copy)]
pub struct DiscardSink;

impl DiscardSink {
    /// Creates a new discard sink.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Write for DiscardSink {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    #[inline]
    fn write_all(&mut self, _buf: &[u8]) -> io::Result<()> {
        Ok(())
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The receiver's outbound protocol messages have no live consumer during a
/// batch replay, so every frame is dropped via the trait's default no-ops.
impl MsgInfoSender for DiscardSink {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A discard sink accepts every byte as written and never reports a short
    /// write, so the receiver's `write_all`/`flush` calls always succeed.
    #[test]
    fn write_swallows_all_bytes() {
        let mut sink = DiscardSink::new();
        assert_eq!(
            sink.write(b"signature-block").unwrap(),
            b"signature-block".len()
        );
        sink.write_all(&[0u8; 4096]).unwrap();
        sink.flush().unwrap();
    }

    /// Every `MsgInfoSender` emission the generator half makes is a no-op that
    /// returns `Ok`, so a receiver that itemizes, reports deletions, or acks a
    /// committed file never fails just because no peer is listening.
    #[test]
    fn msg_info_sender_calls_are_noops() {
        let mut sink = DiscardSink::new();
        sink.send_msg_info(b"itemize").unwrap();
        sink.send_msg_error_xfer(b"xfer").unwrap();
        sink.send_msg_error(b"err").unwrap();
        sink.send_msg_warning(b"warn").unwrap();
        sink.send_msg_deleted(b"gone").unwrap();
        sink.send_msg_success(7).unwrap();
        assert!(!sink.maybe_send_keepalive().unwrap());
        sink.write_files_from_unframed(b"names").unwrap();
        // A batch replay is never multiplexed, so the sink must never claim to
        // frame its output (mirrors upstream's unmultiplexed batch f_out).
        assert!(!sink.is_output_multiplexed());
    }
}
