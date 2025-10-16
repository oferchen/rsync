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
                let (oldest, newest) = crate::version::ProtocolVersion::supported_range_bounds();
                write!(
                    f,
                    "peer advertised unsupported rsync protocol version {} (valid range {}-{})",
                    version, oldest, newest,
                )
            }
            Self::MalformedLegacyGreeting { input } => {
                write!(f, "malformed legacy rsync daemon greeting: {:?}", input)
            }
        }
    }
}

impl std::error::Error for NegotiationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::ProtocolVersion;

    #[test]
    fn display_formats_no_mutual_protocol_context() {
        let err = NegotiationError::NoMutualProtocol {
            peer_versions: vec![29, 30],
        };

        assert_eq!(
            err.to_string(),
            format!(
                "no mutual rsync protocol version; peer offered {:?}, we support {:?}",
                vec![29, 30],
                SUPPORTED_PROTOCOLS
            )
        );
    }

    #[test]
    fn display_mentions_supported_range_for_unsupported_versions() {
        let err = NegotiationError::UnsupportedVersion(27);
        let rendered = err.to_string();

        assert!(rendered.contains("peer advertised unsupported rsync protocol version 27"));
        assert!(rendered.contains("valid range"));
        assert!(rendered.contains(&ProtocolVersion::OLDEST.as_u8().to_string()));
        assert!(rendered.contains(&ProtocolVersion::NEWEST.as_u8().to_string()));
    }

    #[test]
    fn display_echoes_malformed_legacy_greetings() {
        let err = NegotiationError::MalformedLegacyGreeting {
            input: "@RSYNCD: ???".to_owned(),
        };

        assert_eq!(
            err.to_string(),
            "malformed legacy rsync daemon greeting: \"@RSYNCD: ???\""
        );
    }
}
