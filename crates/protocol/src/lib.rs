#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! Protocol version selection utilities for the Rust `rsync` reimplementation.
//!
//! Upstream rsync 3.4.1 negotiates protocol versions in the range 28–32.
//! The constants and helpers in this module mirror the upstream defaults
//! so that higher level components can implement byte-identical handshakes.

use core::convert::TryFrom;
use core::fmt;
use core::num::NonZeroU8;
use core::ops::RangeInclusive;

/// Protocol versions supported by the Rust implementation, ordered from
/// newest to oldest as required by upstream rsync's negotiation logic.
pub const SUPPORTED_PROTOCOLS: [u8; 5] = [32, 31, 30, 29, 28];

/// Inclusive range of protocol versions that upstream rsync 3.4.1 understands.
const UPSTREAM_PROTOCOL_RANGE: RangeInclusive<u8> = 28..=32;

/// A single negotiated rsync protocol version.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProtocolVersion(NonZeroU8);

impl ProtocolVersion {
    const fn new_const(value: u8) -> Self {
        match NonZeroU8::new(value) {
            Some(v) => Self(v),
            None => panic!("protocol version must be non-zero"),
        }
    }

    /// The newest protocol version supported by upstream rsync 3.4.1.
    pub const NEWEST: ProtocolVersion = ProtocolVersion::new_const(32);

    /// The oldest protocol version supported by upstream rsync 3.4.1.
    pub const OLDEST: ProtocolVersion = ProtocolVersion::new_const(28);

    /// Returns the raw numeric value represented by this version.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0.get()
    }

    /// Converts a peer-advertised version into the negotiated protocol version.
    ///
    /// Upstream rsync tolerates peers that advertise a protocol newer than it
    /// understands by clamping the negotiated value to its newest supported
    /// protocol. Versions older than [`ProtocolVersion::OLDEST`] remain
    /// unsupported.
    pub fn from_peer_advertisement(value: u8) -> Result<Self, NegotiationError> {
        if value < Self::OLDEST.as_u8() {
            return Err(NegotiationError::UnsupportedVersion(value));
        }

        let clamped = if value > Self::NEWEST.as_u8() {
            Self::NEWEST.as_u8()
        } else {
            value
        };

        match NonZeroU8::new(clamped) {
            Some(non_zero) => Ok(Self(non_zero)),
            None => Err(NegotiationError::UnsupportedVersion(value)),
        }
    }
}

impl TryFrom<u8> for ProtocolVersion {
    type Error = NegotiationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if UPSTREAM_PROTOCOL_RANGE.contains(&value) {
            match NonZeroU8::new(value) {
                Some(non_zero) => Ok(Self(non_zero)),
                None => Err(NegotiationError::UnsupportedVersion(value)),
            }
        } else {
            Err(NegotiationError::UnsupportedVersion(value))
        }
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_u8())
    }
}

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
                    ProtocolVersion::OLDEST.as_u8(),
                    ProtocolVersion::NEWEST.as_u8()
                )
            }
            Self::MalformedLegacyGreeting { input } => {
                write!(f, "malformed legacy rsync daemon greeting: {:?}", input)
            }
        }
    }
}

impl std::error::Error for NegotiationError {}

/// Parses a legacy ASCII daemon greeting of the form `@RSYNCD: <version>`.
///
/// Upstream rsync emits greetings such as `@RSYNCD: 31.0`. The Rust
/// implementation accepts optional fractional suffixes (e.g. `.0`) but only the
/// integer component participates in protocol negotiation. Any trailing carriage
/// returns or line feeds are ignored.
#[must_use]
pub fn parse_legacy_daemon_greeting(line: &str) -> Result<ProtocolVersion, NegotiationError> {
    const PREFIX: &str = "@RSYNCD:";

    let trimmed = line.trim_end_matches(['\r', '\n']);
    let after_prefix =
        trimmed
            .strip_prefix(PREFIX)
            .ok_or_else(|| NegotiationError::MalformedLegacyGreeting {
                input: trimmed.to_owned(),
            })?;

    let remainder = after_prefix.trim_start();
    if remainder.is_empty() {
        return Err(NegotiationError::MalformedLegacyGreeting {
            input: trimmed.to_owned(),
        });
    }

    let mut rest_start = remainder.len();
    for (idx, ch) in remainder.char_indices() {
        if ch.is_ascii_digit() {
            continue;
        }

        rest_start = idx;
        break;
    }

    let digits = &remainder[..rest_start];
    if digits.is_empty() {
        return Err(NegotiationError::MalformedLegacyGreeting {
            input: trimmed.to_owned(),
        });
    }

    let rest = &remainder[rest_start..];
    let mut tail = rest.chars().peekable();
    while let Some(&ch) = tail.peek() {
        if ch == '.' {
            tail.next();
            let mut saw_digit = false;
            while let Some(&fraction_ch) = tail.peek() {
                if fraction_ch.is_ascii_digit() {
                    saw_digit = true;
                    tail.next();
                } else {
                    break;
                }
            }

            if !saw_digit {
                return Err(NegotiationError::MalformedLegacyGreeting {
                    input: trimmed.to_owned(),
                });
            }
        } else if ch.is_whitespace() {
            tail.next();
        } else {
            return Err(NegotiationError::MalformedLegacyGreeting {
                input: trimmed.to_owned(),
            });
        }
    }

    let version: u8 = digits
        .parse()
        .map_err(|_| NegotiationError::MalformedLegacyGreeting {
            input: trimmed.to_owned(),
        })?;

    ProtocolVersion::from_peer_advertisement(version)
}

