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
