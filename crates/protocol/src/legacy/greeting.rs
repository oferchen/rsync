use crate::error::NegotiationError;
use crate::version::ProtocolVersion;
use core::fmt::{self, Write as FmtWrite};
use core::iter::FusedIterator;
use std::borrow::ToOwned;
use std::str::SplitAsciiWhitespace;

/// Owned representation of a legacy ASCII daemon greeting.
///
/// [`LegacyDaemonGreeting`] borrows the buffer that backed the parsed line,
/// which is convenient for streaming parsers but cumbersome for higher layers
/// that need to retain the metadata beyond the lifetime of the temporary
/// buffer. The owned variant stores the advertised protocol number, optional
/// subprotocol suffix, and digest list without tying them to an external
/// allocation. The structure intentionally mirrors the borrowed API so call
/// sites can switch between the two with minimal friction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyDaemonGreetingOwned {
    protocol: ProtocolVersion,
    advertised_protocol: u32,
    subprotocol: Option<u32>,
    digest_list: Option<String>,
}

use super::{LEGACY_DAEMON_PREFIX, malformed_legacy_greeting};

/// Detailed representation of a legacy ASCII daemon greeting.
///
/// Legacy daemons announce their protocol support via lines such as
/// `@RSYNCD: 31.0 md4 md5`. Besides the major protocol number the banner may
/// contain a fractional component (known as the "subprotocol") and an optional
/// digest list used for challenge/response authentication. Upstream rsync
/// retains all of this metadata during negotiation so the Rust implementation
/// mirrors that structure to avoid lossy parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyDaemonGreeting<'a> {
    protocol: ProtocolVersion,
    advertised_protocol: u32,
    subprotocol: Option<u32>,
    digest_list: Option<&'a str>,
}

impl<'a> LegacyDaemonGreeting<'a> {
    /// Returns the negotiated protocol version after clamping unsupported
    /// advertisements to the newest supported release.
    #[must_use]
    pub const fn protocol(self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns the protocol number advertised by the peer before clamping.
    ///
    /// Future peers may announce versions newer than we support. Upstream rsync
    /// still records the advertised value, so the helper exposes it for higher
    /// layers that mirror that behaviour.
    #[must_use]
    pub const fn advertised_protocol(self) -> u32 {
        self.advertised_protocol
    }

    /// Returns the parsed subprotocol value or zero when it was absent.
    #[must_use]
    pub const fn subprotocol(self) -> u32 {
        match self.subprotocol {
            Some(value) => value,
            None => 0,
        }
    }

    /// Returns the optional subprotocol suffix without normalizing missing values to zero.
    ///
    /// Upstream rsync distinguishes between greetings that included an explicit fractional
    /// component (for example `@RSYNCD: 31.0`) and those that omitted it entirely. The Rust
    /// implementation previously required callers to pair [`Self::has_subprotocol`] with
    /// [`Self::subprotocol`] to retain that distinction. Exposing the raw optional value keeps the
    /// API expressive while preserving the zero-default helper used by code paths that only need the
    /// numeric suffix.
    #[must_use]
    pub const fn subprotocol_raw(self) -> Option<u32> {
        self.subprotocol
    }

    /// Reports whether the greeting explicitly supplied a subprotocol suffix.
    #[must_use]
    pub const fn has_subprotocol(self) -> bool {
        self.subprotocol.is_some()
    }

    /// Returns the digest list announced by the daemon, if any.
    #[must_use]
    pub const fn digest_list(self) -> Option<&'a str> {
        self.digest_list
    }

    /// Reports whether the daemon advertised a digest list used for challenge/response authentication.
    #[must_use]
    pub const fn has_digest_list(self) -> bool {
        self.digest_list.is_some()
    }

