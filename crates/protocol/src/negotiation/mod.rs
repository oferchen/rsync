//! Protocol negotiation between rsync peers.
//!
//! This module implements the initial handshake that determines which protocol
//! version and capabilities a session will use. Upstream rsync supports two
//! negotiation styles:
//!
//! - **Legacy ASCII** (`@RSYNCD:` greeting) -- used by peers speaking protocol
//!   versions older than 30.
//! - **Binary handshake** -- introduced in protocol 30, where capabilities and
//!   algorithm choices are exchanged as binary-encoded values.
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
    ChecksumAlgorithm, CompressionAlgorithm, NegotiationResult, negotiate_capabilities,
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
