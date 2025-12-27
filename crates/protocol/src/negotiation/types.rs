use ::core::fmt;
use ::core::str::FromStr;
use std::io;
use thiserror::Error;

/// Error returned when the caller-provided slice cannot hold the buffered negotiation prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error(
    "provided buffer of length {available} is too small for negotiation prefix (requires {required})"
)]
pub struct BufferedPrefixTooSmall {
    required: usize,
    available: usize,
}

impl BufferedPrefixTooSmall {
    /// Creates an error describing the required and available capacities.
    #[must_use]
    pub const fn new(required: usize, available: usize) -> Self {
        Self {
            required,
            available,
        }
    }

    /// Returns the number of bytes required to copy the buffered prefix.
    #[must_use]
    pub const fn required(self) -> usize {
        self.required
    }

    /// Returns the caller-provided capacity.
    #[must_use]
    pub const fn available(self) -> usize {
        self.available
    }

    /// Returns how many additional bytes are required to hold the buffered prefix.
    ///
    /// When callers supply a scratch buffer that is too small to receive the sniffed
    /// negotiation prefix, upstream rsync reports the exact deficit so operators can
    /// size their allocations appropriately. Mirroring that behavior keeps diagnostics
    /// consistent across implementations and allows higher layers to surface actionable
    /// guidance without reimplementing the subtraction logic. The value is saturated at
    /// zero to tolerate defensive checks that may construct the error even when the
    /// provided capacity already exceeds the requirement.
    #[must_use]
    pub const fn missing(self) -> usize {
        self.required.saturating_sub(self.available)
    }
}

/// Error category produced when parsing a [`NegotiationPrologue`] from text fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseNegotiationPrologueErrorKind {
    /// The provided string was empty after trimming ASCII whitespace.
    Empty,
    /// The provided string did not match a known negotiation identifier.
    Invalid,
}

/// Error returned when parsing a [`NegotiationPrologue`] from text fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub struct ParseNegotiationPrologueError {
    kind: ParseNegotiationPrologueErrorKind,
}

impl ParseNegotiationPrologueError {
    pub(crate) const fn new(kind: ParseNegotiationPrologueErrorKind) -> Self {
        Self { kind }
    }

    /// Returns the classification describing why parsing failed.
    #[must_use]
    pub const fn kind(self) -> ParseNegotiationPrologueErrorKind {
        self.kind
    }
}

impl fmt::Display for ParseNegotiationPrologueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ParseNegotiationPrologueErrorKind::Empty => {
                f.write_str("negotiation prologue identifier is empty")
            }
            ParseNegotiationPrologueErrorKind::Invalid => f.write_str(
                "unrecognized negotiation prologue identifier (expected need-more-data, \
                     legacy-ascii, or binary)",
            ),
        }
    }
}

impl From<BufferedPrefixTooSmall> for io::Error {
    fn from(err: BufferedPrefixTooSmall) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, err)
    }
}

/// Classification of the negotiation prologue received from a peer.
///
/// Upstream rsync distinguishes between two negotiation styles:
///
/// * Legacy ASCII greetings that begin with `@RSYNCD:`. These are produced by
///   peers that only understand protocols older than 30.
/// * Binary handshakes used by newer clients and daemons.
///
/// The detection helper mirrors upstream's lightweight peek: if the very first
/// byte equals `b'@'`, the stream is treated as a legacy greeting (subject to
/// later validation). Otherwise the exchange proceeds in binary mode. When no
/// data has been observed yet, the helper reports
/// [`NegotiationPrologue::NeedMoreData`].
///
/// # Examples
///
/// Parse the textual identifier produced by [`NegotiationPrologue::as_str`] back into the
/// corresponding enum variant.
///
/// ```
/// use std::str::FromStr;
/// use protocol::{NegotiationPrologue, ParseNegotiationPrologueError};
///
/// let legacy = NegotiationPrologue::from_str(" legacy-ascii ")?;
/// assert!(legacy.is_legacy());
/// # Ok::<_, ParseNegotiationPrologueError>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NegotiationPrologue {
    /// There is not enough buffered data to determine the negotiation style.
    NeedMoreData,
    /// The peer is speaking the legacy ASCII `@RSYNCD:` protocol.
    LegacyAscii,
    /// The peer is speaking the modern binary negotiation protocol.
    Binary,
}

