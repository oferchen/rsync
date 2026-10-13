//! Multiplexed message framing for the rsync protocol.
//!
//! Rsync interleaves file data with control messages (errors, warnings, stats)
//! over a single byte stream using a 4-byte little-endian envelope header.
//! This module provides both low-level frame I/O ([`send_msg`], [`recv_msg`])
//! and higher-level wrappers ([`MplexReader`], [`MplexWriter`]) that implement
//! `std::io::Read` and `std::io::Write` with transparent demultiplexing.
//!
//! # Wire Format
//!
//! Each multiplexed frame consists of:
//! - **Header** (4 bytes LE): tag byte (message code + `MPLEX_BASE`) in bits 24-31,
//!   payload length in bits 0-23
//! - **Payload**: up to [`MAX_PAYLOAD_LENGTH`](crate::MAX_PAYLOAD_LENGTH) bytes
//!
//! # Upstream Reference
//!
//! - `io.c:send_msg()` - frames an out-of-band message with its 4-byte header
//! - `io.c:perform_io()` - wraps buffered output in `MSG_DATA` frames
//! - `io.c:read_a_msg()` - decodes an inbound header and dispatches by tag

mod borrowed;
#[cfg(feature = "async")]
mod codec;
mod frame;
mod helpers;
mod io;
mod reader;
mod writer;

#[cfg(test)]
mod tests;

pub use borrowed::{BorrowedMessageFrame, BorrowedMessageFrames};
#[cfg(feature = "async")]
pub use codec::MultiplexCodec;
pub use frame::MessageFrame;
#[cfg(feature = "tokio-transfer")]
pub use io::recv_msg_into_async;
pub use io::{
    FrameReader, recv_msg, recv_msg_into, send_frame, send_keepalive, send_msg, send_msgs_vectored,
};
pub use reader::MplexReader;
pub use writer::MplexWriter;
