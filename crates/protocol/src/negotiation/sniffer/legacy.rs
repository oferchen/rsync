use std::collections::TryReserveError;
use std::io::{self, Read};

use memchr::memchr;

use crate::legacy::{
    LegacyDaemonGreeting, parse_legacy_daemon_greeting_bytes,
    parse_legacy_daemon_greeting_bytes_details,
};
use crate::version::ProtocolVersion;

use super::super::NegotiationPrologue;
use super::NegotiationPrologueSniffer;

/// Reads the complete legacy daemon line after the `@RSYNCD:` prefix has been buffered.
pub fn read_legacy_daemon_line<R: Read>(
    sniffer: &mut NegotiationPrologueSniffer,
    reader: &mut R,
    line: &mut Vec<u8>,
) -> io::Result<()> {
    match sniffer.decision() {
        Some(NegotiationPrologue::LegacyAscii) => {
            if !sniffer.legacy_prefix_complete() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation prefix is incomplete",
                ));
            }
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "legacy negotiation has not been detected",
            ));
        }
    }

    sniffer
        .take_buffered_into(line)
        .map_err(map_reserve_error_for_io)?;

    if let Some(newline_index) = memchr(b'\n', line) {
        let remainder_start = newline_index + 1;
        let remainder_len = line.len() - remainder_start;
        let drain = line.drain(remainder_start..);
        if remainder_len > 0 {
            sniffer
                .buffered_storage_mut()
                .try_reserve_exact(remainder_len)
                .map_err(map_reserve_error_for_io)?;
            sniffer.buffered_storage_mut().extend(drain);
        }
        return Ok(());
    }

    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF while reading legacy rsync daemon line",
                ));
            }
            Ok(_) => {
                line.try_reserve(1).map_err(map_reserve_error_for_io)?;
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

/// Reads and parses the legacy daemon greeting after the negotiation prefix has been buffered.
pub fn read_and_parse_legacy_daemon_greeting<R: Read>(
    sniffer: &mut NegotiationPrologueSniffer,
    reader: &mut R,
    line: &mut Vec<u8>,
) -> io::Result<ProtocolVersion> {
    read_legacy_daemon_line(sniffer, reader, line)?;
    parse_legacy_daemon_greeting_bytes(line).map_err(io::Error::from)
}

/// Reads and parses the legacy daemon greeting, returning a detailed view.
pub fn read_and_parse_legacy_daemon_greeting_details<'a, R: Read>(
    sniffer: &mut NegotiationPrologueSniffer,
    reader: &mut R,
    line: &'a mut Vec<u8>,
) -> io::Result<LegacyDaemonGreeting<'a>> {
    read_legacy_daemon_line(sniffer, reader, line)?;
    parse_legacy_daemon_greeting_bytes_details(line).map_err(io::Error::from)
}

#[derive(Debug, thiserror::Error)]
#[error("failed to reserve memory for legacy negotiation buffer: {inner}")]
struct LegacyBufferReserveError {
    #[source]
    inner: TryReserveError,
}

impl LegacyBufferReserveError {
    const fn new(inner: TryReserveError) -> Self {
        Self { inner }
    }
}

