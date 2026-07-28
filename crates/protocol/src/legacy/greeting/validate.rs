//! Presence gates for the `@RSYNCD:` daemon greeting.
//!
//! The structural greeting parser in [`super::parse`] is intentionally lenient:
//! it accepts a bare version banner so callers can inspect whatever a peer sent.
//! Upstream rsync applies the *policy* gates - which tokens a greeting must
//! carry for a given protocol - separately, inside `clientserver.c`'s
//! `exchange_protocols()`. This module exposes that same decision as a single
//! shared helper so every enforcement site (the daemon validating an incoming
//! client greeting, the client validating the daemon's greeting) agrees on the
//! exact gates without duplicating the logic.

use super::super::{LEGACY_DAEMON_PREFIX, LEGACY_DAEMON_PREFIX_LEN};
use super::advertised::AdvertisedDigests;

/// A required `@RSYNCD:` greeting token that the peer omitted.
///
/// upstream: clientserver.c:188-210 `exchange_protocols()` - the subprotocol
/// suffix and the digest name list are each mandatory past a protocol
/// threshold, and their absence is fatal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissingGreetingToken {
    /// The `.subprotocol` suffix, required when `protocol >= 30`.
    Subprotocol,
    /// The digest name list, required when `protocol > 31`.
    DigestList,
}

impl MissingGreetingToken {
    /// Returns the noun upstream uses to name the token in its diagnostics.
    ///
    /// Both the daemon's `@ERROR: your client omitted the <desc>: <line>` and
    /// the client's `rsync: the server omitted the <desc>: <line>` messages
    /// interpolate this text, matching upstream `clientserver.c:191/193` and
    /// `clientserver.c:207/209`.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Subprotocol => "subprotocol value",
            Self::DigestList => "digest name list",
        }
    }
}

/// Scans a newline-stripped line for the `@RSYNCD: <digits>` head, returning the
/// text after the prefix and how many leading digits the protocol number has.
///
/// This is upstream's `sscanf(buf, "@RSYNCD: %d.%d", ...) >= 1` test: the
/// literal prefix must match and at least one digit must follow (`%d` skips
/// leading whitespace first). `None` means the line is not a version banner.
fn scan_protocol_digits(trimmed: &str) -> Option<(&str, usize)> {
    let after_prefix = trimmed.strip_prefix(LEGACY_DAEMON_PREFIX)?;
    let rest = after_prefix.trim_start();
    let digits = rest
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    (digits > 0).then_some((rest, digits))
}

/// Splits the digest name list off a raw `@RSYNCD:` greeting line.
///
/// This is upstream's `daemon_auth_choices = strchr(buf + 9, ' ')` followed by
/// `strdup(daemon_auth_choices + 1)` (clientserver.c:199-203): the first space
/// past the nine-byte `"@RSYNCD: "` head opens the list, and everything after
/// that space *is* the list - the empty string when the space ends the line.
/// Trailing `\r`/`\n` are stripped first because upstream operates on the
/// newline-stripped `read_line_old()` buffer.
///
/// One rule, one implementation: both the presence gate in
/// [`missing_greeting_token`] and the greeting parser read the list through this
/// function, so they can never disagree about whether a peer advertised one.
#[doc(alias = "daemon_auth_choices")]
#[doc(alias = "@RSYNCD")]
#[must_use]
pub fn advertised_digests_in_greeting(line: &str) -> AdvertisedDigests<'_> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let Some(tail) = trimmed.get(LEGACY_DAEMON_PREFIX_LEN + 1..) else {
        return AdvertisedDigests::Absent;
    };

    match tail.find(' ') {
        Some(offset) => AdvertisedDigests::Present(&tail[offset + 1..]),
        None => AdvertisedDigests::Absent,
    }
}

/// Reports whether `line` is an `@RSYNCD:` version banner at all.
///
/// Mirrors upstream's `sscanf(buf, "@RSYNCD: %d.%d", ...) < 1` guard
/// (`clientserver.c:180`), which the daemon answers with
/// `@ERROR: protocol startup error`. A banner that parses but omits a required
/// token is a *different* refusal - see [`missing_greeting_token`].
#[doc(alias = "@RSYNCD")]
#[must_use]
pub fn is_version_banner(line: &str) -> bool {
    scan_protocol_digits(line.trim_end_matches(['\r', '\n'])).is_some()
}

