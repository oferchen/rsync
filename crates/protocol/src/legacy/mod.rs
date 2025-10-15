use crate::error::NegotiationError;

pub(crate) const LEGACY_DAEMON_PREFIX: &str = "@RSYNCD:";
pub(crate) const LEGACY_DAEMON_PREFIX_LEN: usize = LEGACY_DAEMON_PREFIX.len();

mod bytes;
mod greeting;
mod lines;

pub use bytes::{
    parse_legacy_daemon_greeting_bytes, parse_legacy_daemon_message_bytes,
    parse_legacy_error_message_bytes, parse_legacy_warning_message_bytes,
};
pub use greeting::{format_legacy_daemon_greeting, parse_legacy_daemon_greeting};
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
        assert_eq!(lossy_trimmed_input(b"@RSYNCD: AUTHREQD\xff\n"), "@RSYNCD: AUTHREQD\u{fffd}");
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
