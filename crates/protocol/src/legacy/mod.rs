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
#[allow(unused_imports)]
pub use greeting::write_legacy_daemon_greeting;
pub use greeting::{
    DigestListTokens, LegacyDaemonGreeting, LegacyDaemonGreetingOwned,
    format_legacy_daemon_greeting, parse_legacy_daemon_greeting,
    parse_legacy_daemon_greeting_details, parse_legacy_daemon_greeting_owned,
};
pub use lines::{
    LegacyDaemonMessage, parse_legacy_daemon_message, parse_legacy_error_message,
    parse_legacy_warning_message,
};

pub(super) fn malformed_legacy_greeting(trimmed: &str) -> NegotiationError {
    NegotiationError::MalformedLegacyGreeting {
        input: trimmed.to_owned(),
    }
}

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