impl NegotiationPrologue {
    /// Returns a human-readable description of the negotiation style.
    ///
    /// The returned string mirrors the concise identifiers used throughout the
    /// protocol crate when rendering diagnostics. Logging subsystems can use
    /// the value directly without re-implementing the mapping from enum
    /// variants to textual tags, keeping the terminology consistent across the
    /// codebase.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NeedMoreData => "need-more-data",
            Self::LegacyAscii => "legacy-ascii",
            Self::Binary => "binary",
        }
    }

    /// Returns `true` when the negotiation style has been determined.
    ///
    /// Upstream rsync peeks at the initial byte(s) and proceeds immediately once the
    /// transport yields a decision. Centralizing the predicate keeps higher layers from
    /// duplicating `matches!` checks and mirrors the explicit boolean helpers commonly
    /// found in the C implementation.
    #[must_use = "check whether the negotiation style has been determined"]
    #[inline]
    pub const fn is_decided(self) -> bool {
        !matches!(self, Self::NeedMoreData)
    }

    /// Reports whether additional bytes must be read before the negotiation style is known.
    #[must_use = "determine if additional negotiation bytes must be read"]
    #[inline]
    pub const fn requires_more_data(self) -> bool {
        matches!(self, Self::NeedMoreData)
    }

    /// Returns `true` when the peer is using the legacy ASCII `@RSYNCD:` negotiation.
    #[must_use = "check whether the peer selected the legacy ASCII negotiation"]
    #[inline]
    pub const fn is_legacy(self) -> bool {
        matches!(self, Self::LegacyAscii)
    }

    /// Returns `true` when the peer is using the binary negotiation introduced in protocol 30.
    #[must_use = "check whether the peer selected the binary negotiation"]
    #[inline]
    pub const fn is_binary(self) -> bool {
        matches!(self, Self::Binary)
    }

    /// Classifies a negotiation prologue using the very first byte observed on
    /// the transport.
    ///
    /// Upstream rsync performs a single-byte peek: a leading `b'@'` selects the
    /// legacy ASCII `@RSYNCD:` handshake, while any other value triggers the
    /// binary negotiation introduced in protocol 30. The helper mirrors that
    /// branch so call sites that already ensured at least one byte is buffered
    /// can reuse the canonical mapping without duplicating the literal or
    /// resorting to ad-hoc comparisons.
    #[must_use = "determine the negotiation mode selected by the first byte"]
    #[inline]
    pub const fn from_initial_byte(byte: u8) -> Self {
        if byte == b'@' {
            Self::LegacyAscii
        } else {
            Self::Binary
        }
    }
}

impl From<NegotiationPrologue> for &'static str {
    fn from(prologue: NegotiationPrologue) -> Self {
        prologue.as_str()
    }
}

impl Default for NegotiationPrologue {
    /// Returns [`NegotiationPrologue::NeedMoreData`], matching the undecided state upstream
    /// rsync uses before the first byte is observed.
    fn default() -> Self {
        Self::NeedMoreData
    }
}

impl fmt::Display for NegotiationPrologue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for NegotiationPrologue {
    type Err = ParseNegotiationPrologueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();

        if trimmed.is_empty() {
            return Err(ParseNegotiationPrologueError::new(
                ParseNegotiationPrologueErrorKind::Empty,
            ));
        }

        match trimmed {
            "need-more-data" => Ok(Self::NeedMoreData),
            "legacy-ascii" => Ok(Self::LegacyAscii),
            "binary" => Ok(Self::Binary),
            _ => Err(ParseNegotiationPrologueError::new(
                ParseNegotiationPrologueErrorKind::Invalid,
            )),
        }
    }
}

