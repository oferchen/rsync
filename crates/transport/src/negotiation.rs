use std::collections::TryReserveError;
use std::fmt;
use std::io::{self, BufRead, IoSliceMut, Read};

use rsync_protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonGreeting, LegacyDaemonMessage, NegotiationPrologue,
    NegotiationPrologueSniffer, ProtocolVersion, parse_legacy_daemon_greeting_bytes,
    parse_legacy_daemon_greeting_bytes_details, parse_legacy_daemon_message_bytes,
    parse_legacy_error_message_bytes, parse_legacy_warning_message_bytes,
};

/// Result produced when sniffing the negotiation prologue from a transport stream.
///
/// The structure owns the underlying reader together with the bytes that were
/// consumed while determining whether the peer speaks the legacy ASCII
/// `@RSYNCD:` protocol or the binary negotiation introduced in protocol 30. The
/// buffered data is replayed before any further reads from the inner stream,
/// mirroring upstream rsync's behavior where the detection prefix is fed back
/// into the parsing logic.
#[derive(Debug)]
pub struct NegotiatedStream<R> {
    inner: R,
    decision: NegotiationPrologue,
    buffer: NegotiationBuffer,
}

#[derive(Debug)]
struct NegotiationBuffer {
    sniffed_prefix_len: usize,
    buffered_pos: usize,
    buffered: Vec<u8>,
}

impl<R> NegotiatedStream<R> {
    /// Returns the negotiation style determined while sniffing the transport.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        self.decision
    }

    /// Returns the bytes that were required to classify the negotiation prologue.
    #[must_use]
    pub fn sniffed_prefix(&self) -> &[u8] {
        self.buffer.sniffed_prefix()
    }

    /// Returns the bytes buffered beyond the sniffed negotiation prefix.
    #[must_use]
    pub fn buffered_remainder(&self) -> &[u8] {
        self.buffer.buffered_remainder()
    }

    /// Returns the bytes captured during negotiation sniffing, including the prefix and remainder.
    #[must_use]
    pub fn buffered(&self) -> &[u8] {
        self.buffer.buffered()
    }

    /// Returns the sniffed negotiation prefix together with any buffered remainder.
    ///
    /// The tuple mirrors the view exposed by [`NegotiationPrologueSniffer::buffered_split`],
    /// allowing higher layers to borrow both slices simultaneously when staging replay
    /// buffers. The first element corresponds to the canonical prefix that must be replayed
    /// before continuing the handshake, while the second slice exposes any additional payload
    /// that arrived alongside the detection bytes.
    #[must_use]
    pub fn buffered_split(&self) -> (&[u8], &[u8]) {
        self.buffer.buffered_split()
    }

    /// Returns the total number of buffered bytes staged for replay.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.buffered_len()
    }

    /// Returns the length of the sniffed negotiation prefix.
    #[must_use]
    pub const fn sniffed_prefix_len(&self) -> usize {
        self.buffer.sniffed_prefix_len()
    }

    /// Returns the remaining number of buffered bytes that have not yet been read.
    #[must_use]
    pub fn buffered_remaining(&self) -> usize {
        self.buffer.buffered_remaining()
    }

    /// Releases the wrapper and returns its components.
    #[must_use]
    pub fn into_parts(self) -> NegotiatedStreamParts<R> {
        NegotiatedStreamParts {
            decision: self.decision,
            buffer: self.buffer,
            inner: self.inner,
        }
    }

    /// Returns a shared reference to the inner reader.
    #[must_use]
    pub const fn inner(&self) -> &R {
        &self.inner
    }

    /// Returns a mutable reference to the inner reader.
    #[must_use]
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Releases the wrapper and returns the inner reader.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Transforms the inner reader while preserving the buffered negotiation state.
    ///
    /// The helper allows callers to wrap the underlying transport (for example to
    /// install additional instrumentation or apply timeout adapters) without
    /// losing the bytes that were already sniffed during negotiation detection.
    /// The replay cursor remains unchanged so subsequent reads continue exactly
    /// where the caller left off. The mapping closure is responsible for
    /// carrying over any relevant state on the inner reader (such as read
    /// positions) before returning the replacement value.
    #[must_use]
    pub fn map_inner<F, T>(self, map: F) -> NegotiatedStream<T>
    where
        F: FnOnce(R) -> T,
    {
        self.into_parts().map_inner(map).into_stream()
    }

    fn from_raw_components(
        inner: R,
        decision: NegotiationPrologue,
        sniffed_prefix_len: usize,
        buffered_pos: usize,
        buffered: Vec<u8>,
    ) -> Self {
        Self {
            inner,
            decision,
            buffer: NegotiationBuffer::new(sniffed_prefix_len, buffered_pos, buffered),
        }
    }

    fn from_buffer(inner: R, decision: NegotiationPrologue, buffer: NegotiationBuffer) -> Self {
        Self {
            inner,
            decision,
            buffer,
        }
    }

    /// Reconstructs a [`NegotiatedStream`] from its previously extracted parts.
    ///
    /// The helper restores the buffered read position so consumers that staged
    /// the stream's state for inspection or temporary ownership changes can
    /// resume reading without replaying bytes that were already delivered. The
    /// clamped invariants mirror the construction performed during negotiation
    /// sniffing to guarantee the prefix length never exceeds the buffered
    /// payload.
    #[must_use]
    pub fn from_parts(parts: NegotiatedStreamParts<R>) -> Self {
        let (decision, buffer, inner) = parts.into_components();
        Self::from_buffer(inner, decision, buffer)
    }
}

