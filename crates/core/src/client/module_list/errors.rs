use std::io::{self, BufRead};

use crate::client::module_list::parsing::strip_prefix_ignore_ascii_case;

use rsync_protocol::{NegotiationError, parse_legacy_error_message};

use super::super::{
    ClientError, PARTIAL_TRANSFER_EXIT_CODE, daemon_error, daemon_protocol_error, socket_error,
};
use super::DaemonAddress;

pub(crate) fn read_trimmed_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;

    if bytes == 0 {
        return Ok(None);
    }

    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }

    Ok(Some(line))
}

pub(crate) fn legacy_daemon_error_payload(line: &str) -> Option<String> {
    if let Some(payload) = parse_legacy_error_message(line) {
        return Some(payload.to_string());
    }

    let trimmed = line.trim_matches(['\r', '\n']).trim_start();
    let remainder = strip_prefix_ignore_ascii_case(trimmed, "@ERROR")?;

    if remainder
        .chars()
        .next()
        .filter(|ch| *ch != ':' && !ch.is_ascii_whitespace())
        .is_some()
    {
        return None;
    }

    let payload = remainder
        .trim_start_matches(|ch: char| ch == ':' || ch.is_ascii_whitespace())
        .trim();

    Some(payload.to_string())
}

pub(crate) fn map_daemon_handshake_error(error: io::Error, addr: &DaemonAddress) -> ClientError {
    if let Some(mapped) = handshake_error_to_client_error(&error) {
        mapped
    } else {
        match daemon_error_from_invalid_data(&error) {
            Some(mapped) => mapped,
            None => socket_error("negotiate with", addr.socket_addr_display(), error),
        }
    }
}

fn handshake_error_to_client_error(error: &io::Error) -> Option<ClientError> {
    let negotiation_error = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<NegotiationError>())?;

    if let Some(input) = negotiation_error.malformed_legacy_greeting() {
        if let Some(payload) = legacy_daemon_error_payload(input) {
            return Some(daemon_error(payload, PARTIAL_TRANSFER_EXIT_CODE));
        }

        return Some(daemon_protocol_error(input));
    }

    None
}

fn daemon_error_from_invalid_data(error: &io::Error) -> Option<ClientError> {
    if error.kind() != io::ErrorKind::InvalidData {
        return None;
    }

    let payload_candidates = error
        .get_ref()
        .map(|inner| inner.to_string())
        .into_iter()
        .chain(std::iter::once(error.to_string()));

    for candidate in payload_candidates {
        if let Some(payload) = legacy_daemon_error_payload(&candidate) {
            return Some(daemon_error(payload, PARTIAL_TRANSFER_EXIT_CODE));
        }
    }

    None
}