    /// Returns an iterator over the whitespace-separated digest tokens announced by the daemon.
    ///
    /// Upstream rsync uses the digest list to negotiate challenge/response algorithms during the
    /// legacy ASCII handshake. The iterator splits the stored list on ASCII whitespace while
    /// preserving the original token order, allowing higher layers to check for specific digests
    /// without allocating intermediate buffers.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::parse_legacy_daemon_greeting_details;
    ///
    /// let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md5 md4\n")?;
    /// let tokens: Vec<_> = greeting.digest_tokens().collect();
    ///
    /// assert_eq!(tokens, ["md5", "md4"]);
    /// # Ok::<_, rsync_protocol::NegotiationError>(())
    /// ```
    #[must_use]
    pub fn digest_tokens(&self) -> DigestListTokens<'_> {
        DigestListTokens::new(self.digest_list())
    }

    /// Reports whether the daemon advertised support for the specified digest algorithm.
    ///
    /// The comparison follows upstream rsync's behaviour by matching ASCII tokens without
    /// allocating new strings. Whitespace surrounding the query is ignored and matching is
    /// case-insensitive because the daemon may emit lowercase names while callers often
    /// canonicalise constants using uppercase letters.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::parse_legacy_daemon_greeting_details;
    ///
    /// let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md5 md4\n")?;
    /// assert!(greeting.supports_digest("md5"));
    /// assert!(greeting.supports_digest("MD4"));
    /// assert!(!greeting.supports_digest("sha1"));
    /// # Ok::<_, rsync_protocol::NegotiationError>(())
    /// ```
    #[must_use]
    pub fn supports_digest(&self, name: &str) -> bool {
        let trimmed = name.trim_matches(|ch: char| ch.is_ascii_whitespace());
        if trimmed.is_empty() {
            return false;
        }

        self.digest_tokens()
            .any(|token| token.eq_ignore_ascii_case(trimmed))
    }

    /// Converts the borrowed greeting into an owned representation.
    ///
    /// Legacy negotiation flows often parse the greeting while the underlying
    /// buffer is still borrowed from a network reader. Higher layers may need
    /// to retain the metadata after the buffer is recycled, in which case the
    /// owned variant avoids cloning individual fields one by one.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::{
    ///     parse_legacy_daemon_greeting_details, LegacyDaemonGreetingOwned,
    /// };
    ///
    /// let borrowed = parse_legacy_daemon_greeting_details("@RSYNCD: 29.1 md4\n")?;
    /// let owned: LegacyDaemonGreetingOwned = borrowed.into_owned();
    ///
    /// assert_eq!(owned.advertised_protocol(), 29);
    /// assert_eq!(owned.subprotocol_raw(), Some(1));
    /// assert_eq!(owned.digest_list(), Some("md4"));
    /// # Ok::<_, rsync_protocol::NegotiationError>(())
    /// ```
    #[must_use]
    pub fn into_owned(self) -> LegacyDaemonGreetingOwned {
        self.into()
    }
}

impl LegacyDaemonGreetingOwned {
    /// Returns the negotiated protocol version after clamping unsupported
    /// advertisements to the newest supported release.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns the protocol number advertised by the peer before clamping.
    #[must_use]
    pub const fn advertised_protocol(&self) -> u32 {
        self.advertised_protocol
    }

    /// Returns the parsed subprotocol value or zero when it was absent.
    #[must_use]
    pub const fn subprotocol(&self) -> u32 {
        match self.subprotocol {
            Some(value) => value,
            None => 0,
        }
    }

    /// Returns the optional subprotocol suffix without normalizing missing values to zero.
    #[must_use]
    pub const fn subprotocol_raw(&self) -> Option<u32> {
        self.subprotocol
    }

    /// Reports whether the greeting explicitly supplied a subprotocol suffix.
    #[must_use]
    pub const fn has_subprotocol(&self) -> bool {
        self.subprotocol.is_some()
    }