impl<R: Read> NegotiatedStream<R> {
    /// Reads the legacy daemon greeting line after the negotiation prefix has been sniffed.
    ///
    /// The method mirrors [`rsync_protocol::read_legacy_daemon_line`] but operates on the
    /// replaying stream wrapper instead of a [`NegotiationPrologueSniffer`]. It expects the
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
    pub fn read_legacy_daemon_line(&mut self, line: &mut Vec<u8>) -> io::Result<()> {
        self.read_legacy_line(line, true)
    }

    /// Reads and parses the legacy daemon greeting using the replaying stream wrapper.
    ///
    /// The helper forwards to [`Self::read_legacy_daemon_line`] before delegating to
    /// [`rsync_protocol::parse_legacy_daemon_greeting_bytes`]. On success the negotiated
    /// [`ProtocolVersion`] is returned while leaving any bytes after the newline buffered for
    /// subsequent reads.
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
    pub fn read_and_parse_legacy_daemon_greeting_details<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<LegacyDaemonGreeting<'a>> {
        self.read_legacy_daemon_line(line)?;
        parse_legacy_daemon_greeting_bytes_details(line).map_err(io::Error::from)
    }

    /// Reads and parses a legacy daemon control message such as `@RSYNCD: OK` or `@RSYNCD: AUTHREQD`.
    ///
    /// The helper mirrors [`rsync_protocol::parse_legacy_daemon_message_bytes`] but operates on the
    /// replaying transport wrapper so callers can continue using [`Read`] after the buffered
    /// negotiation prefix has been replayed. The returned [`LegacyDaemonMessage`] borrows the
    /// supplied buffer, matching the lifetime semantics of the parser from the protocol crate.
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
    /// [`rsync_protocol::parse_legacy_error_message_bytes`]. Any parsing failure is converted into
    /// [`io::ErrorKind::InvalidData`], matching the conversion performed by the protocol crate.
    pub fn read_and_parse_legacy_daemon_error_message<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<Option<&'a str>> {
        self.read_legacy_line(line, false)?;
        parse_legacy_error_message_bytes(line).map_err(io::Error::from)
    }

    /// Reads and parses a legacy daemon warning line of the form `@WARNING: ...`.
    pub fn read_and_parse_legacy_daemon_warning_message<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<Option<&'a str>> {
        self.read_legacy_line(line, false)?;
        parse_legacy_warning_message_bytes(line).map_err(io::Error::from)
    }
}

impl<R: Read> NegotiatedStream<R> {
    fn read_legacy_line(&mut self, line: &mut Vec<u8>, require_full_prefix: bool) -> io::Result<()> {
        match self.decision {
            NegotiationPrologue::LegacyAscii => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation has not been detected",
                ));
            }
        }

        if require_full_prefix {
            if !self.buffer.legacy_prefix_complete() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation prefix is incomplete",
                ));
            }

            if self.buffer.sniffed_prefix_remaining() != LEGACY_DAEMON_PREFIX_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation prefix has already been consumed",
                ));
            }
        } else {
            if self.buffer.sniffed_prefix_len() == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation prefix is incomplete",
                ));
            }

            if self.buffer.sniffed_prefix_remaining() != self.buffer.sniffed_prefix_len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation prefix has already been consumed",
                ));
            }
        }

        self.populate_line_from_buffer(line)
    }

    fn populate_line_from_buffer(&mut self, line: &mut Vec<u8>) -> io::Result<()> {
        line.clear();

        while self.buffer.has_remaining() {
            let remaining = self.buffer.remaining_slice();

            if let Some(newline_index) = remaining.iter().position(|&byte| byte == b'\n') {
                let to_copy = newline_index + 1;
                line.try_reserve(to_copy)
                    .map_err(map_line_reserve_error_for_io)?;
                line.extend_from_slice(&remaining[..to_copy]);
                self.buffer.consume(to_copy);
                return Ok(());
            }

            line.try_reserve(remaining.len())
                .map_err(map_line_reserve_error_for_io)?;
            line.extend_from_slice(remaining);
            let consumed = remaining.len();
            self.buffer.consume(consumed);
        }

        let mut byte = [0u8; 1];
        loop {
            match self.inner.read(&mut byte) {
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
                    if observed.iter().any(|&value| value == b'\n') {
                        return Ok(());
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

impl<R: Read> Read for NegotiatedStream<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let copied = self.buffer.copy_into(buf);
        if copied > 0 {
            return Ok(copied);
        }

        self.inner.read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        if bufs.is_empty() {
            return Ok(0);
        }

        let copied = self.buffer.copy_into_vectored(bufs);
        if copied > 0 {
            return Ok(copied);
        }

        self.inner.read_vectored(bufs)
    }
}

impl<R: BufRead> BufRead for NegotiatedStream<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.buffer.has_remaining() {
            return Ok(self.buffer.remaining_slice());
        }

        self.inner.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        let remainder = self.buffer.consume(amt);
        if remainder > 0 {
            BufRead::consume(&mut self.inner, remainder);
        }
    }
}

