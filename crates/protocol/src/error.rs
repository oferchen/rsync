use ::core::fmt;

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
    UnsupportedVersion(u32),
    /// A legacy ASCII daemon greeting could not be parsed.
    MalformedLegacyGreeting {
        /// The raw greeting text without trailing newlines.
        input: String,
    },
}

impl NegotiationError {
    /// Returns the peer-advertised protocol versions that failed to overlap with our
    /// supported set.
    ///
    /// Upstream rsync surfaces the rejected versions when no mutual protocol exists so users can
    /// reason about the mismatch. Exposing the slice keeps higher layers from cloning the vector
    /// simply to inspect the diagnostic context while retaining ownership of the error value.
    #[must_use]
    pub fn peer_versions(&self) -> Option<&[u8]> {
        match self {
            Self::NoMutualProtocol { peer_versions } => Some(peer_versions.as_slice()),
            _ => None,
        }
    }

    /// Returns the unsupported protocol version advertised by the peer, if any.
    ///
    /// When upstream rsync aborts the negotiation due to an out-of-range version it surfaces the
    /// offending byte directly in the diagnostic. The accessor allows callers to recover that
    /// value without pattern matching on [`NegotiationError::UnsupportedVersion`].
    #[must_use]
    pub const fn unsupported_version(&self) -> Option<u32> {
        match self {
            Self::UnsupportedVersion(version) => Some(*version),
            _ => None,
        }
    }

    /// Returns the malformed legacy greeting that triggered a parsing failure, if available.
    ///
    /// Daemon negotiations frequently log the offending banner to aid debugging. Providing a
    /// borrowed view keeps the same capability in higher layers without forcing them to clone the
    /// string owned by the error value.
    #[must_use]
    pub fn malformed_legacy_greeting(&self) -> Option<&str> {
        match self {
            Self::MalformedLegacyGreeting { input } => Some(input.as_str()),
            _ => None,
        }
    }
}

impl fmt::Display for NegotiationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMutualProtocol { peer_versions } => {
                let supported = ProtocolVersion::supported_protocol_numbers();
                write!(
                    f,
                    // clippy(uninlined_format_args): inline variables into the format string.
                    "no mutual rsync protocol version; peer offered {peer_versions:?}, we support {supported:?}"
                )
            }
            Self::UnsupportedVersion(version) => {
                let (oldest, newest) = ProtocolVersion::supported_range_bounds();
                write!(
                    f,
                    // clippy(uninlined_format_args): inline variables into the format string.
                    "peer advertised unsupported rsync protocol version {version} (valid range {oldest}-{newest})"
                )
            }
            Self::MalformedLegacyGreeting { input } => {
                // clippy(uninlined_format_args): inline variables into the format string.
                write!(f, "malformed legacy rsync daemon greeting: {input:?}")
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
    fn accessors_expose_variant_context() {
        let no_mutual = NegotiationError::NoMutualProtocol {
            peer_versions: vec![29, 30],
        };
        assert_eq!(no_mutual.peer_versions(), Some(&[29, 30][..]));
        assert_eq!(no_mutual.unsupported_version(), None);
        assert_eq!(no_mutual.malformed_legacy_greeting(), None);

        let unsupported = NegotiationError::UnsupportedVersion(27);
        assert_eq!(unsupported.peer_versions(), None);
        assert_eq!(unsupported.unsupported_version(), Some(27));
        assert_eq!(unsupported.malformed_legacy_greeting(), None);

        let malformed = NegotiationError::MalformedLegacyGreeting {
            input: "@RSYNCD: ???".to_owned(),
        };
        assert_eq!(malformed.peer_versions(), None);
        assert_eq!(malformed.unsupported_version(), None);
        assert_eq!(malformed.malformed_legacy_greeting(), Some("@RSYNCD: ???"));
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