    /// Constructs an owned legacy daemon greeting from its parsed components.
    ///
    /// The helper mirrors [`parse_legacy_daemon_greeting_details`] by clamping
    /// future protocol advertisements, normalising digest lists, and enforcing
    /// the rule that protocol 31 and newer must include a fractional suffix.
    /// This is primarily useful in tests that want to exercise higher layers
    /// without round-tripping through string formatting.
    ///
    /// # Errors
    ///
    /// Returns [`NegotiationError::UnsupportedVersion`] when the advertised
    /// protocol falls outside the upstream range and
    /// [`NegotiationError::MalformedLegacyGreeting`] when a required
    /// subprotocol suffix is missing.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::{LegacyDaemonGreetingOwned, ProtocolVersion};
    ///
    /// let greeting = LegacyDaemonGreetingOwned::from_parts(
    ///     31,
    ///     Some(0),
    ///     Some(String::from("  md4 md5  ")),
    /// )?;
    ///
    /// assert_eq!(
    ///     greeting.protocol(),
    ///     ProtocolVersion::from_supported(31).unwrap()
    /// );
    /// assert_eq!(greeting.digest_list(), Some("md4 md5"));
    /// assert!(greeting.has_digest_list());
    /// # Ok::<_, rsync_protocol::NegotiationError>(())
    /// ```
    #[doc(alias = "@RSYNCD")]
    pub fn from_parts(
        advertised_protocol: u32,
        subprotocol: Option<u32>,
        digest_list: Option<String>,
    ) -> Result<Self, NegotiationError> {
        let digest_list = digest_list.and_then(|list| {
            let trimmed = list.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.len() == list.len() {
                Some(list)
            } else {
                Some(trimmed.to_owned())
            }
        });

        if advertised_protocol >= 31 && subprotocol.is_none() {
            let mut rendered = format!("{LEGACY_DAEMON_PREFIX} {advertised_protocol}");
            if let Some(ref digest) = digest_list {
                rendered.push(' ');
                rendered.push_str(digest);
            }
            return Err(NegotiationError::MalformedLegacyGreeting { input: rendered });
        }

        let negotiated_byte = advertised_protocol.min(u32::from(u8::MAX)) as u8;
        let protocol = ProtocolVersion::from_peer_advertisement(negotiated_byte)?;

        Ok(Self {
            protocol,
            advertised_protocol,
            subprotocol,
            digest_list,
        })
    }

    /// Returns the digest list announced by the daemon, if any.
    #[must_use]
    pub fn digest_list(&self) -> Option<&str> {
        self.digest_list.as_deref()
    }

    /// Reports whether the daemon advertised a digest list used for challenge/response authentication.
    #[must_use]
    pub const fn has_digest_list(&self) -> bool {
        self.digest_list.is_some()
    }

    /// Returns an iterator over the whitespace-separated digest tokens announced by the daemon.
    ///
    /// This mirrors [`LegacyDaemonGreeting::digest_tokens`] while borrowing from the owned string,
    /// making it convenient to inspect digest capabilities after the greeting has been detached from
    /// the parsing buffer.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::{LegacyDaemonGreetingOwned, NegotiationError};
    ///
    /// let greeting = LegacyDaemonGreetingOwned::from_parts(29, None, Some("md4\tmd5".into()))?;
    /// let tokens: Vec<_> = greeting.digest_tokens().collect();
    ///
    /// assert_eq!(tokens, ["md4", "md5"]);
    /// # Ok::<_, NegotiationError>(())
    /// ```
    #[must_use]
    pub fn digest_tokens(&self) -> DigestListTokens<'_> {
        DigestListTokens::new(self.digest_list())
    }

    /// Reports whether the daemon advertised support for the specified digest algorithm.
    ///
    /// This mirrors [`LegacyDaemonGreeting::supports_digest`] while borrowing from the owned
    /// string, allowing callers that retain the parsed metadata to perform capability checks
    /// without re-parsing the original banner.
    #[must_use]
    pub fn supports_digest(&self, name: &str) -> bool {
        let trimmed = name.trim_matches(|ch: char| ch.is_ascii_whitespace());
        if trimmed.is_empty() {
            return false;
        }

        self.digest_tokens()
            .any(|token| token.eq_ignore_ascii_case(trimmed))
    }

    /// Returns a borrowed representation of the greeting.
    #[must_use]
    pub fn as_borrowed(&self) -> LegacyDaemonGreeting<'_> {
        LegacyDaemonGreeting {
            protocol: self.protocol,
            advertised_protocol: self.advertised_protocol,
            subprotocol: self.subprotocol,
            digest_list: self.digest_list.as_deref(),
        }
    }

    /// Decomposes the greeting into its individual fields without cloning.
    ///
    /// The helper is useful when higher layers need to stash the advertised
    /// protocol, subprotocol, and digest list separately. Consuming `self`
    /// allows the digest list to be moved out of the structure rather than
    /// cloned.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::{parse_legacy_daemon_greeting_owned, ProtocolVersion};
    ///
    /// let owned = parse_legacy_daemon_greeting_owned("@RSYNCD: 30.5 md5\n")?;
    /// let (protocol, advertised, subprotocol, digest) = owned.into_parts();
    ///
    /// assert_eq!(protocol, ProtocolVersion::from_supported(30).unwrap());
    /// assert_eq!(advertised, 30);
    /// assert_eq!(subprotocol, Some(5));
    /// assert_eq!(digest, Some(String::from("md5")));
    /// # Ok::<_, rsync_protocol::NegotiationError>(())
    /// ```
    #[must_use]
    pub fn into_parts(self) -> (ProtocolVersion, u32, Option<u32>, Option<String>) {
        let Self {
            protocol,
            advertised_protocol,
            subprotocol,
            digest_list,
        } = self;

        (protocol, advertised_protocol, subprotocol, digest_list)
    }

    /// Consumes the greeting and returns the optional digest list without
    /// cloning.
    ///
    /// When the caller only needs the digest list, this convenience helper
    /// avoids unpacking the rest of the fields via [`Self::into_parts`].
    #[must_use]
    pub fn into_digest_list(self) -> Option<String> {
        let Self { digest_list, .. } = self;
        digest_list
    }
}