/// Components extracted from a [`NegotiatedStream`].
#[derive(Debug)]
pub struct NegotiatedStreamParts<R> {
    decision: NegotiationPrologue,
    buffer: NegotiationBuffer,
    inner: R,
}

impl<R> NegotiatedStreamParts<R> {
    /// Returns the negotiation style that was detected.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        self.decision
    }

    /// Returns the captured negotiation prefix.
    #[must_use]
    pub fn sniffed_prefix(&self) -> &[u8] {
        self.buffer.sniffed_prefix()
    }

    /// Returns the buffered remainder.
    #[must_use]
    pub fn buffered_remainder(&self) -> &[u8] {
        self.buffer.buffered_remainder()
    }

    /// Returns the buffered bytes captured during sniffing.
    #[must_use]
    pub fn buffered(&self) -> &[u8] {
        self.buffer.buffered()
    }

    /// Returns the sniffed negotiation prefix together with any buffered remainder.
    ///
    /// The tuple mirrors [`NegotiatedStream::buffered_split`], giving callers convenient access
    /// to both slices when rebuilding replay buffers without cloning the stored negotiation
    /// bytes.
    #[must_use]
    pub fn buffered_split(&self) -> (&[u8], &[u8]) {
        self.buffer.buffered_split()
    }

    /// Returns the total number of bytes captured during sniffing.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.buffered_len()
    }

    /// Returns how many buffered bytes remain unread.
    #[must_use]
    pub fn buffered_remaining(&self) -> usize {
        self.buffer.buffered_remaining()
    }

    /// Returns the length of the sniffed negotiation prefix.
    #[must_use]
    pub const fn sniffed_prefix_len(&self) -> usize {
        self.buffer.sniffed_prefix_len()
    }

    /// Returns the inner reader.
    #[must_use]
    pub const fn inner(&self) -> &R {
        &self.inner
    }

    /// Returns the inner reader mutably.
    #[must_use]
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Releases the parts structure and returns the inner reader.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Transforms the inner reader while keeping the sniffed negotiation state intact.
    ///
    /// This mirrors [`NegotiatedStream::map_inner`] but operates on the extracted
    /// parts, allowing the caller to temporarily take ownership of the inner
    /// reader, wrap it, and later rebuild the replaying stream without cloning
    /// the buffered negotiation bytes. The supplied mapping closure is expected
    /// to retain any pertinent state (for example, the current read position) on
    /// the replacement reader before it is returned.
    #[must_use]
    pub fn map_inner<F, T>(self, map: F) -> NegotiatedStreamParts<T>
    where
        F: FnOnce(R) -> T,
    {
        let Self {
            decision,
            buffer,
            inner,
        } = self;

        NegotiatedStreamParts {
            decision,
            buffer,
            inner: map(inner),
        }
    }

    /// Reassembles a [`NegotiatedStream`] from the extracted components.
    ///
    /// Callers can temporarily inspect or adjust the buffered negotiation
    /// state (for example, updating transport-level settings on the inner
    /// reader) and then continue consuming data through the replaying wrapper
    /// without cloning the sniffed bytes.
    #[must_use]
    pub fn into_stream(self) -> NegotiatedStream<R> {
        NegotiatedStream::from_buffer(self.inner, self.decision, self.buffer)
    }
}

impl NegotiationBuffer {
    fn new(sniffed_prefix_len: usize, buffered_pos: usize, buffered: Vec<u8>) -> Self {
        let clamped_prefix_len = sniffed_prefix_len.min(buffered.len());
        let clamped_pos = buffered_pos.min(buffered.len());

        Self {
            sniffed_prefix_len: clamped_prefix_len,
            buffered_pos: clamped_pos,
            buffered,
        }
    }

    fn sniffed_prefix(&self) -> &[u8] {
        &self.buffered[..self.sniffed_prefix_len]
    }

    fn buffered_remainder(&self) -> &[u8] {
        &self.buffered[self.sniffed_prefix_len..]
    }

    fn buffered(&self) -> &[u8] {
        &self.buffered
    }

