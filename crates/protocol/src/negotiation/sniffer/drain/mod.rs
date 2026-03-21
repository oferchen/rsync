//! Drain operations for [`NegotiationPrologueSniffer`](super::NegotiationPrologueSniffer).
//!
//! Split into focused submodules by responsibility:
//!
//! - [`take_prefix`] - drain the sniffed negotiation prefix only
//! - [`take_buffered`] - drain all buffered bytes (prefix + remainder)
//! - [`take_remainder`] - drain the buffered remainder while retaining the prefix

mod take_buffered;
mod take_prefix;
mod take_remainder;