impl<'a> From<LegacyDaemonGreeting<'a>> for LegacyDaemonGreetingOwned {
    fn from(greeting: LegacyDaemonGreeting<'a>) -> Self {
        Self {
            protocol: greeting.protocol(),
            advertised_protocol: greeting.advertised_protocol(),
            subprotocol: greeting.subprotocol_raw(),
            digest_list: greeting.digest_list().map(ToOwned::to_owned),
        }
    }
}

/// Iterator over whitespace-separated digest tokens advertised by a legacy daemon.
///
/// Instances of this iterator are created via [`LegacyDaemonGreeting::digest_tokens`] or
/// [`LegacyDaemonGreetingOwned::digest_tokens`]. The iterator yields each digest exactly once in the
/// order received from the peer, matching upstream rsync's processing of the challenge/response list.
#[derive(Clone, Debug)]
pub struct DigestListTokens<'a> {
    inner: Option<SplitAsciiWhitespace<'a>>,
}

impl<'a> DigestListTokens<'a> {
    fn new(digest_list: Option<&'a str>) -> Self {
        Self {
            inner: digest_list.map(|list| list.split_ascii_whitespace()),
        }
    }
}

impl<'a> Iterator for DigestListTokens<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let iter = self.inner.as_mut()?;
        match iter.next() {
            Some(token) => Some(token),
            None => {
                self.inner = None;
                None
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner
            .as_ref()
            .map_or((0, Some(0)), Iterator::size_hint)
    }
}

impl<'a> FusedIterator for DigestListTokens<'a> {}

/// Parses a legacy ASCII daemon greeting of the form `@RSYNCD: <version>`.
///
/// This convenience wrapper retains the historical API by returning only the
/// negotiated [`ProtocolVersion`]. Callers that need access to the advertised
/// protocol number, subprotocol suffix, or digest list should use
/// [`parse_legacy_daemon_greeting_details`].
#[doc(alias = "@RSYNCD")]
#[must_use = "legacy daemon greeting parsing errors must be handled"]
pub fn parse_legacy_daemon_greeting(line: &str) -> Result<ProtocolVersion, NegotiationError> {
    parse_legacy_daemon_greeting_details(line).map(LegacyDaemonGreeting::protocol)
}

/// Parses a legacy ASCII daemon greeting and returns an owned representation.
///
/// Legacy negotiation helpers frequently need to retain the parsed metadata
/// beyond the lifetime of the buffer that backed the original line. This
/// wrapper mirrors [`parse_legacy_daemon_greeting_details`] but converts the
/// borrowed [`LegacyDaemonGreeting`] into the fully owned
/// [`LegacyDaemonGreetingOwned`], allowing callers to drop the input buffer
/// immediately after parsing.
///
/// # Examples
///
/// ```
/// use rsync_protocol::{parse_legacy_daemon_greeting_owned, ProtocolVersion};
///
/// let owned = parse_legacy_daemon_greeting_owned("@RSYNCD: 29\n")?;
/// assert_eq!(owned.protocol(), ProtocolVersion::from_supported(29).unwrap());
/// assert_eq!(owned.advertised_protocol(), 29);
/// assert!(!owned.has_subprotocol());
/// # Ok::<_, rsync_protocol::NegotiationError>(())
/// ```
#[doc(alias = "@RSYNCD")]
#[must_use = "legacy daemon greeting parsing errors must be handled"]
pub fn parse_legacy_daemon_greeting_owned(
    line: &str,
) -> Result<LegacyDaemonGreetingOwned, NegotiationError> {
    parse_legacy_daemon_greeting_details(line).map(Into::into)
}