    fn buffered_split(&self) -> (&[u8], &[u8]) {
        let prefix_len = self.sniffed_prefix_len();
        debug_assert!(prefix_len <= self.buffered.len());

        (&self.buffered[..prefix_len], &self.buffered[prefix_len..])
    }

    const fn sniffed_prefix_len(&self) -> usize {
        self.sniffed_prefix_len
    }

    fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    fn buffered_remaining(&self) -> usize {
        self.buffered.len().saturating_sub(self.buffered_pos)
    }

    fn sniffed_prefix_remaining(&self) -> usize {
        let consumed_prefix = self.buffered_pos.min(self.sniffed_prefix_len);
        self.sniffed_prefix_len.saturating_sub(consumed_prefix)
    }

    fn legacy_prefix_complete(&self) -> bool {
        self.sniffed_prefix_len >= LEGACY_DAEMON_PREFIX_LEN
    }

    fn has_remaining(&self) -> bool {
        self.buffered_pos < self.buffered.len()
    }

    fn remaining_slice(&self) -> &[u8] {
        &self.buffered[self.buffered_pos..]
    }

    fn copy_into(&mut self, buf: &mut [u8]) -> usize {
        if buf.is_empty() || !self.has_remaining() {
            return 0;
        }

        let available = &self.buffered[self.buffered_pos..];
        let to_copy = available.len().min(buf.len());
        buf[..to_copy].copy_from_slice(&available[..to_copy]);
        self.buffered_pos += to_copy;
        to_copy
    }

    fn copy_into_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> usize {
        if bufs.is_empty() || !self.has_remaining() {
            return 0;
        }

        let available = &self.buffered[self.buffered_pos..];
        let mut copied = 0;

        for buf in bufs.iter_mut() {
            if copied == available.len() {
                break;
            }

            let target = buf.as_mut();
            if target.is_empty() {
                continue;
            }

            let remaining = available.len() - copied;
            let to_copy = remaining.min(target.len());
            target[..to_copy].copy_from_slice(&available[copied..copied + to_copy]);
            copied += to_copy;
        }

        self.buffered_pos += copied;
        copied
    }

    fn consume(&mut self, amt: usize) -> usize {
        if !self.has_remaining() {
            return amt;
        }

        let available = self.buffered_remaining();
        if amt < available {
            self.buffered_pos += amt;
            0
        } else {
            self.buffered_pos = self.buffered.len();
            amt - available
        }
    }
}

impl<R> NegotiatedStreamParts<R> {
    fn into_components(self) -> (NegotiationPrologue, NegotiationBuffer, R) {
        let Self {
            decision,
            buffer,
            inner,
        } = self;
        (decision, buffer, inner)
    }
}

/// Sniffs the negotiation prologue from the provided reader.
///
/// The function mirrors upstream rsync's handshake detection logic: it peeks at
/// the first byte to distinguish binary negotiations from legacy ASCII
/// `@RSYNCD:` exchanges, buffering the observed data so the caller can replay it
/// when parsing the daemon greeting or continuing the binary protocol.
///
/// # Errors
///
/// Returns [`io::ErrorKind::UnexpectedEof`] if the stream ends before a
/// negotiation style can be determined or propagates any underlying I/O error
/// reported by the reader.
pub fn sniff_negotiation_stream<R: Read>(reader: R) -> io::Result<NegotiatedStream<R>> {
    let mut sniffer = NegotiationPrologueSniffer::new();
    sniff_negotiation_stream_with_sniffer(reader, &mut sniffer)
}

/// Sniffs the negotiation prologue using a caller supplied sniffer instance.
///
/// The helper mirrors [`sniff_negotiation_stream`] but reuses the provided
/// [`NegotiationPrologueSniffer`], avoiding temporary allocations when a
/// higher layer already maintains a pool of reusable sniffers. The sniffer is
/// reset to guarantee stale state from previous sessions is discarded before
/// the new transport is observed.
pub fn sniff_negotiation_stream_with_sniffer<R: Read>(
    mut reader: R,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<NegotiatedStream<R>> {
    sniffer.reset();

    let decision = sniffer.read_from(&mut reader)?;
    debug_assert_ne!(decision, NegotiationPrologue::NeedMoreData);

    let sniffed_prefix_len = sniffer.sniffed_prefix_len();
    let buffered = sniffer.take_buffered();

    debug_assert!(sniffed_prefix_len <= LEGACY_DAEMON_PREFIX_LEN);
    debug_assert!(sniffed_prefix_len <= buffered.len());

    Ok(NegotiatedStream::from_raw_components(
        reader,
        decision,
        sniffed_prefix_len,
        0,
        buffered,
    ))
}

#[derive(Debug)]
struct LegacyLineReserveError {
    inner: TryReserveError,
}

impl LegacyLineReserveError {
    fn new(inner: TryReserveError) -> Self {
        Self { inner }
    }
}

impl fmt::Display for LegacyLineReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to reserve memory for legacy negotiation buffer: {}",
            self.inner
        )
    }
}

