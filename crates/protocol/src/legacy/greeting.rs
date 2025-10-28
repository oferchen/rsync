use crate::error::NegotiationError;
use crate::version::ProtocolVersion;
use core::fmt::{self, Write as FmtWrite};
use std::borrow::ToOwned;

mod tokens;
pub use tokens::DigestListTokens;
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
/// Upstream daemons send a line such as `@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n` when speaking to
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
mod tests;