/// Parses a legacy daemon greeting and returns a structured representation.
#[doc(alias = "@RSYNCD")]
#[must_use = "legacy daemon greeting parsing errors must be handled"]
pub fn parse_legacy_daemon_greeting_details(
    line: &str,
) -> Result<LegacyDaemonGreeting<'_>, NegotiationError> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let malformed = || malformed_legacy_greeting(trimmed);

    let after_prefix = trimmed
        .strip_prefix(LEGACY_DAEMON_PREFIX)
        .ok_or_else(malformed)?;

    let mut remainder = after_prefix.trim_start();
    if remainder.is_empty() {
        return Err(malformed());
    }

    let digits_len = ascii_digit_prefix_len(remainder);
    if digits_len == 0 {
        return Err(malformed());
    }

    let digits = &remainder[..digits_len];
    let advertised_protocol = parse_ascii_digits_to_u32(digits);
    remainder = &remainder[digits_len..];

    let mut subprotocol = None;
    loop {
        let trimmed_remainder = remainder.trim_start_matches(char::is_whitespace);
        let had_leading_whitespace = trimmed_remainder.len() != remainder.len();

        if trimmed_remainder.is_empty() {
            remainder = trimmed_remainder;
            break;
        }

        if let Some(after_dot) = trimmed_remainder.strip_prefix('.') {
            let fractional_len = ascii_digit_prefix_len(after_dot);
            if fractional_len == 0 {
                return Err(malformed());
            }

            let fractional_digits = &after_dot[..fractional_len];
            subprotocol = Some(parse_ascii_digits_to_u32(fractional_digits));
            remainder = &after_dot[fractional_len..];
            continue;
        }

        if !had_leading_whitespace {
            return Err(malformed());
        }

        remainder = trimmed_remainder;
        break;
    }

    if advertised_protocol >= 31 && subprotocol.is_none() {
        return Err(malformed());
    }

    let digest_list = remainder.trim();
    let digest_list = if digest_list.is_empty() {
        None
    } else {
        Some(digest_list)
    };

    let negotiated = advertised_protocol.min(u32::from(u8::MAX)) as u8;
    let protocol = ProtocolVersion::from_peer_advertisement(negotiated)?;

    Ok(LegacyDaemonGreeting {
        protocol,
        advertised_protocol,
        subprotocol,
        digest_list,
    })
}

