//! Multiplexed message envelope encoding and decoding.
//!
//! Rsync multiplexes control messages and file data over a single byte stream
//! using a 4-byte little-endian header. The header encodes a message tag
//! (derived from [`MessageCode`]) and a 24-bit payload length, allowing up to
//! [`MAX_PAYLOAD_LENGTH`] bytes per frame.
//!
//! This module provides the low-level types for working with individual
//! envelope headers. Higher-level frame I/O is available through the
//! [`crate::multiplex`] helpers ([`crate::send_msg`], [`crate::recv_msg`],
//! etc.).

mod constants;
mod conversion;
mod error;
mod header;
mod log_code;
mod message_code;

pub use constants::{HEADER_LEN, MAX_PAYLOAD_LENGTH, MPLEX_BASE};
pub use conversion::LogCodeConversionError;
pub use error::EnvelopeError;
pub use header::MessageHeader;
pub use log_code::{LogCode, ParseLogCodeError};
pub use message_code::{MessageCode, ParseMessageCodeError};

pub(crate) use constants::PAYLOAD_MASK;

#[cfg(test)]
mod tests;