pub(crate) fn map_reserve_error_for_io(err: TryReserveError) -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        LegacyBufferReserveError::new(err),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Prepares a sniffer with legacy ASCII negotiation detected and all data buffered.
    ///
    /// Note: `observe()` returns after detecting the `@RSYNCD:` prefix (8 bytes),
    /// so any additional bytes in `data` must be manually added to the buffer.
    fn prepare_sniffer_for_legacy(data: &[u8]) -> NegotiationPrologueSniffer {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (_, consumed) = sniffer.observe(data).expect("observe should succeed");
        // Add any bytes beyond what was consumed by observe()
        if consumed < data.len() {
            sniffer
                .buffered_storage_mut()
                .extend_from_slice(&data[consumed..]);
        }
        sniffer
    }

    // ==================== read_legacy_daemon_line tests ====================

    #[test]
    fn read_legacy_daemon_line_complete_in_buffer() {
        // Complete greeting already buffered
        let data = b"@RSYNCD: 31.0\n";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        let mut line = Vec::new();
        let mut reader = Cursor::new(Vec::<u8>::new());
        read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line).unwrap();

        assert_eq!(line, b"@RSYNCD: 31.0\n");
    }

    #[test]
    fn read_legacy_daemon_line_partial_in_buffer() {
        // Only prefix and partial version in buffer
        let data = b"@RSYNCD: 31";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        // Reader provides the rest
        let remainder = b".0\n";
        let mut reader = Cursor::new(remainder.to_vec());
        let mut line = Vec::new();
        read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line).unwrap();

        assert_eq!(line, b"@RSYNCD: 31.0\n");
    }

    #[test]
    fn read_legacy_daemon_line_only_prefix_buffered() {
        // Only the @RSYNCD: prefix is buffered
        let data = b"@RSYNCD:";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        let remainder = b" 31.0\n";
        let mut reader = Cursor::new(remainder.to_vec());
        let mut line = Vec::new();
        read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line).unwrap();

        assert_eq!(line, b"@RSYNCD: 31.0\n");
    }

    #[test]
    fn read_legacy_daemon_line_preserves_remainder_in_sniffer() {
        // Data with extra bytes after newline
        let data = b"@RSYNCD: 31.0\nextra data";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        let mut line = Vec::new();
        let mut reader = Cursor::new(Vec::<u8>::new());
        read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line).unwrap();

        // Line should only contain up to newline
        assert_eq!(line, b"@RSYNCD: 31.0\n");

        // Remainder should be preserved in sniffer
        assert_eq!(sniffer.buffered_storage(), b"extra data");
    }

    #[test]
    fn read_legacy_daemon_line_incomplete_prefix_error() {
        // Only partial prefix observed
        let data = b"@RSYNC";
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(data).expect("observe succeeds");

        let mut line = Vec::new();
        let mut reader = Cursor::new(Vec::<u8>::new());
        let result = read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn read_legacy_daemon_line_binary_negotiation_error() {
        // Binary negotiation detected (starts with 0x00)
        let data = b"\x00\x00\x00\x00";
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(data).expect("observe succeeds");

        let mut line = Vec::new();
        let mut reader = Cursor::new(Vec::<u8>::new());
        let result = read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn read_legacy_daemon_line_eof_error() {
        // Prefix complete but no newline in buffer or reader
        let data = b"@RSYNCD: 31.0";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        // Empty reader - will hit EOF
        let mut reader = Cursor::new(Vec::<u8>::new());
        let mut line = Vec::new();
        let result = read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    // ==================== read_and_parse_legacy_daemon_greeting tests ====================

    #[test]
    fn read_and_parse_greeting_version_31() {
        let data = b"@RSYNCD: 31.0\n";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        let mut line = Vec::new();
        let mut reader = Cursor::new(Vec::<u8>::new());
        let version =
            read_and_parse_legacy_daemon_greeting(&mut sniffer, &mut reader, &mut line).unwrap();

        assert_eq!(version.as_u8(), 31);
    }

    #[test]
    fn read_and_parse_greeting_version_29() {
        // Protocol 29 is the last to use ASCII negotiation
        let data = b"@RSYNCD: 29.0\n";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        let mut line = Vec::new();
        let mut reader = Cursor::new(Vec::<u8>::new());
        let version =
            read_and_parse_legacy_daemon_greeting(&mut sniffer, &mut reader, &mut line).unwrap();

        assert_eq!(version.as_u8(), 29);
    }

    #[test]
    fn read_and_parse_greeting_from_reader() {
        // Only prefix buffered, rest from reader
        let data = b"@RSYNCD:";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        let remainder = b" 32.0\n";
        let mut reader = Cursor::new(remainder.to_vec());
        let mut line = Vec::new();
        let version =
            read_and_parse_legacy_daemon_greeting(&mut sniffer, &mut reader, &mut line).unwrap();

        assert_eq!(version.as_u8(), 32);
    }

    #[test]
    fn read_and_parse_greeting_invalid_format_error() {
        let data = b"@RSYNCD: invalid\n";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        let mut line = Vec::new();
        let mut reader = Cursor::new(Vec::<u8>::new());
        let result = read_and_parse_legacy_daemon_greeting(&mut sniffer, &mut reader, &mut line);

        assert!(result.is_err());
    }

    // ==================== read_and_parse_legacy_daemon_greeting_details tests ====================

    #[test]
    fn read_and_parse_greeting_details_basic() {
        let data = b"@RSYNCD: 31.0\n";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        let mut line = Vec::new();
        let mut reader = Cursor::new(Vec::<u8>::new());
        let greeting =
            read_and_parse_legacy_daemon_greeting_details(&mut sniffer, &mut reader, &mut line)
                .unwrap();

        assert_eq!(greeting.protocol().as_u8(), 31);
    }

    #[test]
    fn read_and_parse_greeting_details_with_subprotocol() {
        let data = b"@RSYNCD: 31.0 sha512 xxh3\n";
        let mut sniffer = prepare_sniffer_for_legacy(data);

        let mut line = Vec::new();
        let mut reader = Cursor::new(Vec::<u8>::new());
        let greeting =
            read_and_parse_legacy_daemon_greeting_details(&mut sniffer, &mut reader, &mut line)
                .unwrap();

        assert_eq!(greeting.protocol().as_u8(), 31);
        // Should have checksum digests
        assert!(greeting.digest_list().is_some());
    }

    // ==================== LegacyBufferReserveError tests ====================

    #[test]
    fn legacy_buffer_reserve_error_debug() {
        // Create a TryReserveError by trying to reserve an absurd amount
        let mut v: Vec<u8> = Vec::new();
        let reserve_err = v.try_reserve(usize::MAX).unwrap_err();
        let err = LegacyBufferReserveError::new(reserve_err);
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("LegacyBufferReserveError"));
    }

    #[test]
    fn legacy_buffer_reserve_error_display() {
        let mut v: Vec<u8> = Vec::new();
        let reserve_err = v.try_reserve(usize::MAX).unwrap_err();
        let err = LegacyBufferReserveError::new(reserve_err);
        let display_str = format!("{err}");
        assert!(display_str.contains("failed to reserve memory"));
    }

    #[test]
    fn map_reserve_error_for_io_returns_out_of_memory() {
        let mut v: Vec<u8> = Vec::new();
        let reserve_err = v.try_reserve(usize::MAX).unwrap_err();
        let io_err = map_reserve_error_for_io(reserve_err);
        assert_eq!(io_err.kind(), io::ErrorKind::OutOfMemory);
    }
}
