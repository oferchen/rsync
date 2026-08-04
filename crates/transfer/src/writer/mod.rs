//! Server-side writer that can switch between plain and multiplexed modes.
//!
//! Provides [`ServerWriter`] (internal) and [`CountingWriter`] (public).
//! [`ServerWriter`] starts in plain mode and can be upgraded to multiplex
//! or compressed-multiplex mode at runtime, matching upstream rsync's
//! `io_start_multiplex_out()` / `io_start_buffering_out()` transitions.
//!
//! # Submodules
//!
//! - `server` - Mode-switching writer enum dispatching plain, multiplex, and compressed I/O.
//! - `multiplex` - Buffered writer that frames output in `MSG_DATA` multiplex frames.
//! - `msg_info` - Trait for sending `MSG_INFO` protocol messages through multiplexed streams.
//! - `counting` - Byte-counting writer wrapper for transfer statistics.
//! - `throttle` - Bandwidth-limiting writer decorator for the sender socket.
//! - `discard` - Dead-end sink for the batch local-replay receiver (no live peer).

mod counting;
mod discard;
mod msg_info;
pub(crate) mod multiplex;
mod server;
mod throttle;

pub use self::counting::CountingWriter;
pub use self::discard::DiscardSink;
pub use self::msg_info::MsgInfoSender;
pub use self::multiplex::BatchRoute;
pub use self::server::{ServerWriter, shutdown_send_side};
pub use self::throttle::ThrottlingWriter;

#[cfg(test)]
mod tests;