/// Returns the length of the leading ASCII-digit run within `input`.
fn ascii_digit_prefix_len(input: &str) -> usize {
    input
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

/// Parses a string consisting solely of ASCII digits into a `u32`, saturating on
/// overflow.
fn parse_ascii_digits_to_u32(digits: &str) -> u32 {
    let mut value: u32 = 0;

    for &byte in digits.as_bytes() {
        debug_assert!(byte.is_ascii_digit());
        let digit = u32::from(byte - b'0');
        value = value.saturating_mul(10);
        value = value.saturating_add(digit);
    }

    value
}

/// Writes the legacy ASCII daemon greeting into the supplied [`fmt::Write`] sink.
///
/// Upstream daemons send a line such as `@RSYNCD: 32.0\n` when speaking to
/// older clients. The helper mirrors that layout without allocating, enabling
/// callers to render the greeting directly into stack buffers or
/// pre-allocated `String`s. The newline terminator is appended automatically to
/// match upstream rsync's behaviour.
pub fn write_legacy_daemon_greeting<W: FmtWrite>(
    writer: &mut W,
    version: ProtocolVersion,
) -> fmt::Result {
    writer.write_str(LEGACY_DAEMON_PREFIX)?;
    writer.write_char(' ')?;

    let mut value = version.as_u8();
    let mut digits = [0u8; 3];
    let mut len = 0usize;

    loop {
        debug_assert!(
            len < digits.len(),
            "protocol version must fit in three decimal digits"
        );
        digits[len] = value % 10;
        len += 1;
        value /= 10;

        if value == 0 {
            break;
        }
    }

    for index in (0..len).rev() {
        writer.write_char(char::from(b'0' + digits[index]))?;
    }

    writer.write_str(".0\n")
}

/// Formats the legacy ASCII daemon greeting used by pre-protocol-30 peers.
///
/// This convenience wrapper allocates a [`String`] and delegates to
/// [`write_legacy_daemon_greeting`] so existing call sites can retain their API
/// while newer code paths format directly into reusable buffers.
#[must_use]
pub fn format_legacy_daemon_greeting(version: ProtocolVersion) -> String {
    let mut banner = String::with_capacity(LEGACY_DAEMON_PREFIX.len() + 6);
    write_legacy_daemon_greeting(&mut banner, version).expect("writing to a String cannot fail");
    banner
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt;

    #[test]
    fn parses_legacy_daemon_greeting_with_minor_version() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 31.0\r\n").expect("valid greeting");
        assert_eq!(parsed.as_u8(), 31);
    }

    #[test]
    fn legacy_daemon_greeting_exposes_optional_subprotocol() {
        let with_fractional = parse_legacy_daemon_greeting_details("@RSYNCD: 30.5\n")
            .expect("fractional component must parse");
        assert_eq!(with_fractional.subprotocol_raw(), Some(5));
        assert!(with_fractional.has_subprotocol());
        assert_eq!(with_fractional.subprotocol(), 5);

        let without_fractional =
            parse_legacy_daemon_greeting_details("@RSYNCD: 29\n").expect("suffix-less greeting");
        assert_eq!(without_fractional.subprotocol_raw(), None);
        assert!(!without_fractional.has_subprotocol());
        assert_eq!(without_fractional.subprotocol(), 0);
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
    fn parses_legacy_daemon_greeting_details_with_digest_list() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md4 md5\n")
            .expect("digest list should parse");

        assert_eq!(
            greeting.protocol(),
            ProtocolVersion::from_supported(31).unwrap()
        );
        assert_eq!(greeting.advertised_protocol(), 31);
        assert!(greeting.has_subprotocol());
        assert_eq!(greeting.subprotocol(), 0);
        assert_eq!(greeting.digest_list(), Some("md4 md5"));
        assert!(greeting.has_digest_list());
    }

    #[test]
    fn greeting_details_accepts_trailing_whitespace_in_digest_list() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0   md4   md5  \r\n")
            .expect("digest list should tolerate padding");

        assert_eq!(greeting.digest_list(), Some("md4   md5"));
        assert!(greeting.has_digest_list());
    }

    #[test]
    fn borrowed_greeting_digest_tokens_iterate_in_order() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 30.0 md5\tmd4\n")
            .expect("digest list should parse");

        let collected: Vec<_> = greeting.digest_tokens().collect();
        assert_eq!(collected, ["md5", "md4"]);

        let no_digest =
            parse_legacy_daemon_greeting_details("@RSYNCD: 29\n").expect("no digest list");
        let empty: Vec<_> = no_digest.digest_tokens().collect();
        assert!(empty.is_empty());
    }

    #[test]
    fn borrowed_greeting_reports_supported_digests() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md5 md4\n")
            .expect("digest list should parse");

        assert!(greeting.supports_digest("md5"));
        assert!(greeting.supports_digest(" MD4 "));
        assert!(!greeting.supports_digest("sha1"));
        assert!(!greeting.supports_digest(""));

        let no_digest =
            parse_legacy_daemon_greeting_details("@RSYNCD: 29\n").expect("no digest list");
        assert!(!no_digest.supports_digest("md5"));
    }

    #[test]
    fn greeting_details_records_absence_of_subprotocol_for_old_versions() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 29\n")
            .expect("old protocols may omit subprotocol");

        assert_eq!(greeting.protocol().as_u8(), 29);
        assert!(!greeting.has_subprotocol());
        assert_eq!(greeting.subprotocol(), 0);
        assert!(!greeting.has_digest_list());
    }

    #[test]
    fn parse_owned_greeting_retains_metadata() {
        let owned = parse_legacy_daemon_greeting_owned("@RSYNCD: 29.1 md4\n")
            .expect("owned parsing should succeed");

        assert_eq!(
            owned.protocol(),
            ProtocolVersion::from_supported(29).unwrap()
        );
        assert_eq!(owned.advertised_protocol(), 29);
        assert_eq!(owned.subprotocol_raw(), Some(1));
        assert_eq!(owned.digest_list(), Some("md4"));
        assert!(owned.has_digest_list());
    }

    #[test]
    fn owned_greeting_digest_tokens_iterate_in_order() {
        let greeting =
            LegacyDaemonGreetingOwned::from_parts(31, Some(0), Some(" md4  md5  md6".into()))
                .expect("construction succeeds");

        let collected: Vec<_> = greeting.digest_tokens().collect();
        assert_eq!(collected, ["md4", "md5", "md6"]);

        let no_digest =
            LegacyDaemonGreetingOwned::from_parts(28, None, None).expect("no digest list");
        let empty: Vec<_> = no_digest.digest_tokens().collect();
        assert!(empty.is_empty());
    }

    #[test]
    fn owned_greeting_reports_supported_digests() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md4 md5\n")
            .expect("digest list should parse")
            .into_owned();

        assert!(greeting.supports_digest("md4"));
        assert!(greeting.supports_digest("MD5"));
        assert!(!greeting.supports_digest("sha1"));

        let no_digest = parse_legacy_daemon_greeting_details("@RSYNCD: 30.0\n")
            .expect("no digest list")
            .into_owned();
        assert!(!no_digest.supports_digest("md5"));
    }

    #[test]
    fn owned_greeting_captures_digest_list_and_subprotocol() {
        let borrowed = parse_legacy_daemon_greeting_details("@RSYNCD: 31.5 md4 md5\n")
            .expect("greeting should parse");
        let owned = LegacyDaemonGreetingOwned::from(borrowed);

        assert_eq!(owned.protocol(), borrowed.protocol());
        assert_eq!(owned.advertised_protocol(), borrowed.advertised_protocol());
        assert_eq!(owned.subprotocol_raw(), borrowed.subprotocol_raw());
        assert_eq!(owned.digest_list(), borrowed.digest_list());
        assert!(owned.has_subprotocol());
        assert!(owned.has_digest_list());

        let reborrowed = owned.as_borrowed();
        assert_eq!(reborrowed.protocol(), borrowed.protocol());
        assert_eq!(reborrowed.digest_list(), borrowed.digest_list());
    }

    #[test]
    fn owned_greeting_tracks_absent_fields() {
        let borrowed = parse_legacy_daemon_greeting_details("@RSYNCD: 29\n").expect("greeting");
        let owned = LegacyDaemonGreetingOwned::from(borrowed);

        assert_eq!(owned.protocol().as_u8(), 29);
        assert!(!owned.has_subprotocol());
        assert_eq!(owned.subprotocol_raw(), None);
        assert!(owned.digest_list().is_none());
        assert!(!owned.has_digest_list());
    }

    #[test]
    fn borrowed_into_owned_preserves_metadata_without_cloning() {
        let borrowed = parse_legacy_daemon_greeting_details("@RSYNCD: 30.7 md5\n")
            .expect("greeting should parse");
        let owned = borrowed.into_owned();

        assert_eq!(owned.protocol(), borrowed.protocol());
        assert_eq!(owned.advertised_protocol(), borrowed.advertised_protocol());
        assert_eq!(owned.subprotocol_raw(), borrowed.subprotocol_raw());
        assert_eq!(owned.digest_list(), borrowed.digest_list());
    }

    #[test]
    fn into_parts_moves_digest_list_without_extra_allocations() {
        let owned = parse_legacy_daemon_greeting_owned("@RSYNCD: 28.1 md4 md5\n")
            .expect("owned parsing should succeed");
        let (protocol, advertised, subprotocol, digest) = owned.into_parts();

        assert_eq!(protocol, ProtocolVersion::from_supported(28).unwrap());
        assert_eq!(advertised, 28);
        assert_eq!(subprotocol, Some(1));
        assert_eq!(digest, Some(String::from("md4 md5")));
    }

    #[test]
    fn into_digest_list_consumes_greeting() {
        let owned = parse_legacy_daemon_greeting_owned("@RSYNCD: 29.9 md4\n")
            .expect("owned parsing should succeed");
        let digest = owned.into_digest_list();

        assert_eq!(digest, Some(String::from("md4")));
    }

    #[test]
    fn from_parts_replicates_parser_behaviour() {
        let constructed =
            LegacyDaemonGreetingOwned::from_parts(31, Some(0), Some(String::from("  md4   md5  ")))
                .expect("construction should succeed");
        let parsed = parse_legacy_daemon_greeting_owned("@RSYNCD: 31.0   md4   md5  \r\n")
            .expect("parsing should succeed");

        assert_eq!(constructed.protocol(), parsed.protocol());
        assert_eq!(
            constructed.advertised_protocol(),
            parsed.advertised_protocol()
        );
        assert_eq!(constructed.subprotocol_raw(), parsed.subprotocol_raw());
        assert_eq!(constructed.digest_list(), parsed.digest_list());
    }

    #[test]
    fn from_parts_requires_subprotocol_for_newer_versions() {
        let err = LegacyDaemonGreetingOwned::from_parts(31, None, Some(String::from("md4")))
            .expect_err("missing subprotocol must error");
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { input } if input == "@RSYNCD: 31 md4"
        ));
    }

    #[test]
    fn from_parts_clamps_future_versions() {
        let constructed = LegacyDaemonGreetingOwned::from_parts(999, Some(1), None)
            .expect("future advertisement clamps");

        assert_eq!(constructed.protocol(), ProtocolVersion::NEWEST);
        assert_eq!(constructed.advertised_protocol(), 999);
        assert_eq!(constructed.subprotocol_raw(), Some(1));
    }

    #[test]
    fn greeting_details_rejects_missing_subprotocol_for_newer_versions() {
        let err = parse_legacy_daemon_greeting_details("@RSYNCD: 31\n").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn greeting_details_clamps_future_versions_but_retains_advertisement() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 999.1\n")
            .expect("future versions clamp");

        assert_eq!(greeting.protocol(), ProtocolVersion::NEWEST);
        assert_eq!(greeting.advertised_protocol(), 999);
        assert_eq!(greeting.subprotocol(), 1);
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
    fn clamps_future_versions_in_legacy_greeting() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 40.1\n").expect("must clamp");
        assert_eq!(parsed, ProtocolVersion::NEWEST);
    }

    #[test]
    fn parses_large_future_version_numbers_by_clamping() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 999999999999.0\n").expect("must clamp");
        assert_eq!(parsed, ProtocolVersion::NEWEST);
    }

    #[test]
    fn parse_ascii_digits_to_u32_saturates_on_overflow() {
        let digits = "999999999999999999999999999999";
        assert_eq!(parse_ascii_digits_to_u32(digits), u32::MAX);
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
    fn rejects_greeting_with_lowercase_prefix() {
        let err = parse_legacy_daemon_greeting("@rsyncd: 31.0\n").unwrap_err();
        match err {
            NegotiationError::MalformedLegacyGreeting { input } => {
                assert_eq!(input, "@rsyncd: 31.0");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn formats_legacy_daemon_greeting_for_newest_protocol() {
        let rendered = format_legacy_daemon_greeting(ProtocolVersion::NEWEST);
        assert_eq!(rendered, "@RSYNCD: 32.0\n");
    }

    #[test]
    fn formatted_legacy_greeting_round_trips_through_parser() {
        for &version in ProtocolVersion::supported_versions() {
            let rendered = format_legacy_daemon_greeting(version);
            let parsed = parse_legacy_daemon_greeting(&rendered)
                .unwrap_or_else(|err| panic!("failed to parse {rendered:?}: {err}"));
            assert_eq!(parsed, version);
        }
    }

    #[test]
    fn write_legacy_daemon_greeting_matches_formatter() {
        for &version in ProtocolVersion::supported_versions() {
            let mut rendered = String::new();
            write_legacy_daemon_greeting(&mut rendered, version).expect("writing to String");
            assert_eq!(rendered, format_legacy_daemon_greeting(version));
        }
    }

    #[test]
    fn write_legacy_daemon_greeting_propagates_errors() {
        struct FailingWriter {
            remaining: usize,
        }

        impl fmt::Write for FailingWriter {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                if self.remaining < s.len() {
                    self.remaining = 0;
                    return Err(fmt::Error);
                }
                self.remaining -= s.len();
                Ok(())
            }

            fn write_char(&mut self, ch: char) -> fmt::Result {
                let needed = ch.len_utf8();
                if self.remaining < needed {
                    self.remaining = 0;
                    return Err(fmt::Error);
                }
                self.remaining -= needed;
                Ok(())
            }
        }

        let mut writer = FailingWriter {
            remaining: LEGACY_DAEMON_PREFIX.len(),
        };
        assert!(write_legacy_daemon_greeting(&mut writer, ProtocolVersion::NEWEST).is_err());
    }
}
