use crate::error::NegotiationError;
use crate::version::ProtocolVersion;
use std::borrow::ToOwned;

use super::super::LEGACY_DAEMON_PREFIX;
use super::tokens::DigestListTokens;

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

    pub(super) fn new(
        protocol: ProtocolVersion,
        advertised_protocol: u32,
        subprotocol: Option<u32>,
        digest_list: Option<&'a str>,
    ) -> Self {
        Self {
            protocol,
            advertised_protocol,
            subprotocol,
            digest_list,
        }
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
    /// The helper mirrors [`crate::parse_legacy_daemon_greeting_details`] by clamping
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
