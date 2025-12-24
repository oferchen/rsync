//! Session negotiation facade bridging binary and legacy handshake flows.
//!
//! This module re-exports the high-level [`SessionHandshake`] type together with helper
//! functions that detect the negotiation style, perform the binary or legacy handshake,
//! and expose variant-specific metadata through [`SessionHandshakeParts`]. The
//! implementation is decomposed across the [`handshake`] and [`parts`] submodules to keep
//! individual files below the workspace line-count guidelines while preserving the
//! original public API.

mod handshake;
mod parts;

pub use handshake::{
    SessionHandshake, negotiate_session, negotiate_session_from_stream, negotiate_session_parts,
    negotiate_session_parts_from_stream, negotiate_session_parts_with_sniffer,
    negotiate_session_with_sniffer,
};
pub use parts::SessionHandshakeParts;

#[cfg(test)]
mod tests;
