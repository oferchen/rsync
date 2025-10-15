#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! Protocol version selection utilities for the Rust `rsync` reimplementation.
//!
//! Upstream rsync 3.4.1 negotiates protocol versions in the range 28–32.
//! The constants and helpers in this module mirror the upstream defaults
//! so that higher level components can implement byte-identical handshakes.

use core::cmp::min;
use core::convert::TryFrom;
use core::fmt;
use core::num::NonZeroU8;
use core::ops::RangeInclusive;

/// Legacy daemon greeting prefix used by rsync versions that speak the ASCII
/// banner negotiation path.
const LEGACY_DAEMON_PREFIX: &str = "@RSYNCD:";
const LEGACY_DAEMON_PREFIX_LEN: usize = LEGACY_DAEMON_PREFIX.len();

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
/// later validation). Otherwise the exchange proceeds in binary mode. When the
/// caller has not yet accumulated enough bytes to decide, the helper reports
/// [`NegotiationPrologue::NeedMoreData`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NegotiationPrologue {
    /// There is not enough buffered data to determine the negotiation style.
    NeedMoreData,
    /// The peer is speaking the legacy ASCII `@RSYNCD:` protocol.
    LegacyAscii,
    /// The peer is speaking the modern binary negotiation protocol.
    Binary,
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
///   [`parse_legacy_daemon_greeting_bytes`], which will surface malformed input
///   as [`NegotiationError::MalformedLegacyGreeting`].
/// * Otherwise, the exchange uses the binary negotiation.
#[must_use]
pub fn detect_negotiation_prologue(buffer: &[u8]) -> NegotiationPrologue {
    if buffer.is_empty() {
        return NegotiationPrologue::NeedMoreData;
    }

    if buffer[0] != b'@' {
        return NegotiationPrologue::Binary;
    }

    let prefix = LEGACY_DAEMON_PREFIX.as_bytes();
    if buffer.len() < prefix.len() && &prefix[..buffer.len()] == buffer {
        return NegotiationPrologue::NeedMoreData;
    }

    NegotiationPrologue::LegacyAscii
}

/// Incremental detector for the negotiation prologue style.
///
/// The binary vs. legacy ASCII decision in upstream rsync is based on the very
/// first byte read from the transport. However, real transports often deliver
/// data in small bursts, meaning the caller may need to feed multiple chunks
/// before a definitive answer is available. This helper maintains a small
/// amount of state so that `detect_negotiation_prologue` parity can be achieved
/// without repeatedly re-buffering the prefix.
#[derive(Clone, Debug)]
pub struct NegotiationPrologueDetector {
    buffer: [u8; LEGACY_DAEMON_PREFIX_LEN],
    len: usize,
    decided: Option<NegotiationPrologue>,
}

impl NegotiationPrologueDetector {
    /// Creates a fresh detector that has not yet observed any bytes.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: [0; LEGACY_DAEMON_PREFIX_LEN],
            len: 0,
            decided: None,
        }
    }

    /// Observes the next chunk of bytes from the transport and reports the
    /// negotiation style chosen so far.
    ///
    /// Once a non-`NeedMoreData` classification is returned, subsequent calls
    /// will keep producing the same value without inspecting further input.
    pub fn observe(&mut self, chunk: &[u8]) -> NegotiationPrologue {
        if let Some(decided) = self.decided {
            return decided;
        }

        if chunk.is_empty() {
            return NegotiationPrologue::NeedMoreData;
        }

        let prefix = LEGACY_DAEMON_PREFIX.as_bytes();

        for &byte in chunk {
            if self.len == 0 {
                if byte != b'@' {
                    return self.decide(NegotiationPrologue::Binary);
                }

                self.buffer[0] = byte;
                self.len = 1;
                continue;
            }

            if self.len >= LEGACY_DAEMON_PREFIX_LEN {
                return self.decide(NegotiationPrologue::LegacyAscii);
            }

            self.buffer[self.len] = byte;
            self.len += 1;

            if &self.buffer[..self.len] == &prefix[..self.len] {
                if self.len == LEGACY_DAEMON_PREFIX_LEN {
                    return self.decide(NegotiationPrologue::LegacyAscii);
                }
                continue;
            }

            return self.decide(NegotiationPrologue::LegacyAscii);
        }

        NegotiationPrologue::NeedMoreData
    }

    fn decide(&mut self, decision: NegotiationPrologue) -> NegotiationPrologue {
        self.decided = Some(decision);
        decision
    }
}