/// Selects the highest mutual protocol version between the Rust implementation and a peer.
///
/// The caller provides the list of protocol versions advertised by the peer in any order.
/// The function filters the peer list to versions that upstream rsync 3.4.1 recognizes and
/// clamps versions newer than [`ProtocolVersion::NEWEST`] down to the newest supported
/// value, matching upstream tolerance for future releases. Duplicate peer entries and
/// out-of-order announcements are tolerated. If no mutual protocol exists,
/// [`NegotiationError::NoMutualProtocol`] is returned with the filtered peer list for context.
#[must_use]
pub fn select_highest_mutual<I>(peer_versions: I) -> Result<ProtocolVersion, NegotiationError>
where
    I: IntoIterator<Item = u8>,
{
    let mut filtered: Vec<u8> = Vec::new();
    let mut oldest_rejection: Option<u8> = None;

    for version in peer_versions {
        match ProtocolVersion::from_peer_advertisement(version) {
            Ok(proto) => filtered.push(proto.as_u8()),
            Err(NegotiationError::UnsupportedVersion(value))
                if value < ProtocolVersion::OLDEST.as_u8() =>
            {
                match oldest_rejection {
                    Some(current) if value >= current => {}
                    _ => oldest_rejection = Some(value),
                }
            }
            Err(err) => return Err(err),
        }
    }

    filtered.sort_unstable();
    filtered.dedup();

    for ours in SUPPORTED_PROTOCOLS {
        if filtered.binary_search(&ours).is_ok() {
            return ProtocolVersion::try_from(ours);
        }
    }

    if let Some(value) = oldest_rejection {
        return Err(NegotiationError::UnsupportedVersion(value));
    }

    Err(NegotiationError::NoMutualProtocol {
        peer_versions: filtered,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_protocol_is_preferred() {
        let result = select_highest_mutual([32, 31, 30]).expect("must succeed");
        assert_eq!(result, ProtocolVersion::NEWEST);
    }

    #[test]
    fn downgrades_when_peer_lacks_newest() {
        let result = select_highest_mutual([31]).expect("must succeed");
        assert_eq!(result.as_u8(), 31);
    }

    #[test]
    fn reports_no_mutual_protocol() {
        let err = select_highest_mutual(core::iter::empty()).unwrap_err();
        assert_eq!(
            err,
            NegotiationError::NoMutualProtocol {
                peer_versions: vec![]
            }
        );
    }

    #[test]
    fn rejects_zero_protocol_version() {
        let err = select_highest_mutual([0]).unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(0));
    }

    #[test]
    fn parses_legacy_daemon_greeting_with_minor_version() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 31.0\r\n").expect("valid greeting");
        assert_eq!(parsed.as_u8(), 31);
    }

    #[test]
    fn parses_greeting_with_trailing_whitespace() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 30.0   \n").expect("valid greeting");
        assert_eq!(parsed.as_u8(), 30);
    }

    #[test]
    fn rejects_greeting_with_unsupported_version() {
        let err = parse_legacy_daemon_greeting("@RSYNCD: 27.0").unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(27));
    }

    #[test]
    fn clamps_future_peer_versions_in_selection() {
        let negotiated = select_highest_mutual([35, 31]).expect("must clamp to newest");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn clamps_future_versions_in_legacy_greeting() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 40.1\n").expect("must clamp");
        assert_eq!(parsed, ProtocolVersion::NEWEST);
    }

    #[test]
    fn ignores_versions_older_than_supported_when_newer_exists() {
        let negotiated = select_highest_mutual([27, 29, 27]).expect("29 should be selected");
        assert_eq!(negotiated.as_u8(), 29);
    }

    #[test]
    fn reports_unsupported_when_only_too_old_versions_are_offered() {
        let err = select_highest_mutual([27, 26]).unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(26));
    }

    #[test]
    fn rejects_greeting_with_missing_prefix() {
        let err = parse_legacy_daemon_greeting("RSYNCD 32").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn rejects_greeting_without_version_digits() {
        let err = parse_legacy_daemon_greeting("@RSYNCD: .0").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn rejects_greeting_with_fractional_without_digits() {
        let err = parse_legacy_daemon_greeting("@RSYNCD: 31.\n").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn rejects_greeting_with_non_numeric_suffix() {
        let err = parse_legacy_daemon_greeting("@RSYNCD: 31.0beta").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }
}
