use std::io::{self, Read};

use memchr::memchr;

use oc_rsync_protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonGreeting, LegacyDaemonMessage, NegotiationPrologue,
    ProtocolVersion, parse_legacy_daemon_greeting_bytes,
    parse_legacy_daemon_greeting_bytes_details, parse_legacy_daemon_message_bytes,
    parse_legacy_error_message_bytes, parse_legacy_warning_message_bytes,
};

use super::super::map_line_reserve_error_for_io;
use super::base::NegotiatedStream;

impl<R: Read> NegotiatedStream<R> {
    /// Reads the legacy daemon greeting line after the negotiation prefix has been sniffed.
    ///
    /// The method mirrors [`oc_rsync_protocol::read_legacy_daemon_line`] but operates on the
    /// replaying stream wrapper instead of a [`oc_rsync_protocol::NegotiationPrologueSniffer`]. It expects the
    /// negotiation to have been classified as legacy ASCII and the canonical `@RSYNCD:` prefix
    /// to remain fully buffered. Consuming any of the replay bytes before invoking the helper
    /// results in an [`io::ErrorKind::InvalidInput`] error so higher layers cannot accidentally
    /// replay a partial prefix. The captured line (including the terminating newline) is written
    /// into `line`, which is cleared before new data is appended.
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::InvalidInput`] if the negotiation is not legacy ASCII, if the prefix is
    ///   incomplete, or if buffered bytes were consumed prior to calling the method.
    /// - [`io::ErrorKind::UnexpectedEof`] if the underlying stream closes before a newline is
    ///   observed.
    /// - [`io::ErrorKind::OutOfMemory`] when reserving space for the output buffer fails.
    #[doc(alias = "@RSYNCD")]
    pub fn read_legacy_daemon_line(&mut self, line: &mut Vec<u8>) -> io::Result<()> {
        self.read_legacy_line(line, true)
    }

    /// Reads and parses the legacy daemon greeting using the replaying stream wrapper.
    ///
    /// The helper forwards to [`Self::read_legacy_daemon_line`] before delegating to
    /// [`oc_rsync_protocol::parse_legacy_daemon_greeting_bytes`]. On success the negotiated
    /// [`ProtocolVersion`] is returned while leaving any bytes after the newline buffered for
    /// subsequent reads.
    #[doc(alias = "@RSYNCD")]
    pub fn read_and_parse_legacy_daemon_greeting(
        &mut self,
        line: &mut Vec<u8>,
    ) -> io::Result<ProtocolVersion> {
        self.read_legacy_daemon_line(line)?;
        parse_legacy_daemon_greeting_bytes(line).map_err(io::Error::from)
    }

    /// Reads and parses the legacy daemon greeting, returning the detailed representation.
    ///
    /// This mirrors [`Self::read_and_parse_legacy_daemon_greeting`] but exposes the structured
    /// [`LegacyDaemonGreeting`] used by higher layers to inspect the advertised protocol number,
    /// subprotocol, and digest list.
    #[doc(alias = "@RSYNCD")]
    pub fn read_and_parse_legacy_daemon_greeting_details<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<LegacyDaemonGreeting<'a>> {
        self.read_legacy_daemon_line(line)?;
        parse_legacy_daemon_greeting_bytes_details(line).map_err(io::Error::from)
    }

    /// Reads and parses a legacy daemon control message such as `@RSYNCD: OK` or `@RSYNCD: AUTHREQD`.
    ///
    /// The helper mirrors [`oc_rsync_protocol::parse_legacy_daemon_message_bytes`] but operates on the
    /// replaying transport wrapper so callers can continue using [`Read`] after the buffered
    /// negotiation prefix has been replayed. The returned [`LegacyDaemonMessage`] borrows the
    /// supplied buffer, matching the lifetime semantics of the parser from the protocol crate.
    #[doc(alias = "@RSYNCD")]
    pub fn read_and_parse_legacy_daemon_message<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<LegacyDaemonMessage<'a>> {
        self.read_legacy_line(line, false)?;
        parse_legacy_daemon_message_bytes(line).map_err(io::Error::from)
    }

    /// Reads and parses a legacy daemon error line of the form `@ERROR: ...`.
    ///
    /// Empty payloads are returned as `Some("")`, mirroring the behaviour of
    /// [`oc_rsync_protocol::parse_legacy_error_message_bytes`]. Any parsing failure is converted into
    /// [`io::ErrorKind::InvalidData`], matching the conversion performed by the protocol crate.
    #[doc(alias = "@ERROR")]
    pub fn read_and_parse_legacy_daemon_error_message<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<Option<&'a str>> {
        self.read_legacy_line(line, false)?;
        parse_legacy_error_message_bytes(line).map_err(io::Error::from)
    }

    /// Reads and parses a legacy daemon warning line of the form `@WARNING: ...`.
    #[doc(alias = "@WARNING")]
    pub fn read_and_parse_legacy_daemon_warning_message<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<Option<&'a str>> {
        self.read_legacy_line(line, false)?;
        parse_legacy_warning_message_bytes(line).map_err(io::Error::from)
    }
}

impl<R: Read> NegotiatedStream<R> {
    fn read_legacy_line(
        &mut self,
        line: &mut Vec<u8>,
        require_full_prefix: bool,
    ) -> io::Result<()> {
        line.clear();

        match self.decision() {
            NegotiationPrologue::LegacyAscii => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation has not been detected",
                ));
            }
        }

        let prefix_len = self.sniffed_prefix_len();
        let legacy_prefix_complete = self.legacy_prefix_complete();
        let remaining = self.sniffed_prefix_remaining();
        if require_full_prefix {
            if !legacy_prefix_complete {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation prefix is incomplete",
                ));
            }

            if remaining != LEGACY_DAEMON_PREFIX_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation prefix has already been consumed",
                ));
            }
        } else if legacy_prefix_complete && remaining != 0 && remaining != prefix_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "legacy negotiation prefix has been partially consumed",
            ));
        }

        self.populate_line_from_buffer(line)
    }

    fn populate_line_from_buffer(&mut self, line: &mut Vec<u8>) -> io::Result<()> {
        while self.buffered_remaining() > 0 {
            let consumed;
            let newline_hit = {
                let remaining = self.buffered_remaining_slice();

                if let Some(newline_index) = memchr(b'\n', remaining) {
                    consumed = newline_index + 1;
                    line.try_reserve(consumed)
                        .map_err(map_line_reserve_error_for_io)?;
                    line.extend_from_slice(&remaining[..consumed]);
                    true
                } else {
                    consumed = remaining.len();
                    line.try_reserve(consumed)
                        .map_err(map_line_reserve_error_for_io)?;
                    line.extend_from_slice(remaining);
                    false
                }
            };

            self.consume_buffered(consumed);

            if newline_hit {
                return Ok(());
            }
        }

        let mut byte = [0u8; 1];
        loop {
            match self.inner_mut().read(&mut byte) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "EOF while reading legacy rsync daemon line",
                    ));
                }
                Ok(read) => {
                    let observed = &byte[..read];
                    line.try_reserve(observed.len())
                        .map_err(map_line_reserve_error_for_io)?;
                    line.extend_from_slice(observed);
                    if memchr(b'\n', observed).is_some() {
                        return Ok(());
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }
}
