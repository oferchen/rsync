//! Protocol negotiation between rsync peers.
//!
//! This module implements the initial handshake that determines which protocol
//! version and capabilities a session will use. Upstream rsync has two
//! handshake styles:
//!
//! - **ASCII greeting** (`@RSYNCD:`) - the daemon (`rsync://`) transport, at
//!   every protocol version.
//! - **Binary handshake** - the remote-shell transport, which exchanges the
//!   protocol version as a 4-byte integer.
//!
//! At protocol 30 and later both styles continue with the binary compatibility
//! flags and algorithm negotiation.
//!
//! The [`detect_negotiation_prologue`] function and the incremental
//! [`NegotiationPrologueDetector`] classify the exchange style from the first
//! byte(s) observed on the transport. Once the style is known, higher layers
//! proceed with either the legacy greeting parser or the binary capability
//! negotiation provided by [`negotiate_capabilities`].

mod capabilities;
mod detector;
mod sniffer;
mod types;

pub use capabilities::{
    ChecksumAlgorithm, CompressionAlgorithm, NegotiationConfig, NegotiationResult,
    negotiate_capabilities, negotiate_capabilities_with_override,
};
pub use detector::NegotiationPrologueDetector;
pub use sniffer::{
    NegotiationPrologueSniffer, read_and_parse_legacy_daemon_greeting,
    read_and_parse_legacy_daemon_greeting_details, read_legacy_daemon_line,
};
pub use types::{
    BufferedPrefixTooSmall, NegotiationPrologue, ParseNegotiationPrologueError,
    ParseNegotiationPrologueErrorKind, detect_negotiation_prologue,
};

#[cfg(test)]
mod tests;
