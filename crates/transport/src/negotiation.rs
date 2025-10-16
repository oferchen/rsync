use std::io::{self, BufRead, Read};

use rsync_protocol::{LEGACY_DAEMON_PREFIX_LEN, NegotiationPrologue, NegotiationPrologueSniffer};

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

    /// Returns the length of the sniffed negotiation prefix.
    #[must_use]
    pub const fn sniffed_prefix_len(&self) -> usize {
        self.buffer.sniffed_prefix_len()
    }

    /// Returns the total number of buffered bytes staged for replay.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.buffered_len()
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

    const fn sniffed_prefix_len(&self) -> usize {
        self.sniffed_prefix_len
    }

    fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    fn buffered_remaining(&self) -> usize {
        self.buffered.len().saturating_sub(self.buffered_pos)
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
pub fn sniff_negotiation_stream<R: Read>(mut reader: R) -> io::Result<NegotiatedStream<R>> {
    let mut sniffer = NegotiationPrologueSniffer::new();
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{self, BufRead, Cursor, Read};

    fn sniff_bytes(data: &[u8]) -> io::Result<NegotiatedStream<Cursor<Vec<u8>>>> {
        let cursor = Cursor::new(data.to_vec());
        sniff_negotiation_stream(cursor)
    }

    #[test]
    fn sniff_negotiation_detects_binary_prefix() {
        let mut stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
        assert_eq!(stream.sniffed_prefix(), &[0x00]);
        assert!(stream.buffered_remainder().is_empty());

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
        assert!(stream.buffered_remainder().is_empty());

        let mut replay = Vec::new();
        stream
            .read_to_end(&mut replay)
            .expect("read_to_end succeeds");
        assert_eq!(replay, legacy);
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
    fn parts_structure_exposes_buffered_state() {
        let data = b"\x00more";
        let stream = sniff_bytes(data).expect("sniff succeeds");
        let parts = stream.into_parts();
        assert_eq!(parts.decision(), NegotiationPrologue::Binary);
        assert_eq!(parts.sniffed_prefix(), b"\x00");
        assert!(parts.buffered_remainder().is_empty());
        assert_eq!(parts.buffered_remaining(), parts.sniffed_prefix_len());
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

        let mut rehydrated = NegotiatedStream::from_parts(parts);
        assert_eq!(
            rehydrated.buffered_remaining(),
            LEGACY_DAEMON_PREFIX_LEN - prefix_chunk.len()
        );

        let mut remainder = Vec::new();
        rehydrated
            .read_to_end(&mut remainder)
            .expect("reconstructed stream yields the remaining bytes");
        assert_eq!(remainder, b"NCD: 29.0\nrest");
    }
}
