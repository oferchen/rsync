//! The digest name list advertised in an `@RSYNCD:` greeting.
//!
//! Upstream splits the list off the greeting with
//! `daemon_auth_choices = strchr(buf + 9, ' ')` and, when that finds a space,
//! keeps `strdup(daemon_auth_choices + 1)` (clientserver.c:199-203). A greeting
//! whose last byte *is* that space therefore produces a non-NULL **empty**
//! string, which upstream treats differently from the NULL a space-less
//! greeting produces. This module carries that distinction in the type system so
//! it cannot be lost on the way to `negotiate_daemon_auth()`.

use super::tokens::DigestListTokens;

/// The daemon-auth digest names a peer advertised in its `@RSYNCD:` greeting.
///
/// The two states drive different upstream code paths and must never be
/// conflated:
///
/// - [`Self::Absent`] - no space followed the `"@RSYNCD: "` head, so
///   `daemon_auth_choices` stays NULL. `negotiate_daemon_auth()` substitutes
///   `protocol_version >= 30 ? "md5" : "md4"` and sets `md4_is_old`
///   (compat.c:857-862), which always negotiates successfully.
/// - [`Self::Present`] - a space was found, so `daemon_auth_choices` is the text
///   after it and may be empty. `parse_negotiate_str()` walks that text and
///   returns 0 when it yields no mutually supported name (compat.c:333-364),
///   which is fatal on both sides: the daemon answers
///   `@ERROR: your client does not support one of our daemon-auth checksums`
///   and the client aborts with `Failed to negotiate a daemon auth checksum
///   choice.`, both exiting `RERR_UNSUPPORTED`.
///
/// Collapsing `Present("")` into `Absent` grants access upstream refuses, which
/// is why this is an enum rather than an `Option<&str>` whose `None` and
/// `Some("")` invite exactly that mistake.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AdvertisedDigests<'a> {
    /// The greeting named no digest list at all.
    #[default]
    Absent,
    /// The greeting named a digest list, whose verbatim contents may be empty.
    Present(&'a str),
}

impl<'a> AdvertisedDigests<'a> {
    /// Returns the advertised names, or `None` when no list was advertised.
    ///
    /// `Some("")` is a real answer: the peer advertised an empty list.
    #[must_use]
    pub const fn names(self) -> Option<&'a str> {
        match self {
            Self::Absent => None,
            Self::Present(names) => Some(names),
        }
    }

    /// Reports whether the greeting advertised a list, empty or not.
    #[must_use]
    pub const fn is_present(self) -> bool {
        matches!(self, Self::Present(_))
    }

    /// Reports whether the greeting advertised no list at all.
    #[must_use]
    pub const fn is_absent(self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Returns an iterator over the whitespace-separated digest tokens.
    ///
    /// An [`Self::Absent`] list and a [`Self::Present`] but empty list both
    /// yield no tokens; use [`Self::is_present`] to tell them apart.
    #[must_use]
    pub fn tokens(self) -> DigestListTokens<'a> {
        DigestListTokens::new(self.names())
    }

    /// Reports whether the peer advertised support for `name`.
    ///
    /// Matching is ASCII case-insensitive because daemons emit lowercase names
    /// while callers often hold uppercase constants. Whitespace around the query
    /// is ignored and an empty query never matches.
    #[must_use]
    pub fn supports(self, name: &str) -> bool {
        let trimmed = name.trim_matches(|ch: char| ch.is_ascii_whitespace());
        if trimmed.is_empty() {
            return false;
        }

        self.tokens()
            .any(|token| token.eq_ignore_ascii_case(trimmed))
    }
}

impl<'a> From<Option<&'a str>> for AdvertisedDigests<'a> {
    /// Lifts the raw `daemon_auth_choices` pointer into the modelled states:
    /// `None` is upstream's NULL and `Some(names)` its `strdup` result, empty
    /// string included.
    fn from(names: Option<&'a str>) -> Self {
        match names {
            None => Self::Absent,
            Some(names) => Self::Present(names),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AdvertisedDigests;

    // The whole point of the type: an empty list is advertised, an absent one is
    // not, and both tokenize to nothing. A caller that only looked at the tokens
    // would grant access where upstream refuses.
    #[test]
    fn empty_list_is_present_but_absent_list_is_not() {
        let empty = AdvertisedDigests::Present("");
        let absent = AdvertisedDigests::Absent;

        assert!(empty.is_present());
        assert!(!empty.is_absent());
        assert_eq!(empty.names(), Some(""));
        assert_eq!(empty.tokens().count(), 0);

        assert!(absent.is_absent());
        assert!(!absent.is_present());
        assert_eq!(absent.names(), None);
        assert_eq!(absent.tokens().count(), 0);

        assert_ne!(empty, absent);
    }

    #[test]
    fn tokens_preserve_order_and_split_on_ascii_whitespace() {
        let digests = AdvertisedDigests::Present("sha512\tsha256  md5");
        let tokens: Vec<_> = digests.tokens().collect();
        assert_eq!(tokens, ["sha512", "sha256", "md5"]);
    }

    #[test]
    fn supports_ignores_case_and_padding() {
        let digests = AdvertisedDigests::Present("md4 MD5");
        assert!(digests.supports("MD4"));
        assert!(digests.supports(" md5 "));
        assert!(!digests.supports("sha1"));
        assert!(!digests.supports(""));
        assert!(!AdvertisedDigests::Absent.supports("md5"));
    }

    #[test]
    fn conversion_from_optional_names_keeps_the_empty_case_present() {
        assert_eq!(
            AdvertisedDigests::from(Some("")),
            AdvertisedDigests::Present(""),
        );
        assert_eq!(AdvertisedDigests::from(None), AdvertisedDigests::Absent);
        assert_eq!(AdvertisedDigests::default(), AdvertisedDigests::Absent);
    }
}