impl Default for NegotiationPrologueDetector {
    fn default() -> Self {
        Self::new()
    }
}

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

    /// Array of protocol versions supported by the Rust implementation,
    /// ordered from newest to oldest.
    pub const SUPPORTED_VERSIONS: [ProtocolVersion; SUPPORTED_PROTOCOLS.len()] = [
        ProtocolVersion::new_const(32),
        ProtocolVersion::new_const(31),
        ProtocolVersion::new_const(30),
        ProtocolVersion::new_const(29),
        ProtocolVersion::new_const(28),
    ];

    /// Returns a reference to the list of supported protocol versions in
    /// newest-to-oldest order.
    #[must_use]
    pub const fn supported_versions() -> &'static [ProtocolVersion; SUPPORTED_PROTOCOLS.len()] {
        &Self::SUPPORTED_VERSIONS
    }

    /// Reports whether the provided version is supported by this
    /// implementation. This helper mirrors the upstream negotiation guard and
    /// allows callers to perform quick validation before attempting a
    /// handshake.
    #[must_use]
    #[inline]
    pub const fn is_supported(value: u8) -> bool {
        matches!(value, 28..=32)
    }

    /// Returns the raw numeric value represented by this version.
    #[must_use]
    #[inline]
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

impl TryFrom<NonZeroU8> for ProtocolVersion {
    type Error = NegotiationError;

    fn try_from(value: NonZeroU8) -> Result<Self, Self::Error> {
        <ProtocolVersion as TryFrom<u8>>::try_from(value.get())
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_u8())
    }
}

impl From<ProtocolVersion> for u8 {
    fn from(value: ProtocolVersion) -> Self {
        value.as_u8()
    }
}

impl From<ProtocolVersion> for NonZeroU8 {
    fn from(value: ProtocolVersion) -> Self {
        value.0
    }
}

impl PartialEq<u8> for ProtocolVersion {
    fn eq(&self, other: &u8) -> bool {
        self.as_u8() == *other
    }
}

