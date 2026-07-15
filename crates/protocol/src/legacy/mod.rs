//! Legacy ASCII `@RSYNCD:` negotiation helpers shared by daemon clients and servers.
//!
//! The legacy path is the only daemon handshake before the binary multiplex stream
//! takes over. It is the sole negotiation form for peers limited to protocols older
//! than 30, and remains the bootstrap exchange even at protocol 32: the version
//! line, optional subprotocol suffix, and digest list are all transported as ASCII
//! `@RSYNCD:` records before the connection switches to framed I/O. This module
//! groups the prefix constants together with the byte-, string-, and structured
//! parsers that mirror upstream rsync's wire formatting for greetings, error
//! banners, and warning lines.
//!
//! upstream: clientserver.c:read_line / start_inband_exchange (rsync 3.4.1)

use crate::error::NegotiationError;

/// Canonical ASCII prefix that identifies the legacy `@RSYNCD:` negotiation style.
///
/// Upstream rsync emits this exact marker (including the trailing colon) before sending
/// the daemon greeting when a peer is limited to protocols older than 30. Exposing the
/// constant allows higher layers to reference the prefix without duplicating the literal,
/// keeping diagnostics and buffer sizing in sync with the canonical value.
pub const LEGACY_DAEMON_PREFIX: &str = "@RSYNCD:";

/// Number of bytes in [`LEGACY_DAEMON_PREFIX`].
///
/// The length matches the canonical prefix observed on the wire and is exported so
/// transports can size temporary buffers without recomputing it at runtime.
pub const LEGACY_DAEMON_PREFIX_LEN: usize = LEGACY_DAEMON_PREFIX.len();

/// Canonical byte representation of [`LEGACY_DAEMON_PREFIX`].
///
/// Legacy negotiation helpers frequently need to work with the ASCII prefix as raw bytes.
/// Publishing the array keeps those call-sites allocation-free and avoids repeated
/// conversions via [`str::as_bytes`].
pub const LEGACY_DAEMON_PREFIX_BYTES: &[u8; LEGACY_DAEMON_PREFIX_LEN] = b"@RSYNCD:";

mod bytes;
mod greeting;
mod lines;

pub use bytes::{
    parse_legacy_daemon_greeting_bytes, parse_legacy_daemon_greeting_bytes_details,
    parse_legacy_daemon_greeting_bytes_owned, parse_legacy_daemon_message_bytes,
    parse_legacy_error_message_bytes, parse_legacy_warning_message_bytes,
};
#[allow(unused_imports)] // REASON: convenience re-export; not all items used in every consumer
pub use greeting::write_legacy_daemon_greeting;
#[allow(unused_imports)] // REASON: convenience re-export; not all items used in every consumer
pub use greeting::{
    DigestListTokens, LegacyDaemonGreeting, LegacyDaemonGreetingOwned, MissingGreetingToken,
    format_legacy_daemon_greeting, missing_greeting_token, parse_legacy_daemon_greeting,
    parse_legacy_daemon_greeting_details, parse_legacy_daemon_greeting_owned,
};
#[allow(unused_imports)] // REASON: convenience re-export; not all items used in every consumer
pub use lines::{
    LegacyDaemonMessage, format_legacy_daemon_message, parse_legacy_daemon_message,
    parse_legacy_error_message, parse_legacy_warning_message, write_legacy_daemon_message,
};

/// Builds a [`NegotiationError::MalformedLegacyGreeting`] from a trimmed legacy line.
///
/// Captures the offending input verbatim so diagnostics can echo what the daemon sent,
/// matching upstream rsync's `@ERROR: protocol startup error` flow that includes the
/// greeting bytes verbatim.
///
/// upstream: clientserver.c:read_line (`@ERROR: protocol startup error`)
pub(super) fn malformed_legacy_greeting(trimmed: &str) -> NegotiationError {
    NegotiationError::MalformedLegacyGreeting {
        input: trimmed.to_owned(),
    }
}

/// Renders a possibly invalid byte sequence into a trimmed lossy [`String`] for diagnostics.
///
/// Trailing CR/LF terminators are removed so error messages match the canonical
/// trimmed form that upstream rsync echoes back to the user when rejecting a greeting.
pub(super) fn lossy_trimmed_input(bytes: &[u8]) -> String {
    let mut owned = String::from_utf8_lossy(bytes).into_owned();
    let trimmed_len = owned.trim_end_matches(['\r', '\n']).len();
    if trimmed_len != owned.len() {
        owned.truncate(trimmed_len);
    }
    owned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossy_trimmed_input_drops_trailing_newlines() {
        assert_eq!(lossy_trimmed_input(b"@RSYNCD: OK\r\n"), "@RSYNCD: OK");
    }

    #[test]
    fn lossy_trimmed_input_replaces_invalid_utf8() {
        assert_eq!(
            lossy_trimmed_input(b"@RSYNCD: AUTHREQD\xff\n"),
            "@RSYNCD: AUTHREQD\u{fffd}"
        );
    }

    #[test]
    fn malformed_legacy_greeting_preserves_trimmed_input() {
        let err = malformed_legacy_greeting("@RSYNCD: ???");
        assert_eq!(
            err,
            NegotiationError::MalformedLegacyGreeting {
                input: "@RSYNCD: ???".to_owned(),
            }
        );
    }
}
