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

/// Applies upstream `exchange_protocols()`'s presence gates to a raw `@RSYNCD:`
/// greeting line, returning the token the peer omitted, if any.
///
/// Returns `None` when the line is a well-formed greeting, is not an `@RSYNCD:`
/// version banner at all, or is a legacy (`protocol < 30`) greeting that needs
/// neither token. The detection is byte-faithful to upstream
/// `clientserver.c:180-211`:
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

    let after_prefix = trimmed.strip_prefix(LEGACY_DAEMON_PREFIX)?;

    // upstream: sscanf `%d` skips leading whitespace before the protocol number.
    let rest = after_prefix.trim_start();
    let digits = rest
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 {
        // sscanf(...) < 1: not a version banner - the caller keeps parsing.
        return None;
    }
    let remote_protocol: u32 = rest[..digits].parse().unwrap_or(u32::MAX);

    // upstream: `remote_sub` stays < 0 unless a ".NNN" suffix follows the number.
    let has_subprotocol = rest[digits..]
        .strip_prefix('.')
        .and_then(|fractional| fractional.as_bytes().first())
        .is_some_and(u8::is_ascii_digit);
    if !has_subprotocol && remote_protocol >= 30 {
        return Some(MissingGreetingToken::Subprotocol);
    }

    // upstream: `daemon_auth_choices = strchr(buf + 9, ' ')` - any space past
    // "@RSYNCD: " marks the start of the digest-name list.
    let has_digest_list = trimmed
        .as_bytes()
        .get(LEGACY_DAEMON_PREFIX_LEN + 1..)
        .is_some_and(|tail| tail.contains(&b' '));
    if !has_digest_list && remote_protocol > 31 {
        return Some(MissingGreetingToken::DigestList);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{MissingGreetingToken, missing_greeting_token};

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