impl PartialEq<ProtocolVersion> for u8 {
    fn eq(&self, other: &ProtocolVersion) -> bool {
        *self == other.as_u8()
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
pub fn parse_legacy_daemon_greeting(line: &str) -> Result<ProtocolVersion, NegotiationError> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let malformed = || malformed_legacy_greeting(trimmed);

    let after_prefix = trimmed
        .strip_prefix(LEGACY_DAEMON_PREFIX)
        .ok_or_else(malformed)?;

    let remainder = after_prefix.trim_start();
    if remainder.is_empty() {
        return Err(malformed());
    }

    let digits_len = ascii_digit_prefix_len(remainder);
    let digits = &remainder[..digits_len];
    if digits.is_empty() {
        return Err(malformed());
    }

    let mut rest = &remainder[digits_len..];
    loop {
        rest = rest.trim_start_matches(char::is_whitespace);

        if rest.is_empty() {
            break;
        }

        if let Some(after_dot) = rest.strip_prefix('.') {
            let fractional_len = ascii_digit_prefix_len(after_dot);
            if fractional_len == 0 {
                return Err(malformed());
            }

            rest = &after_dot[fractional_len..];
            continue;
        }

        return Err(malformed());
    }

    let version: u8 = digits.parse().map_err(|_| malformed())?;

    ProtocolVersion::from_peer_advertisement(version)
}

/// Returns the length of the leading ASCII-digit run within `input`.
fn ascii_digit_prefix_len(input: &str) -> usize {
    input
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

/// Constructs a [`NegotiationError::MalformedLegacyGreeting`] for `trimmed` input.
fn malformed_legacy_greeting(trimmed: &str) -> NegotiationError {
    NegotiationError::MalformedLegacyGreeting {
        input: trimmed.to_owned(),
    }
}

/// Parses a byte-oriented legacy daemon greeting by first validating UTF-8 and
/// then delegating to [`parse_legacy_daemon_greeting`].
///
/// Legacy clients and daemons exchange greetings as ASCII byte streams. The
/// Rust implementation keeps the byte-oriented helper separate so higher level
/// transports can operate directly on buffers received from the network without
/// performing lossy conversions. Invalid UTF-8 sequences are rejected with a
/// [`NegotiationError::MalformedLegacyGreeting`] that captures the lossy string
/// representation for diagnostics, mirroring upstream behavior where the raw
/// greeting is echoed back to the user.
pub fn parse_legacy_daemon_greeting_bytes(
    line: &[u8],
) -> Result<ProtocolVersion, NegotiationError> {
    match core::str::from_utf8(line) {
        Ok(text) => parse_legacy_daemon_greeting(text),
        Err(_) => Err(NegotiationError::MalformedLegacyGreeting {
            input: String::from_utf8_lossy(line).into_owned(),
        }),
    }
}

/// Formats the legacy ASCII daemon greeting used by pre-protocol-30 peers.
///
/// Upstream daemons send a line such as `@RSYNCD: 32.0\n` when speaking to
/// older clients. The Rust implementation mirrors that exact layout so callers
/// can emit byte-identical banners during negotiation and round-trip the value
/// through [`parse_legacy_daemon_greeting`].
#[must_use]
pub fn format_legacy_daemon_greeting(version: ProtocolVersion) -> String {
    let mut banner = String::with_capacity(16);
    banner.push_str(LEGACY_DAEMON_PREFIX);
    banner.push(' ');
    let digits = version.as_u8().to_string();
    banner.push_str(&digits);
    banner.push_str(".0\n");
    banner
}

/// Selects the highest mutual protocol version between the Rust implementation and a peer.
///
/// The caller provides the list of protocol versions advertised by the peer in any order.
/// The function filters the peer list to versions that upstream rsync 3.4.1 recognizes and
/// clamps versions newer than [`ProtocolVersion::NEWEST`] down to the newest supported
/// value, matching upstream tolerance for future releases. Duplicate peer entries and
/// out-of-order announcements are tolerated. If no mutual protocol exists,
/// [`NegotiationError::NoMutualProtocol`] is returned with the filtered peer list for context.
pub fn select_highest_mutual<I>(peer_versions: I) -> Result<ProtocolVersion, NegotiationError>
where
    I: IntoIterator<Item = u8>,
{
    let iter = peer_versions.into_iter();
    let (lower_bound, _) = iter.size_hint();
    let mut filtered: Vec<u8> = Vec::with_capacity(min(lower_bound, SUPPORTED_PROTOCOLS.len()));
    let mut oldest_rejection: Option<u8> = None;

    for version in iter {
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
    use core::num::NonZeroU8;

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
    fn select_highest_mutual_deduplicates_peer_versions() {
        let negotiated = select_highest_mutual([32, 32, 31, 31]).expect("must select 32");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn display_for_no_mutual_protocol_mentions_filtered_list() {
        let err = NegotiationError::NoMutualProtocol {
            peer_versions: vec![29, 30],
        };
        let rendered = err.to_string();
        assert!(rendered.contains("peer offered [29, 30]"));
        assert!(rendered.contains("we support"));
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
    fn parses_legacy_daemon_greeting_without_space_after_prefix() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD:31.0\n").expect("valid greeting");
        assert_eq!(parsed.as_u8(), 31);
    }

    #[test]
    fn parses_legacy_daemon_greeting_with_whitespace_before_fractional() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 32   .0   \n").expect("valid greeting");
        assert_eq!(parsed, ProtocolVersion::NEWEST);
    }

    #[test]
    fn parses_legacy_daemon_greeting_without_fractional_suffix() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 30\n").expect("fractional optional");
        assert_eq!(parsed.as_u8(), 30);
    }

    #[test]
    fn parses_greeting_with_trailing_whitespace() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 30.0   \n").expect("valid greeting");
        assert_eq!(parsed.as_u8(), 30);
    }

    #[test]
    fn parses_legacy_greeting_from_bytes() {
        let parsed =
            parse_legacy_daemon_greeting_bytes(b"@RSYNCD: 29.0\r\n").expect("valid byte greeting");
        assert_eq!(parsed.as_u8(), 29);
    }

    #[test]
    fn rejects_non_utf8_legacy_greetings() {
        let err = parse_legacy_daemon_greeting_bytes(b"@RSYNCD: 31.0\xff").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn rejects_greeting_with_unsupported_version() {
        let err = parse_legacy_daemon_greeting("@RSYNCD: 27.0").unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(27));
    }

    #[test]
    fn clamps_future_versions_in_peer_advertisements_directly() {
        let negotiated =
            ProtocolVersion::from_peer_advertisement(40).expect("future versions clamp");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn rejects_peer_advertisements_older_than_supported_range() {
        let err = ProtocolVersion::from_peer_advertisement(27).unwrap_err();
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

    #[test]
    fn formats_legacy_daemon_greeting_for_newest_protocol() {
        let rendered = format_legacy_daemon_greeting(ProtocolVersion::NEWEST);
        assert_eq!(rendered, "@RSYNCD: 32.0\n");
    }

    #[test]
    fn formatted_legacy_greeting_round_trips_through_parser() {
        let version = ProtocolVersion::try_from(29).expect("valid version");
        let rendered = format_legacy_daemon_greeting(version);
        let parsed = parse_legacy_daemon_greeting(&rendered).expect("parseable banner");
        assert_eq!(parsed, version);
    }

    #[test]
    fn parses_legacy_daemon_greeting_bytes() {
        let bytes = b"@RSYNCD: 30.0\n";
        let parsed = parse_legacy_daemon_greeting_bytes(bytes).expect("valid greeting");
        assert_eq!(parsed.as_u8(), 30);
    }

    #[test]
    fn rejects_non_utf8_legacy_daemon_greeting_bytes() {
        let bytes = b"@RSYNCD: 31.0\xff";
        let err = parse_legacy_daemon_greeting_bytes(bytes).unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn converts_protocol_version_to_u8() {
        let version = ProtocolVersion::try_from(31).expect("valid version");
        let numeric: u8 = version.into();
        assert_eq!(numeric, 31);
    }

    #[test]
    fn converts_protocol_version_to_non_zero_u8() {
        let version = ProtocolVersion::try_from(32).expect("valid version");
        let numeric: NonZeroU8 = version.into();
        assert_eq!(numeric.get(), 32);
    }

    #[test]
    fn converts_from_non_zero_u8() {
        let non_zero = NonZeroU8::new(32).expect("non-zero literal");
        let version = ProtocolVersion::try_from(non_zero).expect("within range");
        assert_eq!(version, ProtocolVersion::NEWEST);
    }

    #[test]
    fn rejects_out_of_range_non_zero_u8() {
        let non_zero = NonZeroU8::new(27).expect("non-zero literal");
        let err = ProtocolVersion::try_from(non_zero).unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(27));
    }

    #[test]
    fn compares_directly_with_u8() {
        let version = ProtocolVersion::try_from(30).expect("valid version");
        assert_eq!(version, 30);
        assert_eq!(30, version);
        assert_ne!(version, 31);
        assert_ne!(31, version);
    }

    #[test]
    fn supported_versions_constant_matches_u8_list() {
        let as_u8: Vec<u8> = ProtocolVersion::supported_versions()
            .iter()
            .map(|version| version.as_u8())
            .collect();
        assert_eq!(as_u8.as_slice(), &SUPPORTED_PROTOCOLS);
    }

    #[test]
    fn detects_supported_versions() {
        for &version in &SUPPORTED_PROTOCOLS {
            assert!(ProtocolVersion::is_supported(version));
        }

        assert!(!ProtocolVersion::is_supported(27));
        assert!(!ProtocolVersion::is_supported(0));
    }

    #[test]
    fn detect_negotiation_prologue_requires_data() {
        assert_eq!(
            detect_negotiation_prologue(b""),
            NegotiationPrologue::NeedMoreData
        );
    }

    #[test]
    fn detect_negotiation_prologue_waits_for_full_prefix() {
        assert_eq!(
            detect_negotiation_prologue(b"@RS"),
            NegotiationPrologue::NeedMoreData
        );
    }

    #[test]
    fn detect_negotiation_prologue_flags_legacy_ascii() {
        assert_eq!(
            detect_negotiation_prologue(b"@RSYNCD: 31.0\n"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn detect_negotiation_prologue_flags_malformed_ascii_as_legacy() {
        assert_eq!(
            detect_negotiation_prologue(b"@RSYNCX"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn detect_negotiation_prologue_detects_binary() {
        assert_eq!(
            detect_negotiation_prologue(&[0x00, 0x20, 0x00, 0x00]),
            NegotiationPrologue::Binary
        );
    }

    #[test]
    fn prologue_detector_requires_data() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b""), NegotiationPrologue::NeedMoreData);
        assert_eq!(detector.observe(b"@"), NegotiationPrologue::NeedMoreData);
        assert_eq!(
            detector.observe(b"RSYNCD: 31.0\n"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn prologue_detector_detects_binary_immediately() {
        let mut detector = NegotiationPrologueDetector::default();
        assert_eq!(detector.observe(b"x"), NegotiationPrologue::Binary);
        assert_eq!(detector.observe(b"@"), NegotiationPrologue::Binary);
    }

    #[test]
    fn prologue_detector_handles_prefix_mismatch() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(
            detector.observe(b"@RSYNCD"),
            NegotiationPrologue::NeedMoreData
        );
        assert_eq!(detector.observe(b"X"), NegotiationPrologue::LegacyAscii);
        assert_eq!(
            detector.observe(b"additional"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn prologue_detector_handles_split_prefix_chunks() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::NeedMoreData);
        assert_eq!(detector.observe(b"YN"), NegotiationPrologue::NeedMoreData);
        assert_eq!(
            detector.observe(b"CD: 32"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn prologue_detector_caches_decision() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b"@X"), NegotiationPrologue::LegacyAscii);
        assert_eq!(
            detector.observe(b"anything"),
            NegotiationPrologue::LegacyAscii
        );
    }
}