/// Determines whether the peer is performing the legacy ASCII negotiation or
/// the modern binary handshake.
///
/// The caller provides the initial bytes read from the transport without
/// consuming them. The helper follows upstream rsync's logic:
///
/// * If no data has been received yet, more bytes are required before a
///   decision can be made.
/// * If the first byte is `b'@'`, the peer is assumed to speak the legacy
///   protocol. Callers should then parse the banner via
///   [`parse_legacy_daemon_greeting_bytes`](crate::parse_legacy_daemon_greeting_bytes),
///   which will surface malformed input as
///   [`NegotiationError::MalformedLegacyGreeting`](crate::NegotiationError::MalformedLegacyGreeting).
/// * Otherwise, the exchange uses the binary negotiation.
#[must_use]
pub fn detect_negotiation_prologue(buffer: &[u8]) -> NegotiationPrologue {
    match buffer.first().copied() {
        Some(byte) => NegotiationPrologue::from_initial_byte(byte),
        None => NegotiationPrologue::NeedMoreData,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for BufferedPrefixTooSmall
    #[test]
    fn buffered_prefix_too_small_new() {
        let err = BufferedPrefixTooSmall::new(100, 50);
        assert_eq!(err.required(), 100);
        assert_eq!(err.available(), 50);
    }

    #[test]
    fn buffered_prefix_too_small_missing() {
        let err = BufferedPrefixTooSmall::new(100, 50);
        assert_eq!(err.missing(), 50);
    }

    #[test]
    fn buffered_prefix_too_small_missing_saturates() {
        // When available exceeds required, missing should be 0
        let err = BufferedPrefixTooSmall::new(50, 100);
        assert_eq!(err.missing(), 0);
    }

    #[test]
    fn buffered_prefix_too_small_missing_exact() {
        let err = BufferedPrefixTooSmall::new(100, 100);
        assert_eq!(err.missing(), 0);
    }

    #[test]
    fn buffered_prefix_too_small_display() {
        let err = BufferedPrefixTooSmall::new(100, 50);
        let msg = format!("{err}");
        assert!(msg.contains("100"));
        assert!(msg.contains("50"));
    }

    #[test]
    fn buffered_prefix_too_small_into_io_error() {
        let err = BufferedPrefixTooSmall::new(100, 50);
        let io_err: io::Error = err.into();
        assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn buffered_prefix_too_small_clone() {
        let err = BufferedPrefixTooSmall::new(100, 50);
        assert_eq!(err.clone(), err);
    }

    #[test]
    fn buffered_prefix_too_small_copy() {
        let err = BufferedPrefixTooSmall::new(100, 50);
        let copy = err;
        assert_eq!(err, copy);
    }

    // Tests for ParseNegotiationPrologueError
    #[test]
    fn parse_error_empty_kind() {
        let err = ParseNegotiationPrologueError::new(ParseNegotiationPrologueErrorKind::Empty);
        assert_eq!(err.kind(), ParseNegotiationPrologueErrorKind::Empty);
    }

    #[test]
    fn parse_error_invalid_kind() {
        let err = ParseNegotiationPrologueError::new(ParseNegotiationPrologueErrorKind::Invalid);
        assert_eq!(err.kind(), ParseNegotiationPrologueErrorKind::Invalid);
    }

    #[test]
    fn parse_error_display_empty() {
        let err = ParseNegotiationPrologueError::new(ParseNegotiationPrologueErrorKind::Empty);
        let msg = format!("{err}");
        assert!(msg.contains("empty"));
    }

    #[test]
    fn parse_error_display_invalid() {
        let err = ParseNegotiationPrologueError::new(ParseNegotiationPrologueErrorKind::Invalid);
        let msg = format!("{err}");
        assert!(msg.contains("unrecognized"));
    }

    #[test]
    fn parse_error_clone() {
        let err = ParseNegotiationPrologueError::new(ParseNegotiationPrologueErrorKind::Empty);
        assert_eq!(err.clone(), err);
    }

    // Tests for NegotiationPrologue
    #[test]
    fn negotiation_prologue_as_str() {
        assert_eq!(NegotiationPrologue::NeedMoreData.as_str(), "need-more-data");
        assert_eq!(NegotiationPrologue::LegacyAscii.as_str(), "legacy-ascii");
        assert_eq!(NegotiationPrologue::Binary.as_str(), "binary");
    }

    #[test]
    fn negotiation_prologue_is_decided() {
        assert!(!NegotiationPrologue::NeedMoreData.is_decided());
        assert!(NegotiationPrologue::LegacyAscii.is_decided());
        assert!(NegotiationPrologue::Binary.is_decided());
    }

    #[test]
    fn negotiation_prologue_requires_more_data() {
        assert!(NegotiationPrologue::NeedMoreData.requires_more_data());
        assert!(!NegotiationPrologue::LegacyAscii.requires_more_data());
        assert!(!NegotiationPrologue::Binary.requires_more_data());
    }

    #[test]
    fn negotiation_prologue_is_legacy() {
        assert!(!NegotiationPrologue::NeedMoreData.is_legacy());
        assert!(NegotiationPrologue::LegacyAscii.is_legacy());
        assert!(!NegotiationPrologue::Binary.is_legacy());
    }

    #[test]
    fn negotiation_prologue_is_binary() {
        assert!(!NegotiationPrologue::NeedMoreData.is_binary());
        assert!(!NegotiationPrologue::LegacyAscii.is_binary());
        assert!(NegotiationPrologue::Binary.is_binary());
    }

    #[test]
    fn negotiation_prologue_from_initial_byte_at_sign() {
        assert_eq!(
            NegotiationPrologue::from_initial_byte(b'@'),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn negotiation_prologue_from_initial_byte_other() {
        assert_eq!(
            NegotiationPrologue::from_initial_byte(b'\x00'),
            NegotiationPrologue::Binary
        );
        assert_eq!(
            NegotiationPrologue::from_initial_byte(b'A'),
            NegotiationPrologue::Binary
        );
        assert_eq!(
            NegotiationPrologue::from_initial_byte(0xFF),
            NegotiationPrologue::Binary
        );
    }

    #[test]
    fn negotiation_prologue_from_str_valid() {
        assert_eq!(
            NegotiationPrologue::from_str("need-more-data").unwrap(),
            NegotiationPrologue::NeedMoreData
        );
        assert_eq!(
            NegotiationPrologue::from_str("legacy-ascii").unwrap(),
            NegotiationPrologue::LegacyAscii
        );
        assert_eq!(
            NegotiationPrologue::from_str("binary").unwrap(),
            NegotiationPrologue::Binary
        );
    }

    #[test]
    fn negotiation_prologue_from_str_with_whitespace() {
        assert_eq!(
            NegotiationPrologue::from_str("  legacy-ascii  ").unwrap(),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn negotiation_prologue_from_str_empty() {
        let result = NegotiationPrologue::from_str("");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            ParseNegotiationPrologueErrorKind::Empty
        );
    }

    #[test]
    fn negotiation_prologue_from_str_whitespace_only() {
        let result = NegotiationPrologue::from_str("   ");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            ParseNegotiationPrologueErrorKind::Empty
        );
    }

    #[test]
    fn negotiation_prologue_from_str_invalid() {
        let result = NegotiationPrologue::from_str("unknown");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            ParseNegotiationPrologueErrorKind::Invalid
        );
    }

    #[test]
    fn negotiation_prologue_default() {
        assert_eq!(
            NegotiationPrologue::default(),
            NegotiationPrologue::NeedMoreData
        );
    }

    #[test]
    fn negotiation_prologue_display() {
        assert_eq!(
            format!("{}", NegotiationPrologue::NeedMoreData),
            "need-more-data"
        );
        assert_eq!(
            format!("{}", NegotiationPrologue::LegacyAscii),
            "legacy-ascii"
        );
        assert_eq!(format!("{}", NegotiationPrologue::Binary), "binary");
    }

    #[test]
    fn negotiation_prologue_into_str() {
        let s: &'static str = NegotiationPrologue::Binary.into();
        assert_eq!(s, "binary");
    }

    #[test]
    fn negotiation_prologue_clone() {
        let p = NegotiationPrologue::Binary;
        assert_eq!(p.clone(), p);
    }

    #[test]
    fn negotiation_prologue_copy() {
        let p = NegotiationPrologue::Binary;
        let copy = p;
        assert_eq!(p, copy);
    }

    #[test]
    fn negotiation_prologue_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(NegotiationPrologue::Binary);
        set.insert(NegotiationPrologue::LegacyAscii);
        assert!(set.contains(&NegotiationPrologue::Binary));
        assert!(set.contains(&NegotiationPrologue::LegacyAscii));
    }

    // Tests for detect_negotiation_prologue
    #[test]
    fn detect_negotiation_prologue_empty() {
        assert_eq!(
            detect_negotiation_prologue(&[]),
            NegotiationPrologue::NeedMoreData
        );
    }

    #[test]
    fn detect_negotiation_prologue_at_sign() {
        assert_eq!(
            detect_negotiation_prologue(b"@RSYNCD: 31.0"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn detect_negotiation_prologue_binary() {
        assert_eq!(
            detect_negotiation_prologue(&[0x00, 0x01, 0x02]),
            NegotiationPrologue::Binary
        );
    }

    #[test]
    fn detect_negotiation_prologue_single_byte() {
        assert_eq!(
            detect_negotiation_prologue(b"@"),
            NegotiationPrologue::LegacyAscii
        );
        assert_eq!(
            detect_negotiation_prologue(b"A"),
            NegotiationPrologue::Binary
        );
    }
}
