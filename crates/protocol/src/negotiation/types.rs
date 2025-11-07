use core::fmt;
use core::str::FromStr;
use std::io;

/// Error returned when the caller-provided slice cannot hold the buffered negotiation prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

impl fmt::Display for BufferedPrefixTooSmall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "provided buffer of length {} is too small for negotiation prefix (requires {})",
            self.available, self.required
        )
    }
}

impl std::error::Error for BufferedPrefixTooSmall {}

/// Error category produced when parsing a [`NegotiationPrologue`] from text fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseNegotiationPrologueErrorKind {
    /// The provided string was empty after trimming ASCII whitespace.
    Empty,
    /// The provided string did not match a known negotiation identifier.
    Invalid,
}

/// Error returned when parsing a [`NegotiationPrologue`] from text fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseNegotiationPrologueError {
    kind: ParseNegotiationPrologueErrorKind,
}

impl ParseNegotiationPrologueError {
    const fn new(kind: ParseNegotiationPrologueErrorKind) -> Self {
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

impl std::error::Error for ParseNegotiationPrologueError {}

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
/// use oc_rsync_protocol::{NegotiationPrologue, ParseNegotiationPrologueError};
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
