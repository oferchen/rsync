#![allow(clippy::module_name_repetitions)]

//! Binary rsync protocol negotiation.
//!
//! This module implements the binary handshake used by remote-shell (SSH)
//! transports. It detects a binary prologue, exchanges 4-byte little-endian
//! protocol versions, and reads compatibility flags for protocol 30+.

mod handshake;
mod negotiate;
mod parts;

pub use handshake::BinaryHandshake;
pub use negotiate::{
    negotiate_binary_session, negotiate_binary_session_from_stream,
    negotiate_binary_session_with_sniffer,
};
pub use parts::BinaryHandshakeParts;

#[cfg(test)]
mod tests;
