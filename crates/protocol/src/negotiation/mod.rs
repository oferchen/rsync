mod detector;
mod sniffer;
mod types;

pub use detector::NegotiationPrologueDetector;
pub use sniffer::{
    NegotiationPrologueSniffer, read_and_parse_legacy_daemon_greeting, read_legacy_daemon_line,
};
pub use types::{
    BufferedPrefixTooSmall, NegotiationPrologue, ParseNegotiationPrologueError,
    ParseNegotiationPrologueErrorKind, detect_negotiation_prologue,
};

#[cfg(test)]
mod tests;
