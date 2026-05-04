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
/// Writers that support multiplexed output (like [`ServerWriter`] and
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
}

impl<T: MsgInfoSender + ?Sized> MsgInfoSender for &mut T {
    fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
        (**self).send_msg_info(data)
    }
}

impl<W: MsgInfoSender> MsgInfoSender for CountingWriter<W> {
    fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
        self.inner_ref_mut().send_msg_info(data)
    }
}