/// Applies upstream `exchange_protocols()`'s presence gates to a raw `@RSYNCD:`
/// greeting line, returning the token the peer omitted, if any.
///
/// Returns `None` when the line is a well-formed greeting, is not an `@RSYNCD:`
/// version banner at all (see [`is_version_banner`] for that distinct case), or
/// is a legacy (`protocol < 30`) greeting that needs neither token. The
/// detection is byte-faithful to upstream `clientserver.c:180-211`:
///
/// - the subprotocol is parsed with the equivalent of
///   `sscanf(buf, "@RSYNCD: %d.%d", ...)`; a missing `.subprotocol` leaves the
///   value unset and is fatal for `remote_protocol >= 30`
///   (upstream: clientserver.c:188-197),
/// - the digest list is detected with the equivalent of
///   `strchr(buf + 9, ' ')` - any space past the `"@RSYNCD: "` prefix - and its
///   absence is fatal for `remote_protocol > 31`
///   (upstream: clientserver.c:199-211).
///
/// Trailing `\r`/`\n` are stripped first so the gate matches upstream, which
/// operates on the newline-stripped `read_line_old()` buffer.
#[doc(alias = "@RSYNCD")]
#[must_use]
pub fn missing_greeting_token(line: &str) -> Option<MissingGreetingToken> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let (rest, digits) = scan_protocol_digits(trimmed)?;
    let remote_protocol: u32 = rest[..digits].parse().unwrap_or(u32::MAX);

    // upstream: `remote_sub` stays < 0 unless a ".NNN" suffix follows the number.
    let has_subprotocol = rest[digits..]
        .strip_prefix('.')
        .and_then(|fractional| fractional.as_bytes().first())
        .is_some_and(u8::is_ascii_digit);
    if !has_subprotocol && remote_protocol >= 30 {
        return Some(MissingGreetingToken::Subprotocol);
    }

    // upstream: the gate is `daemon_auth_choices == NULL`, i.e. no space past
    // "@RSYNCD: " at all. A greeting that ends in that space advertised an
    // *empty* list, which is present and so passes this gate - it is refused
    // later, by digest negotiation.
    if advertised_digests_in_greeting(trimmed).is_absent() && remote_protocol > 31 {
        return Some(MissingGreetingToken::DigestList);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{
        AdvertisedDigests, MissingGreetingToken, advertised_digests_in_greeting,
        missing_greeting_token,
    };

    // upstream: clientserver.c:199-203 - `strchr(buf + 9, ' ')` finds the
    // trailing space, so `strdup(that + 1)` is a non-NULL EMPTY string. That is
    // a different state from the NULL a space-less greeting yields, and the
    // difference decides whether the peer is authenticated or refused.
    #[test]
    fn trailing_space_advertises_an_empty_list_not_an_absent_one() {
        assert_eq!(
            advertised_digests_in_greeting("@RSYNCD: 31.0 "),
            AdvertisedDigests::Present(""),
        );
        assert_eq!(
            advertised_digests_in_greeting("@RSYNCD: 31.0"),
            AdvertisedDigests::Absent,
        );
        assert_eq!(
            advertised_digests_in_greeting("@RSYNCD: 32.0 \r\n"),
            AdvertisedDigests::Present(""),
        );
        assert_eq!(
            advertised_digests_in_greeting("@RSYNCD: 32.0 md5 md4\n"),
            AdvertisedDigests::Present("md5 md4"),
        );
    }

    // Lines shorter than the nine-byte "@RSYNCD: " head cannot carry a list;
    // upstream's `buf + 9` walks straight into the terminator.
    #[test]
    fn short_lines_advertise_nothing() {
        assert_eq!(
            advertised_digests_in_greeting("@RSYNCD:"),
            AdvertisedDigests::Absent,
        );
        assert_eq!(
            advertised_digests_in_greeting(""),
            AdvertisedDigests::Absent
        );
    }

    // Regression for the refusal this fixes: at protocol 32 an EMPTY list is
    // still a list, so upstream does NOT answer "omitted the digest name list"
    // - it falls through to the daemon-auth checksum refusal. Verified against
    // rsync 3.4.4, which answers `@ERROR: your client does not support one of
    // our daemon-auth checksums: ...` for `@RSYNCD: 32.0 `.
    #[test]
    fn empty_digest_list_passes_the_presence_gate_at_protocol_32() {
        assert_eq!(missing_greeting_token("@RSYNCD: 32.0 "), None);
        assert_eq!(
            missing_greeting_token("@RSYNCD: 32.0"),
            Some(MissingGreetingToken::DigestList),
        );
    }

    // upstream: clientserver.c:188-197 - the subprotocol suffix is mandatory the
    // moment protocol reaches 30, not 31; both 30 and 32 without ".NNN" are fatal.
    #[test]
    fn subprotocol_required_from_protocol_30() {
        assert_eq!(
            missing_greeting_token("@RSYNCD: 30"),
            Some(MissingGreetingToken::Subprotocol),
        );
        assert_eq!(
            missing_greeting_token("@RSYNCD: 31"),
            Some(MissingGreetingToken::Subprotocol),
        );
        assert_eq!(
            missing_greeting_token("@RSYNCD: 32"),
            Some(MissingGreetingToken::Subprotocol),
        );
    }

    // upstream: clientserver.c:199-211 - protocol > 31 must carry a digest name
    // list even when the subprotocol suffix is present.
    #[test]
    fn digest_list_required_past_protocol_31() {
        assert_eq!(
            missing_greeting_token("@RSYNCD: 32.0"),
            Some(MissingGreetingToken::DigestList),
        );
        // protocol 31 needs the subprotocol but not a digest list.
        assert_eq!(missing_greeting_token("@RSYNCD: 31.0"), None);
    }

    // upstream: clientserver.c:196 - protocol < 30 defaults remote_sub to 0 and
    // needs no digest list, so a bare legacy version is accepted.
    #[test]
    fn legacy_versions_need_neither_token() {
        assert_eq!(missing_greeting_token("@RSYNCD: 29"), None);
        assert_eq!(missing_greeting_token("@RSYNCD: 28.0"), None);
    }

    // A fully-formed modern greeting carries both tokens and passes cleanly.
    #[test]
    fn well_formed_modern_greeting_passes() {
        assert_eq!(
            missing_greeting_token("@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n"),
            None,
        );
    }

    // Non-version lines are not greetings; the gate defers to normal parsing.
    #[test]
    fn non_version_lines_defer() {
        assert_eq!(missing_greeting_token("@RSYNCD: OK"), None);
        assert_eq!(missing_greeting_token("module"), None);
        assert_eq!(missing_greeting_token("@RSYNCD:"), None);
    }

    #[test]
    fn description_matches_upstream_nouns() {
        assert_eq!(
            MissingGreetingToken::Subprotocol.description(),
            "subprotocol value"
        );
        assert_eq!(
            MissingGreetingToken::DigestList.description(),
            "digest name list"
        );
    }
}
