use core::fmt;

use crate::version::SUPPORTED_PROTOCOLS;

/// Errors that can occur while attempting to negotiate a protocol version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NegotiationError {
    /// None of the peer protocol versions overlap with our supported set.
    NoMutualProtocol {
        /// Versions advertised by the peer (after filtering to the upstream range).
        peer_versions: Vec<u8>,
    },
    /// The peer advertised a protocol version outside the upstream supported range.
    UnsupportedVersion(u8),
    /// A legacy ASCII daemon greeting could not be parsed.
    MalformedLegacyGreeting {
        /// The raw greeting text without trailing newlines.
        input: String,
    },
}

impl fmt::Display for NegotiationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMutualProtocol { peer_versions } => {
                write!(
                    f,
                    "no mutual rsync protocol version; peer offered {:?}, we support {:?}",
                    peer_versions, SUPPORTED_PROTOCOLS
                )
            }
            Self::UnsupportedVersion(version) => {
                write!(
                    f,
                    "peer advertised unsupported rsync protocol version {} (valid range {}-{})",
                    version,
                    crate::version::ProtocolVersion::OLDEST.as_u8(),
                    crate::version::ProtocolVersion::NEWEST.as_u8()
                )
            }
            Self::MalformedLegacyGreeting { input } => {
                write!(f, "malformed legacy rsync daemon greeting: {:?}", input)
            }
        }
    }
}

impl std::error::Error for NegotiationError {}
