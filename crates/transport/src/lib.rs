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

mod negotiation;

pub use negotiation::{NegotiatedStream, NegotiatedStreamParts, sniff_negotiation_stream};
