use core::fmt;

use crate::version::ProtocolVersion;
use std::io;

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
                let supported = ProtocolVersion::supported_protocol_numbers();
                write!(
                    f,
                    "no mutual rsync protocol version; peer offered {:?}, we support {:?}",
                    peer_versions, supported
                )
            }
            Self::UnsupportedVersion(version) => {
                let (oldest, newest) = ProtocolVersion::supported_range_bounds();
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

impl From<NegotiationError> for io::Error {
    fn from(err: NegotiationError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                ProtocolVersion::supported_protocol_numbers()
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

    #[test]
    fn converts_to_io_error_preserving_kind_and_source() {
        let err = NegotiationError::UnsupportedVersion(27);
        let io_err: io::Error = err.clone().into();

        assert_eq!(io_err.kind(), io::ErrorKind::InvalidData);

        let source = io_err
            .get_ref()
            .and_then(|src| src.downcast_ref::<NegotiationError>())
            .expect("io::Error must carry NegotiationError source");
        assert_eq!(source, &err);
    }
}
