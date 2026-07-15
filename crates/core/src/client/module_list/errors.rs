use std::io::{self, BufRead};

use crate::client::module_list::parsing::strip_prefix_ignore_ascii_case;

use protocol::{NegotiationError, parse_legacy_error_message};

use super::super::{
    ClientError, PARTIAL_TRANSFER_EXIT_CODE, PROTOCOL_INCOMPATIBLE_EXIT_CODE, daemon_error,
    daemon_protocol_error, socket_error,
};
use super::DaemonAddress;

pub(crate) fn read_trimmed_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();

    let bytes = loop {
        match reader.read_line(&mut line) {
            Ok(bytes) => break bytes,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                line.clear();
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::BrokenPipe
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
    };

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
        return Some(payload.to_owned());
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

    Some(payload.to_owned())
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

    // upstream: compat.c:619-623 setup_protocol - a daemon that advertises a
    // protocol version outside [MIN_PROTOCOL_VERSION, MAX_PROTOCOL_VERSION]
    // makes the client exit_cleanup(RERR_PROTOCOL) (exit 2). Both the
    // out-of-range (UnsupportedVersion) and no-mutual-version cases carry that
    // negotiation failure, so map them to RERR_PROTOCOL rather than falling
    // through to the RERR_SOCKETIO (10) socket-error path.
    if negotiation_error.unsupported_version().is_some()
        || negotiation_error.peer_versions().is_some()
    {
        return Some(daemon_error(
            negotiation_error.to_string(),
            PROTOCOL_INCOMPATIBLE_EXIT_CODE,
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, BufRead, Cursor, Read};

    #[test]
    fn read_trimmed_line_treats_connection_reset_as_eof() {
        let mut reader = ErrorReader::new(io::ErrorKind::ConnectionReset);
        let result = read_trimmed_line(&mut reader).expect("connection reset treated as eof");
        assert!(result.is_none());
    }

    #[test]
    fn read_trimmed_line_treats_connection_aborted_as_eof() {
        let mut reader = ErrorReader::new(io::ErrorKind::ConnectionAborted);
        let result = read_trimmed_line(&mut reader).expect("connection aborted treated as eof");
        assert!(result.is_none());
    }

    #[test]
    fn read_trimmed_line_retries_on_interrupted_errors() {
        let mut reader = InterruptedThenLine::new("payload\n");
        let result = read_trimmed_line(&mut reader).expect("interrupted call should retry");
        assert_eq!(result.as_deref(), Some("payload"));
    }

    #[test]
    fn read_trimmed_line_trims_newline_terminators() {
        let mut reader = Cursor::new(b"hello world\r\n");
        let result = read_trimmed_line(&mut reader).expect("cursor read should succeed");
        assert_eq!(result.as_deref(), Some("hello world"));
    }

    struct ErrorReader {
        kind: io::ErrorKind,
        emitted: bool,
    }

    impl ErrorReader {
        fn new(kind: io::ErrorKind) -> Self {
            Self {
                kind,
                emitted: false,
            }
        }
    }

    impl Read for ErrorReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            if self.emitted {
                Ok(0)
            } else {
                self.emitted = true;
                Err(io::Error::new(self.kind, "synthetic read failure"))
            }
        }
    }

    impl BufRead for ErrorReader {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.emitted {
                Ok(&[])
            } else {
                self.emitted = true;
                Err(io::Error::new(self.kind, "synthetic buffer failure"))
            }
        }

        fn consume(&mut self, _amt: usize) {}
    }

    struct InterruptedThenLine {
        bytes: Vec<u8>,
        offset: usize,
        interrupted: bool,
    }

    impl InterruptedThenLine {
        fn new(line: &str) -> Self {
            Self {
                bytes: line.as_bytes().to_vec(),
                offset: 0,
                interrupted: false,
            }
        }
    }

    impl Read for InterruptedThenLine {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.fill_buf() {
                Ok(data) => {
                    if data.is_empty() {
                        return Ok(0);
                    }

                    let len = data.len().min(buf.len());
                    buf[..len].copy_from_slice(&data[..len]);
                    self.consume(len);
                    Ok(len)
                }
                Err(error) => Err(error),
            }
        }
    }

    impl BufRead for InterruptedThenLine {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if !self.interrupted {
                self.interrupted = true;
                Err(io::Error::new(io::ErrorKind::Interrupted, "synthetic"))
            } else {
                Ok(&self.bytes[self.offset..])
            }
        }

        fn consume(&mut self, amt: usize) {
            self.offset = usize::min(self.bytes.len(), self.offset.saturating_add(amt));
        }
    }

    #[test]
    fn legacy_daemon_error_payload_parses_at_error_prefix() {
        let result = legacy_daemon_error_payload("@ERROR: access denied");
        assert_eq!(result, Some("access denied".to_owned()));
    }

    #[test]
    fn legacy_daemon_error_payload_parses_at_error_without_colon() {
        let result = legacy_daemon_error_payload("@ERROR access denied");
        assert_eq!(result, Some("access denied".to_owned()));
    }

    #[test]
    fn legacy_daemon_error_payload_handles_leading_whitespace() {
        let result = legacy_daemon_error_payload("  @ERROR: some error");
        assert_eq!(result, Some("some error".to_owned()));
    }

    #[test]
    fn legacy_daemon_error_payload_handles_crlf() {
        let result = legacy_daemon_error_payload("@ERROR: module not found\r\n");
        assert_eq!(result, Some("module not found".to_owned()));
    }

    #[test]
    fn legacy_daemon_error_payload_returns_none_for_non_error() {
        let result = legacy_daemon_error_payload("@RSYNCD: 31.0");
        assert!(result.is_none());
    }

    #[test]
    fn legacy_daemon_error_payload_returns_none_for_attached_text() {
        let result = legacy_daemon_error_payload("@ERRORsome text");
        assert!(result.is_none());
    }

    #[test]
    fn legacy_daemon_error_payload_handles_empty_payload() {
        let result = legacy_daemon_error_payload("@ERROR:");
        assert_eq!(result, Some("".to_owned()));
    }

    #[test]
    fn legacy_daemon_error_payload_case_insensitive() {
        let result = legacy_daemon_error_payload("@error: lowercase");
        assert_eq!(result, Some("lowercase".to_owned()));
    }

    #[test]
    fn read_trimmed_line_returns_none_on_empty_input() {
        let mut reader = Cursor::new(b"");
        let result = read_trimmed_line(&mut reader).expect("read should succeed");
        assert!(result.is_none());
    }

    #[test]
    fn read_trimmed_line_trims_multiple_newlines() {
        let mut reader = Cursor::new(b"hello\r\n\r\n");
        let result = read_trimmed_line(&mut reader).expect("read");
        assert_eq!(result.as_deref(), Some("hello"));
    }

    #[test]
    fn read_trimmed_line_handles_lf_only() {
        let mut reader = Cursor::new(b"line\n");
        let result = read_trimmed_line(&mut reader).expect("read");
        assert_eq!(result.as_deref(), Some("line"));
    }

    #[test]
    fn read_trimmed_line_handles_cr_only() {
        let mut reader = Cursor::new(b"line\r");
        let result = read_trimmed_line(&mut reader).expect("read");
        assert_eq!(result.as_deref(), Some("line"));
    }

    #[test]
    fn read_trimmed_line_treats_broken_pipe_as_eof() {
        let mut reader = ErrorReader::new(io::ErrorKind::BrokenPipe);
        let result = read_trimmed_line(&mut reader).expect("broken pipe as eof");
        assert!(result.is_none());
    }

    // WHY: upstream compat.c:619-623 setup_protocol exits RERR_PROTOCOL (2) when
    // the daemon advertises a protocol version outside
    // [MIN_PROTOCOL_VERSION, MAX_PROTOCOL_VERSION]. oc rejects the out-of-range
    // greeting during negotiation, so the daemon handshake mapper must report
    // exit 2 - not the RERR_SOCKETIO (10) socket-error fallback - so a drop-in
    // tool distinguishes a protocol mismatch from a transport failure.
    #[test]
    fn out_of_range_daemon_version_maps_to_rerr_protocol() {
        let error: io::Error = NegotiationError::UnsupportedVersion(999).into();
        let mapped =
            handshake_error_to_client_error(&error).expect("out-of-range version is classified");
        assert_eq!(mapped.exit_code(), PROTOCOL_INCOMPATIBLE_EXIT_CODE);
        assert_eq!(mapped.exit_code(), 2);
    }

    // WHY: a daemon whose advertised versions do not overlap our supported set
    // is the same protocol-incompatibility class and must also exit 2.
    #[test]
    fn no_mutual_daemon_version_maps_to_rerr_protocol() {
        let error: io::Error = NegotiationError::NoMutualProtocol {
            peer_versions: vec![20, 21],
        }
        .into();
        let mapped =
            handshake_error_to_client_error(&error).expect("no-mutual version is classified");
        assert_eq!(mapped.exit_code(), PROTOCOL_INCOMPATIBLE_EXIT_CODE);
    }

    // Regression guard: a genuine transport failure (not a NegotiationError)
    // must NOT be reclassified as a protocol error; it stays on the socket-error
    // path (RERR_SOCKETIO, 10) handled by the caller.
    #[test]
    fn non_negotiation_error_is_not_classified_as_protocol() {
        let error = io::Error::new(io::ErrorKind::ConnectionReset, "reset");
        assert!(handshake_error_to_client_error(&error).is_none());
    }
}