impl std::error::Error for LegacyLineReserveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.inner)
    }
}

fn map_line_reserve_error_for_io(err: TryReserveError) -> io::Error {
    io::Error::new(io::ErrorKind::OutOfMemory, LegacyLineReserveError::new(err))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{self, BufRead, Cursor, IoSliceMut, Read};

    use rsync_protocol::ProtocolVersion;

    fn sniff_bytes(data: &[u8]) -> io::Result<NegotiatedStream<Cursor<Vec<u8>>>> {
        let cursor = Cursor::new(data.to_vec());
        sniff_negotiation_stream(cursor)
    }

    #[test]
    fn sniff_negotiation_detects_binary_prefix() {
        let mut stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
        assert_eq!(stream.sniffed_prefix(), &[0x00]);
        assert_eq!(stream.sniffed_prefix_len(), 1);
        assert_eq!(stream.buffered_len(), 1);
        assert!(stream.buffered_remainder().is_empty());
        let (prefix, remainder) = stream.buffered_split();
        assert_eq!(prefix, &[0x00]);
        assert!(remainder.is_empty());

        let mut buf = [0u8; 3];
        stream
            .read_exact(&mut buf)
            .expect("read_exact drains buffered prefix and remainder");
        assert_eq!(&buf, &[0x00, 0x12, 0x34]);

        let mut tail = [0u8; 2];
        let read = stream
            .read(&mut tail)
            .expect("read after buffer consumes inner");
        assert_eq!(read, 0);
        assert!(tail.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn sniff_negotiation_detects_legacy_prefix_and_preserves_remainder() {
        let legacy = b"@RSYNCD: 31.0\n#list";
        let mut stream = sniff_bytes(legacy).expect("sniff succeeds");
        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
        assert_eq!(stream.sniffed_prefix(), b"@RSYNCD:");
        assert_eq!(stream.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);
        assert_eq!(stream.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);
        assert!(stream.buffered_remainder().is_empty());
        let (prefix, remainder) = stream.buffered_split();
        assert_eq!(prefix, b"@RSYNCD:");
        assert!(remainder.is_empty());

        let mut replay = Vec::new();
        stream
            .read_to_end(&mut replay)
            .expect("read_to_end succeeds");
        assert_eq!(replay, legacy);
    }

    #[test]
    fn sniff_negotiation_with_supplied_sniffer_reuses_internal_buffer() {
        let mut sniffer = NegotiationPrologueSniffer::new();

        {
            let mut stream = sniff_negotiation_stream_with_sniffer(
                Cursor::new(b"@RSYNCD: 31.0\nrest".to_vec()),
                &mut sniffer,
            )
            .expect("sniff succeeds");

            assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
            assert_eq!(stream.sniffed_prefix(), b"@RSYNCD:");

            let mut replay = Vec::new();
            stream
                .read_to_end(&mut replay)
                .expect("replay reads all bytes");
            assert_eq!(replay, b"@RSYNCD: 31.0\nrest");
        }

        assert_eq!(sniffer.buffered_len(), 0);
        assert_eq!(sniffer.sniffed_prefix_len(), 0);

        {
            let mut stream = sniff_negotiation_stream_with_sniffer(
                Cursor::new(vec![0x00, 0x12, 0x34, 0x56]),
                &mut sniffer,
            )
            .expect("sniff succeeds");

            assert_eq!(stream.decision(), NegotiationPrologue::Binary);
            assert_eq!(stream.sniffed_prefix(), &[0x00]);

            let mut replay = Vec::new();
            stream
                .read_to_end(&mut replay)
                .expect("binary replay drains reader");
            assert_eq!(replay, &[0x00, 0x12, 0x34, 0x56]);
        }

        assert_eq!(sniffer.buffered_len(), 0);
        assert_eq!(sniffer.sniffed_prefix_len(), 0);
    }

    #[test]
    fn sniff_negotiation_buffered_split_exposes_prefix_and_remainder() {
        let cursor = Cursor::new(Vec::<u8>::new());
        let mut stream = NegotiatedStream::from_raw_components(
            cursor,
            NegotiationPrologue::Binary,
            1,
            0,
            vec![0x00, b'a', b'b', b'c'],
        );

        let (prefix, remainder) = stream.buffered_split();
        assert_eq!(prefix, &[0x00]);
        assert_eq!(remainder, b"abc");

        // Partially consume the prefix to ensure the tuple remains stable.
        let mut buf = [0u8; 1];
        stream
            .read_exact(&mut buf)
            .expect("read_exact consumes the buffered prefix");
        assert_eq!(buf, [0x00]);

        let (after_read_prefix, after_read_remainder) = stream.buffered_split();
        assert_eq!(after_read_prefix, &[0x00]);
        assert_eq!(after_read_remainder, b"abc");
    }

    #[test]
    fn sniff_negotiation_errors_on_empty_stream() {
        let err = sniff_bytes(&[]).expect_err("sniff should fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn sniffed_stream_supports_bufread_semantics() {
        let data = b"@RSYNCD: 32.0\nhello";
        let mut stream = sniff_bytes(data).expect("sniff succeeds");

        assert_eq!(stream.fill_buf().expect("fill_buf succeeds"), b"@RSYNCD:");
        stream.consume(LEGACY_DAEMON_PREFIX_LEN);
        assert_eq!(
            stream.fill_buf().expect("fill_buf after consume succeeds"),
            b" 32.0\nhello"
        );
        stream.consume(3);
        assert_eq!(
            stream.fill_buf().expect("fill_buf after partial consume"),
            b".0\nhello"
        );
    }

    #[test]
    fn sniffed_stream_supports_vectored_reads_from_buffer() {
        let data = b"@RSYNCD: 31.0\nrest";
        let mut stream = sniff_bytes(data).expect("sniff succeeds");

        let mut head = [0u8; 4];
        let mut tail = [0u8; 8];
        let mut bufs = [IoSliceMut::new(&mut head), IoSliceMut::new(&mut tail)];

        let read = stream
            .read_vectored(&mut bufs)
            .expect("vectored read drains buffered prefix");
        assert_eq!(read, LEGACY_DAEMON_PREFIX_LEN);
        assert_eq!(&head, b"@RSY");

        let tail_prefix = read.saturating_sub(head.len());
        assert_eq!(&tail[..tail_prefix], b"NCD:");

        let mut remainder = Vec::new();
        stream
            .read_to_end(&mut remainder)
            .expect("remaining bytes are readable");
        assert_eq!(remainder, &data[read..]);
    }

    #[test]
    fn vectored_reads_delegate_to_inner_after_buffer_is_drained() {
        let data = b"\x00rest";
        let mut stream = sniff_bytes(data).expect("sniff succeeds");

        let mut prefix_buf = [0u8; 1];
        let mut bufs = [IoSliceMut::new(&mut prefix_buf)];
        let read = stream
            .read_vectored(&mut bufs)
            .expect("vectored read captures sniffed prefix");
        assert_eq!(read, 1);
        assert_eq!(prefix_buf, [0x00]);

        let mut first = [0u8; 2];
        let mut second = [0u8; 8];
        let mut remainder_bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
        let remainder_read = stream
            .read_vectored(&mut remainder_bufs)
            .expect("vectored read forwards to inner reader");

        let mut remainder = Vec::new();
        remainder.extend_from_slice(&first[..first.len().min(remainder_read)]);
        if remainder_read > first.len() {
            let extra = (remainder_read - first.len()).min(second.len());
            remainder.extend_from_slice(&second[..extra]);
        }
        if remainder.len() < b"rest".len() {
            let mut tail = Vec::new();
            stream
                .read_to_end(&mut tail)
                .expect("consume any bytes left by the default vectored implementation");
            remainder.extend_from_slice(&tail);
        }
        assert_eq!(remainder, b"rest");
    }

    #[test]
    fn vectored_reads_delegate_to_inner_even_without_specialized_support() {
        let data = b"\x00rest".to_vec();
        let mut stream =
            sniff_negotiation_stream(NonVectoredCursor::new(data)).expect("sniff succeeds");

        let mut prefix_buf = [0u8; 1];
        let mut prefix_vecs = [IoSliceMut::new(&mut prefix_buf)];
        let read = stream
            .read_vectored(&mut prefix_vecs)
            .expect("vectored read yields buffered prefix");
        assert_eq!(read, 1);
        assert_eq!(prefix_buf, [0x00]);

        let mut first = [0u8; 2];
        let mut second = [0u8; 8];
        let mut remainder_bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
        let remainder_read = stream
            .read_vectored(&mut remainder_bufs)
            .expect("vectored read falls back to inner read implementation");

        let mut remainder = Vec::new();
        remainder.extend_from_slice(&first[..first.len().min(remainder_read)]);
        if remainder_read > first.len() {
            let extra = (remainder_read - first.len()).min(second.len());
            remainder.extend_from_slice(&second[..extra]);
        }
        let mut tail = Vec::new();
        stream
            .read_to_end(&mut tail)
            .expect("consume any bytes left by the default vectored implementation");
        remainder.extend_from_slice(&tail);

        assert_eq!(remainder, b"rest");
    }

    #[test]
    fn parts_structure_exposes_buffered_state() {
        let data = b"\x00more";
        let stream = sniff_bytes(data).expect("sniff succeeds");
        let parts = stream.into_parts();
        assert_eq!(parts.decision(), NegotiationPrologue::Binary);
        assert_eq!(parts.sniffed_prefix(), b"\x00");
        assert!(parts.buffered_remainder().is_empty());
        assert_eq!(parts.buffered_len(), parts.sniffed_prefix_len());
        assert_eq!(parts.buffered_remaining(), parts.sniffed_prefix_len());
        let (prefix, remainder) = parts.buffered_split();
        assert_eq!(prefix, b"\x00");
        assert!(remainder.is_empty());
    }

    #[test]
    fn parts_can_be_rehydrated_without_rewinding_consumed_bytes() {
        let data = b"@RSYNCD: 29.0\nrest";
        let mut stream = sniff_bytes(data).expect("sniff succeeds");

        let mut prefix_chunk = [0u8; 4];
        stream
            .read_exact(&mut prefix_chunk)
            .expect("read_exact consumes part of the buffered prefix");
        assert_eq!(&prefix_chunk, b"@RSY");

        let parts = stream.into_parts();
        assert_eq!(
            parts.buffered_remaining(),
            LEGACY_DAEMON_PREFIX_LEN - prefix_chunk.len()
        );
        assert_eq!(parts.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);

        let mut rehydrated = NegotiatedStream::from_parts(parts);
        assert_eq!(
            rehydrated.buffered_remaining(),
            LEGACY_DAEMON_PREFIX_LEN - prefix_chunk.len()
        );

        let (rehydrated_prefix, rehydrated_remainder) = rehydrated.buffered_split();
        assert_eq!(rehydrated_prefix, b"@RSYNCD:");
        assert_eq!(rehydrated_remainder, rehydrated.buffered_remainder());

        let mut remainder = Vec::new();
        rehydrated
            .read_to_end(&mut remainder)
            .expect("reconstructed stream yields the remaining bytes");
        assert_eq!(remainder, b"NCD: 29.0\nrest");
    }

    #[test]
    fn map_inner_preserves_buffered_progress() {
        let mut stream = sniff_bytes(&[0x00, 0x12, 0x34, 0x56]).expect("sniff succeeds");
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);

        let mut prefix = [0u8; 1];
        stream
            .read_exact(&mut prefix)
            .expect("read_exact delivers sniffed prefix");
        assert_eq!(prefix, [0x00]);

        let mut mapped = stream.map_inner(|cursor| {
            let position = cursor.position();
            let boxed = cursor.into_inner().into_boxed_slice();
            let mut replacement = Cursor::new(boxed);
            replacement.set_position(position);
            replacement
        });
        assert_eq!(mapped.decision(), NegotiationPrologue::Binary);

        let mut remainder = [0u8; 3];
        mapped
            .read_exact(&mut remainder)
            .expect("replay continues from buffered position");
        assert_eq!(&remainder, &[0x12, 0x34, 0x56]);
    }

    #[test]
    fn parts_map_inner_allows_rewrapping_inner_reader() {
        let data = b"@RSYNCD: 31.0\n#list";
        let parts = sniff_bytes(data).expect("sniff succeeds").into_parts();
        let remaining = parts.buffered_remaining();

        let mapped_parts = parts.map_inner(|cursor| {
            let position = cursor.position();
            let boxed = cursor.into_inner().into_boxed_slice();
            let mut replacement = Cursor::new(boxed);
            replacement.set_position(position);
            replacement
        });
        assert_eq!(mapped_parts.decision(), NegotiationPrologue::LegacyAscii);
        assert_eq!(mapped_parts.buffered_remaining(), remaining);
        let (prefix, remainder) = mapped_parts.buffered_split();
        assert_eq!(prefix, b"@RSYNCD:");
        assert_eq!(remainder, mapped_parts.buffered_remainder());

        let mut rebuilt = mapped_parts.into_stream();
        let mut replay = Vec::new();
        rebuilt
            .read_to_end(&mut replay)
            .expect("rebuilt stream yields original contents");
        assert_eq!(replay, data);
    }

    #[test]
    fn read_legacy_daemon_line_replays_buffered_prefix() {
        let mut stream = sniff_bytes(b"@RSYNCD: 30.0\n#list\n").expect("sniff succeeds");
        let mut line = Vec::new();
        stream
            .read_legacy_daemon_line(&mut line)
            .expect("legacy line is read");
        assert_eq!(line, b"@RSYNCD: 30.0\n");

        let mut remainder = Vec::new();
        stream
            .read_to_end(&mut remainder)
            .expect("remaining bytes are replayed");
        assert_eq!(remainder, b"#list\n");
    }

    #[test]
    fn read_and_parse_legacy_daemon_message_routes_keywords() {
        let mut stream = sniff_bytes(b"@RSYNCD: AUTHREQD module\n").expect("sniff succeeds");
        let mut line = Vec::new();
        match stream
            .read_and_parse_legacy_daemon_message(&mut line)
            .expect("message parses")
        {
            LegacyDaemonMessage::AuthRequired { module } => {
                assert_eq!(module, Some("module"));
            }
            other => panic!("unexpected message: {other:?}"),
        }
        assert_eq!(line, b"@RSYNCD: AUTHREQD module\n");
    }

    #[test]
    fn read_and_parse_legacy_daemon_message_routes_versions() {
        let mut stream = sniff_bytes(b"@RSYNCD: 29.0\n").expect("sniff succeeds");
        let mut line = Vec::new();
        match stream
            .read_and_parse_legacy_daemon_message(&mut line)
            .expect("message parses")
        {
            LegacyDaemonMessage::Version(version) => {
                let expected = ProtocolVersion::from_supported(29).expect("supported version");
                assert_eq!(version, expected);
            }
            other => panic!("unexpected message: {other:?}"),
        }
        assert_eq!(line, b"@RSYNCD: 29.0\n");
    }

    #[test]
    fn read_and_parse_legacy_daemon_message_propagates_parse_errors() {
        let mut stream = sniff_bytes(b"@RSYNCD:\n").expect("sniff succeeds");
        let mut line = Vec::new();
        let err = stream
            .read_and_parse_legacy_daemon_message(&mut line)
            .expect_err("message parsing should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_and_parse_legacy_daemon_error_message_returns_payload() {
        let mut stream = sniff_bytes(b"@ERROR: something went wrong\n").expect("sniff succeeds");
        let mut line = Vec::new();
        {
            let payload = stream
                .read_and_parse_legacy_daemon_error_message(&mut line)
                .expect("error payload parses")
                .expect("payload is present");
            assert_eq!(payload, "something went wrong");
        }
        assert_eq!(line, b"@ERROR: something went wrong\n");
    }

    #[test]
    fn read_and_parse_legacy_daemon_error_message_allows_empty_payloads() {
        let mut stream = sniff_bytes(b"@ERROR:\n").expect("sniff succeeds");
        let mut line = Vec::new();
        {
            let payload = stream
                .read_and_parse_legacy_daemon_error_message(&mut line)
                .expect("empty payload parses");
            assert_eq!(payload, Some(""));
        }
        assert_eq!(line, b"@ERROR:\n");
    }

    #[test]
    fn read_and_parse_legacy_daemon_warning_message_returns_payload() {
        let mut stream = sniff_bytes(b"@WARNING: check perms\n").expect("sniff succeeds");
        let mut line = Vec::new();
        {
            let payload = stream
                .read_and_parse_legacy_daemon_warning_message(&mut line)
                .expect("warning payload parses")
                .expect("payload is present");
            assert_eq!(payload, "check perms");
        }
        assert_eq!(line, b"@WARNING: check perms\n");
    }

    #[test]
    fn read_legacy_daemon_line_errors_when_prefix_already_consumed() {
        let mut stream = sniff_bytes(b"@RSYNCD: 29.0\nrest").expect("sniff succeeds");
        let mut prefix_chunk = [0u8; 4];
        stream
            .read_exact(&mut prefix_chunk)
            .expect("prefix chunk is replayed before parsing");

        let mut line = Vec::new();
        let err = stream
            .read_legacy_daemon_line(&mut line)
            .expect_err("consuming prefix first should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn read_legacy_daemon_line_errors_for_binary_negotiation() {
        let mut stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
        let mut line = Vec::new();
        let err = stream
            .read_legacy_daemon_line(&mut line)
            .expect_err("binary negotiations do not yield legacy lines");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn read_legacy_daemon_line_errors_on_eof_before_newline() {
        let mut stream = sniff_bytes(b"@RSYNCD:").expect("sniff succeeds");
        let mut line = Vec::new();
        let err = stream
            .read_legacy_daemon_line(&mut line)
            .expect_err("EOF before newline must error");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_and_parse_legacy_daemon_greeting_from_stream() {
        let mut stream = sniff_bytes(b"@RSYNCD: 31.0\n").expect("sniff succeeds");
        let mut line = Vec::new();
        let version = stream
            .read_and_parse_legacy_daemon_greeting(&mut line)
            .expect("greeting parses");
        assert_eq!(version, ProtocolVersion::from_supported(31).unwrap());
        assert_eq!(line, b"@RSYNCD: 31.0\n");
    }

    #[test]
    fn read_and_parse_legacy_daemon_greeting_details_from_stream() {
        let mut stream = sniff_bytes(b"@RSYNCD: 31.0 md4 md5\n").expect("sniff succeeds");
        let mut line = Vec::new();
        let details = stream
            .read_and_parse_legacy_daemon_greeting_details(&mut line)
            .expect("detailed greeting parses");
        assert_eq!(
            details.protocol(),
            ProtocolVersion::from_supported(31).unwrap()
        );
        assert_eq!(details.digest_list(), Some("md4 md5"));
        assert!(details.has_subprotocol());
        assert_eq!(line, b"@RSYNCD: 31.0 md4 md5\n");
    }

    #[derive(Debug)]
    struct NonVectoredCursor(Cursor<Vec<u8>>);

    impl NonVectoredCursor {
        fn new(bytes: Vec<u8>) -> Self {
            Self(Cursor::new(bytes))
        }
    }

    impl Read for NonVectoredCursor {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }
}
