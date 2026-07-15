//! Trait for sending `MSG_INFO` protocol messages through multiplexed streams.
//!
//! Allows the receiver to emit itemize output as `MSG_INFO` frames without
//! being tightly coupled to [`ServerWriter`]. Writers that support multiplexed
//! output implement this trait; writers that do not use the default no-op.

use std::io::{self, Write};

use protocol::MessageCode;

use super::counting::CountingWriter;
use super::server::ServerWriter;

/// Trait for writers that can send `MSG_INFO` multiplexed messages.
///
/// Writers that support multiplexed output (like `ServerWriter` and
/// [`CountingWriter`]) implement this trait to forward `MSG_INFO` payloads
/// through the multiplex layer; writers that do not support it use the
/// default no-op.
///
/// # Upstream Reference
///
/// - `log.c:330-340` - `rwrite()` sends FINFO/FCLIENT as `MSG_INFO` when `am_server`
pub trait MsgInfoSender {
    /// Sends a `MSG_INFO` frame through the multiplexed output stream.
    ///
    /// The default implementation is a no-op, suitable for writers that do
    /// not support multiplexed protocol messages (e.g., plain `Vec<u8>` in tests).
    fn send_msg_info(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }

    /// Sends a `MSG_ERROR_XFER` frame through the multiplexed output stream.
    ///
    /// Mirrors upstream `rsyserr(FERROR_XFER, ...)`: the peer's `rwrite()`
    /// sets `got_xfer_error = 1` on receipt, so the transfer terminates with
    /// `RERR_PARTIAL` (exit 23) rather than success. Used by the receiver to
    /// report per-file transfer errors (e.g. a failed output `mkstemp()`) that
    /// force the incoming delta to be discarded.
    ///
    /// The default implementation is a no-op, matching [`Self::send_msg_info`].
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:297` - `rsyserr(FERROR_XFER, errno, "mkstemp %s failed", ...)`
    /// - `log.c:311` - receipt of `FERROR_XFER` sets `got_xfer_error = 1`
    fn send_msg_error_xfer(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }

    /// Sends a `MSG_DELETED` frame carrying the raw name of a deleted entry.
    ///
    /// A server generator emits one frame per deletion so the client renders the
    /// `deleting <path>` line itself (the client owns the verbosity and itemize
    /// gating). Directories carry a trailing NUL in `data` so the peer can tell
    /// a directory from a regular file (upstream `io.c:1616`).
    ///
    /// The default implementation is a no-op, matching [`Self::send_msg_info`].
    ///
    /// # Upstream Reference
    ///
    /// - `log.c:866-869` - `am_server && protocol_version >= 29` sends
    ///   `send_msg(MSG_DELETED, fname, len, am_generator)`.
    fn send_msg_deleted(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

impl<W: Write> MsgInfoSender for ServerWriter<W> {
    fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
        // Plain mode cannot send MSG_INFO - silently ignore (matches upstream
        // behavior where am_server gates message emission).
        if self.is_multiplexed() {
            self.send_message(MessageCode::Info, data)
        } else {
            Ok(())
        }
    }

    fn send_msg_error_xfer(&mut self, data: &[u8]) -> io::Result<()> {
        if self.is_multiplexed() {
            self.send_message(MessageCode::ErrorXfer, data)
        } else {
            Ok(())
        }
    }

    fn send_msg_deleted(&mut self, data: &[u8]) -> io::Result<()> {
        // upstream: log.c:866-869 - the MSG_DELETED path only exists on a
        // multiplexed server stream; plain mode never reaches this branch.
        if self.is_multiplexed() {
            self.send_message(MessageCode::Deleted, data)
        } else {
            Ok(())
        }
    }
}

impl<T: MsgInfoSender + ?Sized> MsgInfoSender for &mut T {
    fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
        (**self).send_msg_info(data)
    }

    fn send_msg_error_xfer(&mut self, data: &[u8]) -> io::Result<()> {
        (**self).send_msg_error_xfer(data)
    }

    fn send_msg_deleted(&mut self, data: &[u8]) -> io::Result<()> {
        (**self).send_msg_deleted(data)
    }
}

impl<W: MsgInfoSender> MsgInfoSender for CountingWriter<W> {
    fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
        self.inner_ref_mut().send_msg_info(data)
    }

    fn send_msg_error_xfer(&mut self, data: &[u8]) -> io::Result<()> {
        self.inner_ref_mut().send_msg_error_xfer(data)
    }

    fn send_msg_deleted(&mut self, data: &[u8]) -> io::Result<()> {
        self.inner_ref_mut().send_msg_deleted(data)
    }
}
