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

#[derive(Debug)]
struct LegacyBufferReserveError {
    inner: TryReserveError,
}

impl LegacyBufferReserveError {
    fn new(inner: TryReserveError) -> Self {
        Self { inner }
    }

    fn inner(&self) -> &TryReserveError {
        &self.inner
    }
}

impl std::fmt::Display for LegacyBufferReserveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "failed to reserve memory for legacy negotiation buffer: {}",
            self.inner
        )
    }
}

impl std::error::Error for LegacyBufferReserveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.inner())
    }
}

pub(crate) fn map_reserve_error_for_io(err: TryReserveError) -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        LegacyBufferReserveError::new(err),
    )
}
