//! Batched acknowledgment system for reducing network overhead.
//!
//! This module implements ACK batching to reduce the number of round-trips
//! during rsync file transfers. Instead of sending an acknowledgment for
//! each file individually, multiple ACKs are batched together and sent
//! as a single network message.
//!
//! # Design
//!
//! The [`AckBatcher`] accumulates file transfer acknowledgments and flushes
//! them when any of the following conditions are met:
//!
//! 1. **Count threshold** - batch reaches N files (default: 16)
//! 2. **Time threshold** - T milliseconds have elapsed since first ACK (default: 50ms)
//! 3. **Error condition** - an error requires immediate ACK
//! 4. **Explicit flush** - caller requests immediate flush
//!
//! # Wire Format
//!
//! Batched ACKs are encoded as a sequence of ACK entries in the multiplex stream:
//!
//! ```text
//! count: u16 LE (number of ACKs in batch)
//! ack[0]: ndx(i32 LE) + status(u8) [+ error_len(u16 LE) + error_msg]
//! ack[1]: ndx(i32 LE) + status(u8) [+ error_len(u16 LE) + error_msg]
//! ...
//! ```
//!
//! # Protocol Compatibility
//!
//! Batched ACKs are only used when both sides support them (detected via
//! capability negotiation). When communicating with legacy rsync, the batcher
//! falls back to individual ACKs.

mod batcher;
mod types;

#[cfg(test)]
mod tests;

pub use batcher::AckBatcher;
pub use types::{
    AckBatcherConfig, AckBatcherStats, AckEntry, AckStatus, DEFAULT_BATCH_SIZE,
    DEFAULT_BATCH_TIMEOUT_MS, MAX_BATCH_SIZE, MAX_BATCH_TIMEOUT_MS, MIN_BATCH_SIZE,
};
