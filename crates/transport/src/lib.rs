#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! Transport-level helpers for the Rust rsync reimplementation.
//!
//! The crate currently focuses on negotiation sniffing, providing wrappers that
//! preserve the bytes consumed while detecting whether a connection speaks the
//! legacy ASCII or binary handshake. Future modules will extend this facade to
//! cover the SSH stdio transport and the `rsync://` daemon loop while keeping
//! higher layers agnostic to the underlying I/O implementation details.

mod binary;
mod daemon;
mod negotiation;
mod session;

pub use binary::{
    BinaryHandshake, negotiate_binary_session, negotiate_binary_session_with_sniffer,
};
pub use daemon::{
    LegacyDaemonHandshake, negotiate_legacy_daemon_session,
    negotiate_legacy_daemon_session_with_sniffer,
};
pub use negotiation::{
    NegotiatedStream, NegotiatedStreamParts, TryMapInnerError, sniff_negotiation_stream,
    sniff_negotiation_stream_with_sniffer,
};
pub use session::{
    SessionHandshake, SessionHandshakeParts, negotiate_session, negotiate_session_parts,
    negotiate_session_parts_with_sniffer, negotiate_session_with_sniffer,
};
