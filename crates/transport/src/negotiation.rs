use std::collections::TryReserveError;
use std::fmt;
use std::io::{self, BufRead, IoSlice, IoSliceMut, Read, Write};

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

#[derive(Clone, Debug)]
struct NegotiationBuffer {
    sniffed_prefix_len: usize,
    buffered_pos: usize,
    buffered: Vec<u8>,
}

/// Error returned when a caller-provided buffer is too small to hold the sniffed bytes.
///
/// The structure reports how many bytes were required to copy the replay data and how many were
/// provided by the caller. It mirrors upstream rsync's approach of signalling insufficient
/// capacity without mutating the destination, allowing higher layers to retry with a suitably
/// sized buffer while keeping the captured negotiation prefix intact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferedCopyTooSmall {
    required: usize,
    provided: usize,
}

impl BufferedCopyTooSmall {
    const fn new(required: usize, provided: usize) -> Self {
        Self { required, provided }
    }

    /// Returns the number of bytes necessary to copy the buffered negotiation data.
    #[must_use]
    pub const fn required(self) -> usize {
        self.required
    }

    /// Returns the number of bytes made available by the caller.
    #[must_use]
    pub const fn provided(self) -> usize {
        self.provided
    }

    /// Returns how many additional bytes would have been required for the copy to succeed.
    ///
    /// The difference is calculated with saturation to guard against inconsistent inputs. When the
    /// error originates from helpers such as [`NegotiatedStream::copy_buffered_into_slice`], the
    /// return value matches `required - provided`, mirroring the missing capacity reported by
    /// upstream rsync diagnostics.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nrest".to_vec()))
    ///     .expect("sniff succeeds");
    /// let mut scratch = [0u8; 4];
    /// let err = stream
    ///     .copy_buffered_into_slice(&mut scratch)
    ///     .expect_err("insufficient capacity must error");
    /// assert_eq!(err.missing(), stream.buffered_len() - scratch.len());
    /// ```
    #[must_use]
    pub const fn missing(self) -> usize {
        self.required.saturating_sub(self.provided)
    }
}

impl fmt::Display for BufferedCopyTooSmall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "buffered negotiation data requires {} bytes but destination provided {}",
            self.required, self.provided
        )
    }
}

impl std::error::Error for BufferedCopyTooSmall {}

impl<R> NegotiatedStream<R> {
    /// Returns the negotiation style determined while sniffing the transport.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        self.decision
    }

    /// Ensures the sniffed negotiation matches the expected style.
    ///
    /// The helper mirrors the checks performed by the binary and legacy
    /// handshake wrappers, returning an [`io::ErrorKind::InvalidData`] error
    /// with the supplied message when the peer advertises a different
    /// negotiation style. Centralising the logic keeps the error strings used
    /// across the transport crate in sync and avoids drift when additional
    /// call sites are introduced.
    pub fn ensure_decision(
        &self,
        expected: NegotiationPrologue,
        error_message: &'static str,
    ) -> io::Result<()> {
        match self.decision {
            decision if decision == expected => Ok(()),
            NegotiationPrologue::NeedMoreData => {
                unreachable!("negotiation sniffer fully classifies the prologue")
            }
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, error_message)),
        }
    }

    /// Returns the bytes that were required to classify the negotiation prologue.
    #[must_use]
    pub fn sniffed_prefix(&self) -> &[u8] {
        self.buffer.sniffed_prefix()
    }

    /// Returns the unread bytes buffered beyond the sniffed negotiation prefix.
    #[must_use]
    pub fn buffered_remainder(&self) -> &[u8] {
        self.buffer.buffered_remainder()
    }

    /// Returns the bytes captured during negotiation sniffing, including the prefix and remainder.
    #[must_use]
    pub fn buffered(&self) -> &[u8] {
        self.buffer.buffered()
    }

    /// Copies the buffered negotiation data into a caller-provided vector without consuming it.
    ///
    /// The helper mirrors [`Self::buffered`] but writes into an owned [`Vec<u8>`], allowing callers
    /// to reuse heap storage between sessions. Any additional capacity required to hold the
    /// buffered bytes is reserved via [`Vec::try_reserve`], so allocation failures are surfaced
    /// through [`TryReserveError`]. The vector is cleared before the bytes are appended, matching
    /// upstream rsync's behaviour where replay buffers replace any previous contents.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let mut replay = Vec::new();
    /// stream
    ///     .copy_buffered_into_vec(&mut replay)
    ///     .expect("vector can reserve space for replay bytes");
    /// assert_eq!(replay.as_slice(), stream.buffered());
    /// ```
    #[must_use = "the result reports whether the replay vector had sufficient capacity"]
    pub fn copy_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        self.buffer.copy_into_vec(target)
    }

    /// Returns the sniffed negotiation prefix together with any buffered remainder.
    ///
    /// The tuple mirrors the view exposed by [`NegotiationPrologueSniffer::buffered_split`],
    /// allowing higher layers to borrow both slices simultaneously when staging replay
    /// buffers. The first element contains the portion of the canonical prefix that has not
    /// yet been replayed, while the second slice exposes any additional payload that
    /// arrived alongside the detection bytes and remains buffered.
    #[must_use]
    pub fn buffered_split(&self) -> (&[u8], &[u8]) {
        self.buffer.buffered_split()
    }

    /// Returns the total number of buffered bytes staged for replay.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.buffered_len()
    }

    /// Returns how many buffered bytes have already been replayed.
    ///
    /// The counter starts at zero and increases as callers consume data through [`Read::read`],
    /// [`Read::read_vectored`], or [`BufRead::consume`]. Once it matches [`Self::buffered_len`], the
    /// replay buffer has been exhausted and subsequent reads operate on the inner transport.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, Read};
    ///
    /// let mut stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// assert_eq!(stream.buffered_consumed(), 0);
    ///
    /// let mut buf = [0u8; 4];
    /// stream.read(&mut buf).expect("buffered bytes are available");
    /// assert_eq!(stream.buffered_consumed(), 4);
    /// ```
    #[must_use]
    pub fn buffered_consumed(&self) -> usize {
        self.buffer.buffered_consumed()
    }

    /// Returns the length of the sniffed negotiation prefix.
    ///
    /// The value is the number of bytes that were required to classify the
    /// negotiation prologue. For legacy ASCII handshakes it matches the length
    /// of the canonical `@RSYNCD:` prefix. The method complements
    /// [`Self::sniffed_prefix_remaining`], which tracks how many of those bytes
    /// have yet to be replayed.
    #[must_use]
    pub const fn sniffed_prefix_len(&self) -> usize {
        self.buffer.sniffed_prefix_len()
    }

    /// Returns how many bytes from the sniffed negotiation prefix remain buffered.
    ///
    /// The value decreases as callers consume the replay data (for example via
    /// [`Read::read`] or [`BufRead::consume`]). A return value of zero indicates that
    /// the entire detection prefix has been drained and subsequent reads operate
    /// directly on the inner transport.
    #[must_use]
    pub fn sniffed_prefix_remaining(&self) -> usize {
        self.buffer.sniffed_prefix_remaining()
    }

    /// Reports whether the canonical legacy negotiation prefix has been fully buffered.
    ///
    /// For legacy daemon sessions this becomes `true` once the entire `@RSYNCD:` marker has been
    /// captured during negotiation sniffing. Binary negotiations never observe that prefix, so the
    /// method returns `false` for them. The return value is unaffected by how many buffered bytes
    /// have already been consumed, allowing higher layers to detect whether they may replay the
    /// prefix into the legacy greeting parser without issuing additional reads from the transport.
    #[must_use]
    pub fn legacy_prefix_complete(&self) -> bool {
        self.buffer.legacy_prefix_complete()
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

    /// Releases the wrapper and returns the raw negotiation components together with the inner reader.
    ///
    /// The tuple mirrors the state captured during sniffing: the detected [`NegotiationPrologue`], the
    /// length of the buffered prefix, the current replay cursor (how many buffered bytes were already
    /// consumed), the owned buffer containing the sniffed bytes, and the inner reader. This variant
    /// avoids cloning the buffer when higher layers need to persist or reuse the sniffed data. The
    /// replay cursor and prefix length are preserved so callers can reconstruct a [`NegotiatedStream`]
    /// using [`Self::from_raw_parts`] without losing track of partially consumed replay bytes.
    #[must_use]
    pub fn into_raw_parts(self) -> (NegotiationPrologue, usize, usize, Vec<u8>, R) {
        self.into_parts().into_raw_parts()
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

    /// Copies the buffered negotiation prefix and any captured remainder into `target`.
    ///
    /// The helper provides read-only access to the bytes that were observed while detecting the
    /// negotiation style without consuming them. Callers can therefore inspect, log, or cache the
    /// handshake transcript while continuing to rely on the replaying [`Read`] implementation for
    /// subsequent parsing. The destination vector is cleared before new data is written; its
    /// capacity is grown as needed and the resulting length is returned for convenience.
    ///
    /// # Errors
    ///
    /// Propagates [`TryReserveError`] when the destination vector cannot be grown to hold the
    /// buffered bytes. On failure `target` retains its previous contents so callers can recover or
    /// surface the allocation error.
    #[must_use = "the returned length reports how many bytes were copied and whether allocation succeeded"]
    pub fn copy_buffered_into(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        self.buffer.copy_into_vec(target)
    }

    /// Copies the buffered negotiation data into the provided vectored buffers without consuming it.
    ///
    /// The helper mirrors [`Self::copy_buffered_into_slice`] but operates on a slice of
    /// [`IoSliceMut`], allowing callers to scatter the replay bytes across multiple scratch buffers
    /// without reallocating. This is particularly useful when staging transcript snapshots inside
    /// pre-allocated ring buffers or logging structures while keeping the replay cursor untouched.
    ///
    /// # Errors
    ///
    /// Returns [`BufferedCopyTooSmall`] when the combined capacity of `bufs` is smaller than the
    /// buffered negotiation payload. On success the buffers are populated sequentially and the total
    /// number of written bytes is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, IoSliceMut};
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let expected = stream.buffered().to_vec();
    /// let mut head = [0u8; 12];
    /// let mut tail = [0u8; 32];
    /// let mut bufs = [IoSliceMut::new(&mut head), IoSliceMut::new(&mut tail)];
    /// let copied = stream
    ///     .copy_buffered_into_vectored(&mut bufs)
    ///     .expect("buffers are large enough");
    ///
    /// let prefix_len = head.len().min(copied);
    /// let mut assembled = Vec::new();
    /// assembled.extend_from_slice(&head[..prefix_len]);
    /// let remainder_len = copied - prefix_len;
    /// if remainder_len > 0 {
    ///     assembled.extend_from_slice(&tail[..remainder_len]);
    /// }
    /// assert_eq!(assembled, expected);
    /// ```
    #[must_use = "the return value conveys whether the provided buffers were large enough"]
    pub fn copy_buffered_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer.copy_all_into_vectored(bufs)
    }

    /// Copies the buffered negotiation data into the caller-provided slice without consuming it.
    ///
    /// This mirrors [`Self::copy_buffered_into`] but avoids reallocating a [`Vec`]. Callers that
    /// stage replay data on the stack or inside fixed-size scratch buffers can therefore reuse
    /// their storage while keeping the sniffed bytes untouched. When the destination cannot hold
    /// the buffered data, a [`BufferedCopyTooSmall`] error is returned and the slice remains
    /// unchanged.
    #[must_use = "the result indicates if the destination slice could hold the buffered bytes"]
    pub fn copy_buffered_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer.copy_all_into_slice(target)
    }

    /// Copies the buffered negotiation data into a caller-provided array without consuming it.
    ///
    /// The helper mirrors [`Self::copy_buffered_into_slice`] but accepts a fixed-size array
    /// directly, allowing stack-allocated scratch storage to be reused without converting it into a
    /// mutable slice at the call site.
    #[must_use = "the result indicates if the destination array could hold the buffered bytes"]
    pub fn copy_buffered_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer.copy_all_into_array(target)
    }

    /// Streams the buffered negotiation data into the provided writer without consuming it.
    ///
    /// The buffered bytes are written exactly once, mirroring upstream rsync's behaviour when the
    /// handshake transcript is echoed into logs or diagnostics. Any I/O error reported by the
    /// writer is propagated unchanged.
    #[must_use = "the returned length reports how many bytes were written and surfaces I/O failures"]
    pub fn copy_buffered_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        self.buffer.copy_all_into_writer(target)
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

    /// Attempts to transform the inner reader while keeping the buffered negotiation state intact.
    ///
    /// The closure returns the replacement reader on success or a tuple containing the error and
    /// original reader on failure. The latter allows callers to recover the original
    /// [`NegotiatedStream`] without losing any replay bytes.
    pub fn try_map_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<NegotiatedStream<T>, TryMapInnerError<NegotiatedStream<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        self.into_parts()
            .try_map_inner(map)
            .map(NegotiatedStreamParts::into_stream)
            .map_err(|err| err.map_original(NegotiatedStreamParts::into_stream))
    }

    /// Reconstructs a [`NegotiatedStream`] from previously extracted raw components.
    ///
    /// Callers typically pair this with [`Self::into_raw_parts`], allowing negotiation state to be
    /// stored temporarily (for example across async boundaries) and later resumed without replaying the
    /// sniffed prefix. The provided lengths are clamped to the buffer capacity to maintain the
    /// invariants enforced during initial construction.
    #[must_use]
    pub fn from_raw_parts(
        inner: R,
        decision: NegotiationPrologue,
        sniffed_prefix_len: usize,
        buffered_pos: usize,
        buffered: Vec<u8>,
    ) -> Self {
        Self::from_raw_components(inner, decision, sniffed_prefix_len, buffered_pos, buffered)
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
    #[doc(alias = "@RSYNCD")]
    pub fn read_legacy_daemon_line(&mut self, line: &mut Vec<u8>) -> io::Result<()> {
        self.read_legacy_line(line, true)
    }

    /// Reads and parses the legacy daemon greeting using the replaying stream wrapper.
    ///
    /// The helper forwards to [`Self::read_legacy_daemon_line`] before delegating to
    /// [`rsync_protocol::parse_legacy_daemon_greeting_bytes`]. On success the negotiated
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
    /// The helper mirrors [`rsync_protocol::parse_legacy_daemon_message_bytes`] but operates on the
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
    /// [`rsync_protocol::parse_legacy_error_message_bytes`]. Any parsing failure is converted into
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

        match self.decision {
            NegotiationPrologue::LegacyAscii => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation has not been detected",
                ));
            }
        }

        let prefix_len = self.buffer.sniffed_prefix_len();
        let legacy_prefix_complete = self.buffer.legacy_prefix_complete();
        let remaining = self.buffer.sniffed_prefix_remaining();
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
                    if observed.contains(&b'\n') {
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

impl<R: Write> Write for NegotiatedStream<R> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.inner.write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn write_fmt(&mut self, fmt: fmt::Arguments<'_>) -> io::Result<()> {
        self.inner.write_fmt(fmt)
    }
}

/// Components extracted from a [`NegotiatedStream`].
///
/// # Examples
///
/// Decompose the replaying stream into its constituent pieces and resume consumption once any
/// inspection or wrapping is complete.
///
/// ```
/// use rsync_protocol::NegotiationPrologue;
/// use rsync_transport::sniff_negotiation_stream;
/// use std::io::{Cursor, Read};
///
/// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
///     .expect("sniff succeeds");
/// let parts = stream.into_parts();
/// assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
///
/// let mut rebuilt = parts.into_stream();
/// let mut replay = Vec::new();
/// rebuilt
///     .read_to_end(&mut replay)
///     .expect("replayed bytes remain available");
/// assert_eq!(replay, b"@RSYNCD: 31.0\nreply");
/// ```
#[derive(Clone, Debug)]
pub struct NegotiatedStreamParts<R> {
    decision: NegotiationPrologue,
    buffer: NegotiationBuffer,
    inner: R,
}

/// Error returned when mapping the inner transport fails.
///
/// The structure preserves the original value so callers can continue using it after handling the
/// error. This mirrors the ergonomics of APIs such as `BufReader::into_inner`, ensuring buffered
/// negotiation bytes are not lost when a transformation cannot be completed.
///
/// # Examples
///
/// Propagate a failed transport transformation without losing the replaying stream. The preserved
/// value can be recovered via [`TryMapInnerError::into_original`] and consumed just like the
/// original [`NegotiatedStream`].
///
/// ```
/// use rsync_transport::sniff_negotiation_stream;
/// use std::io::{self, Cursor, Read};
///
/// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
///     .expect("sniff succeeds");
/// let result = stream.try_map_inner(|cursor| -> Result<Cursor<Vec<u8>>, (io::Error, Cursor<Vec<u8>>)> {
///     Err((io::Error::new(io::ErrorKind::Other, "wrap failed"), cursor))
/// });
/// let err = result.expect_err("mapping fails");
/// assert_eq!(err.error().kind(), io::ErrorKind::Other);
///
/// let mut restored = err.into_original();
/// let mut replay = Vec::new();
/// restored
///     .read_to_end(&mut replay)
///     .expect("replayed bytes remain available");
/// assert_eq!(replay, b"@RSYNCD: 31.0\n");
/// ```
pub struct TryMapInnerError<T, E> {
    error: E,
    original: T,
}

impl<T, E> TryMapInnerError<T, E> {
    fn new(error: E, original: T) -> Self {
        Self { error, original }
    }

    /// Returns a shared reference to the underlying error.
    #[must_use]
    pub const fn error(&self) -> &E {
        &self.error
    }

    /// Returns a mutable reference to the underlying error.
    ///
    /// This mirrors [`Self::error`] but allows callers to adjust the preserved error in-place before
    /// reusing the buffered transport state. Upstream rsync occasionally downgrades rich errors to
    /// more specific variants (for example timeouts) prior to surfacing them, so exposing a mutable
    /// handle keeps those transformations possible without reconstructing the entire
    /// [`TryMapInnerError`].
    #[must_use]
    pub fn error_mut(&mut self) -> &mut E {
        &mut self.error
    }

    /// Returns a shared reference to the value that failed to be mapped.
    #[must_use]
    pub const fn original(&self) -> &T {
        &self.original
    }

    /// Returns a mutable reference to the value that failed to be mapped.
    ///
    /// The mutable accessor mirrors [`Self::original`] and allows callers to prepare the preserved
    /// transport (for example by consuming buffered bytes or tweaking adapter state) before the
    /// error is resolved. The modifications are retained when [`Self::into_original`] is invoked,
    /// matching the behaviour of upstream rsync where negotiation buffers remain usable after a
    /// failed transformation.
    #[must_use]
    pub fn original_mut(&mut self) -> &mut T {
        &mut self.original
    }

    /// Decomposes the error into its parts.
    #[must_use]
    pub fn into_parts(self) -> (E, T) {
        (self.error, self.original)
    }

    /// Returns ownership of the error, discarding the original value.
    #[must_use]
    pub fn into_error(self) -> E {
        self.error
    }

    /// Returns ownership of the original value, discarding the error.
    #[must_use]
    pub fn into_original(self) -> T {
        self.original
    }

    /// Maps the preserved value into another type while retaining the error.
    #[must_use]
    pub fn map_original<U, F>(self, map: F) -> TryMapInnerError<U, E>
    where
        F: FnOnce(T) -> U,
    {
        let (error, original) = self.into_parts();
        TryMapInnerError::new(error, map(original))
    }

    /// Maps the preserved error into another type while retaining the original value.
    ///
    /// This mirrors [`Self::map_original`] but transforms the stored error instead. It is useful when
    /// callers need to downgrade rich error types (for example to [`io::ErrorKind`]) without losing the
    /// buffered transport state captured by [`TryMapInnerError`].
    #[must_use]
    pub fn map_error<E2, F>(self, map: F) -> TryMapInnerError<T, E2>
    where
        F: FnOnce(E) -> E2,
    {
        let (error, original) = self.into_parts();
        TryMapInnerError::new(map(error), original)
    }
}

impl<T, E: fmt::Debug> fmt::Debug for TryMapInnerError<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TryMapInnerError")
            .field("error", &self.error)
            .finish()
    }
}

impl<T, E: fmt::Display> fmt::Display for TryMapInnerError<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to map inner value: {}", self.error)
    }
}

impl<T, E> std::error::Error for TryMapInnerError<T, E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
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

    /// Copies the buffered negotiation data into a caller-provided vector without consuming it.
    ///
    /// The helper mirrors [`Self::buffered`] but writes the bytes into an owned [`Vec<u8>`], making
    /// it straightforward to persist handshake transcripts or reuse heap storage across sessions.
    /// The vector is cleared before the data is appended. Any additional capacity required to
    /// complete the copy is reserved using [`Vec::try_reserve`], with allocation failures reported
    /// via [`TryReserveError`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let parts = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds")
    ///     .into_parts();
    /// let mut replay = Vec::with_capacity(4);
    /// parts
    ///     .copy_buffered_into_vec(&mut replay)
    ///     .expect("vector can reserve space for replay bytes");
    /// assert_eq!(replay.as_slice(), parts.buffered());
    /// ```
    #[must_use = "the result reports whether the replay vector had sufficient capacity"]
    pub fn copy_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        self.buffer.copy_into_vec(target)
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

    /// Returns how many buffered bytes had already been replayed when the stream was decomposed.
    ///
    /// The counter mirrors [`NegotiatedStream::buffered_consumed`], allowing callers to observe the
    /// replay position without reconstructing the wrapper. It is useful when higher layers need to
    /// resume consumption from the point where the stream was split into parts.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, Read};
    ///
    /// let mut stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let mut buf = [0u8; 5];
    /// stream.read(&mut buf).expect("buffered bytes are available");
    /// let consumed = stream.buffered_consumed();
    ///
    /// let parts = stream.into_parts();
    /// assert_eq!(parts.buffered_consumed(), consumed);
    /// ```
    #[must_use]
    pub fn buffered_consumed(&self) -> usize {
        self.buffer.buffered_consumed()
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

    /// Returns how many bytes from the sniffed negotiation prefix remain buffered.
    ///
    /// This mirrors [`NegotiatedStream::sniffed_prefix_remaining`], giving callers
    /// access to the replay position after extracting the parts without
    /// reconstructing a [`NegotiatedStream`].
    #[must_use]
    pub fn sniffed_prefix_remaining(&self) -> usize {
        self.buffer.sniffed_prefix_remaining()
    }

    /// Reports whether the canonical legacy negotiation prefix has been fully buffered.
    ///
    /// The method mirrors [`NegotiatedStream::legacy_prefix_complete`], making it possible to query
    /// the sniffed prefix state even after the stream has been decomposed into parts. It is `true`
    /// for legacy ASCII negotiations once the entire `@RSYNCD:` marker has been captured and `false`
    /// otherwise (including for binary sessions).
    #[must_use]
    pub fn legacy_prefix_complete(&self) -> bool {
        self.buffer.legacy_prefix_complete()
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

    /// Copies the buffered negotiation bytes into the destination vector without consuming them.
    ///
    /// This mirrors [`NegotiatedStream::copy_buffered_into`], allowing callers that temporarily
    /// decompose the stream into parts to observe the sniffed prefix and remainder while preserving
    /// the replay state. The destination is cleared before data is appended; if additional capacity
    /// is required a [`TryReserveError`] is returned and the original contents remain untouched.
    #[must_use = "the returned length reports how many bytes were copied and whether allocation succeeded"]
    pub fn copy_buffered_into(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        self.buffer.copy_into_vec(target)
    }

    /// Copies the buffered negotiation data into the provided vectored buffers without consuming it.
    ///
    /// The helper mirrors [`Self::copy_buffered_into_slice`] while operating on a mutable slice of
    /// [`IoSliceMut`]. This is useful when the stream has been decomposed into parts but callers
    /// still need to scatter the sniffed negotiation transcript across multiple scratch buffers
    /// without cloning the stored bytes.
    ///
    /// # Errors
    ///
    /// Returns [`BufferedCopyTooSmall`] if the combined capacity of `bufs` is smaller than the
    /// buffered negotiation payload.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, IoSliceMut};
    ///
    /// let parts = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds")
    ///     .into_parts();
    /// let expected = parts.buffered().to_vec();
    /// let mut first = [0u8; 10];
    /// let mut second = [0u8; 32];
    /// let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    /// let copied = parts
    ///     .copy_buffered_into_vectored(&mut bufs)
    ///     .expect("buffers are large enough");
    ///
    /// let prefix_len = first.len().min(copied);
    /// let mut assembled = Vec::new();
    /// assembled.extend_from_slice(&first[..prefix_len]);
    /// let remainder_len = copied - prefix_len;
    /// if remainder_len > 0 {
    ///     assembled.extend_from_slice(&second[..remainder_len]);
    /// }
    /// assert_eq!(assembled, expected);
    /// ```
    #[must_use = "the return value conveys whether the provided buffers were large enough"]
    pub fn copy_buffered_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer.copy_all_into_vectored(bufs)
    }

    /// Copies the buffered negotiation data into the caller-provided slice without consuming it.
    #[must_use = "the result indicates if the destination slice could hold the buffered bytes"]
    pub fn copy_buffered_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer.copy_all_into_slice(target)
    }

    /// Copies the buffered negotiation data into a caller-provided array without consuming it.
    #[must_use = "the result indicates if the destination array could hold the buffered bytes"]
    pub fn copy_buffered_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer.copy_all_into_array(target)
    }

    /// Streams the buffered negotiation data into the provided writer without consuming it.
    #[must_use = "the returned length reports how many bytes were written and surfaces I/O failures"]
    pub fn copy_buffered_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        self.buffer.copy_all_into_writer(target)
    }

    /// Attempts to transform the inner reader while preserving the buffered negotiation state.
    ///
    /// When the mapping fails the original reader is returned alongside the error, ensuring callers
    /// retain access to the sniffed bytes without needing to re-run negotiation detection.
    pub fn try_map_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<NegotiatedStreamParts<T>, TryMapInnerError<NegotiatedStreamParts<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        let Self {
            decision,
            buffer,
            inner,
        } = self;

        match map(inner) {
            Ok(mapped) => Ok(NegotiatedStreamParts {
                decision,
                buffer,
                inner: mapped,
            }),
            Err((error, original)) => Err(TryMapInnerError::new(
                error,
                NegotiatedStreamParts {
                    decision,
                    buffer,
                    inner: original,
                },
            )),
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

    /// Releases the parts structure and returns the raw negotiation components together with the reader.
    ///
    /// The returned tuple includes the detected [`NegotiationPrologue`], the length of the sniffed
    /// prefix, the number of buffered bytes that were already consumed, the owned buffer containing the
    /// sniffed data, and the inner reader. This mirrors the layout used by [`NegotiatedStream::into_raw_parts`]
    /// while avoiding an intermediate reconstruction of the wrapper when only the raw buffers are needed.
    #[must_use]
    pub fn into_raw_parts(self) -> (NegotiationPrologue, usize, usize, Vec<u8>, R) {
        let (decision, buffer, inner) = self.into_components();
        let (sniffed_prefix_len, buffered_pos, buffered) = buffer.into_raw_parts();
        (decision, sniffed_prefix_len, buffered_pos, buffered, inner)
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
        let start = self
            .buffered_pos
            .max(self.sniffed_prefix_len())
            .min(self.buffered.len());
        &self.buffered[start..]
    }

    fn buffered(&self) -> &[u8] {
        &self.buffered
    }

    fn buffered_split(&self) -> (&[u8], &[u8]) {
        let prefix_len = self.sniffed_prefix_len();
        debug_assert!(prefix_len <= self.buffered.len());

        let consumed_prefix = self.buffered_pos.min(prefix_len);
        let prefix_start = consumed_prefix;
        let prefix_slice = &self.buffered[prefix_start..prefix_len];

        let remainder_start = self.buffered_pos.max(prefix_len).min(self.buffered.len());
        let remainder_slice = &self.buffered[remainder_start..];

        (prefix_slice, remainder_slice)
    }

    const fn sniffed_prefix_len(&self) -> usize {
        self.sniffed_prefix_len
    }

    fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    fn buffered_consumed(&self) -> usize {
        self.buffered_pos
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

    fn copy_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        let required = self.buffered.len();
        let len = target.len();
        target.try_reserve(required.saturating_sub(len))?;

        target.clear();
        target.extend_from_slice(&self.buffered);
        Ok(target.len())
    }

    fn copy_all_into_slice(&self, target: &mut [u8]) -> Result<usize, BufferedCopyTooSmall> {
        let required = self.buffered.len();
        if target.len() < required {
            return Err(BufferedCopyTooSmall::new(required, target.len()));
        }

        target[..required].copy_from_slice(&self.buffered);
        Ok(required)
    }

    fn copy_all_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.copy_all_into_slice(target.as_mut_slice())
    }

    fn copy_all_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        target.write_all(&self.buffered)?;
        Ok(self.buffered.len())
    }

    fn copy_all_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        let required = self.buffered.len();
        if required == 0 {
            return Ok(0);
        }

        let mut provided = 0usize;
        for buf in bufs.iter() {
            provided = provided.saturating_add(buf.len());
        }

        if provided < required {
            return Err(BufferedCopyTooSmall::new(required, provided));
        }

        let mut written = 0usize;
        for buf in bufs.iter_mut() {
            if written == required {
                break;
            }

            let slice = buf.as_mut();
            if slice.is_empty() {
                continue;
            }

            let to_copy = (required - written).min(slice.len());
            slice[..to_copy].copy_from_slice(&self.buffered[written..written + to_copy]);
            written += to_copy;
        }

        debug_assert_eq!(written, required);
        Ok(required)
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

    fn into_raw_parts(self) -> (usize, usize, Vec<u8>) {
        let Self {
            sniffed_prefix_len,
            buffered_pos,
            buffered,
        } = self;
        (sniffed_prefix_len, buffered_pos, buffered)
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

    use std::{
        collections::TryReserveError,
        error::Error as _,
        io::{self, BufRead, Cursor, IoSlice, IoSliceMut, Read, Write},
    };

    use rsync_protocol::{LEGACY_DAEMON_PREFIX_LEN, ProtocolVersion};

    #[test]
    fn map_line_reserve_error_for_io_marks_out_of_memory() {
        let mut buf = Vec::<u8>::new();
        let reserve_err = buf
            .try_reserve_exact(usize::MAX)
            .expect_err("capacity overflow must error");

        let mapped = super::map_line_reserve_error_for_io(reserve_err);
        assert_eq!(mapped.kind(), io::ErrorKind::OutOfMemory);
        assert!(
            mapped
                .to_string()
                .contains("failed to reserve memory for legacy negotiation buffer")
        );

        let source = mapped.source().expect("mapped error must retain source");
        assert!(source.downcast_ref::<TryReserveError>().is_some());
    }

    fn sniff_bytes(data: &[u8]) -> io::Result<NegotiatedStream<Cursor<Vec<u8>>>> {
        let cursor = Cursor::new(data.to_vec());
        sniff_negotiation_stream(cursor)
    }

    #[derive(Debug)]
    struct RecordingTransport {
        reader: Cursor<Vec<u8>>,
        writes: Vec<u8>,
        flushes: usize,
    }

    impl RecordingTransport {
        fn new(input: &[u8]) -> Self {
            Self {
                reader: Cursor::new(input.to_vec()),
                writes: Vec::new(),
                flushes: 0,
            }
        }

        fn from_cursor(cursor: Cursor<Vec<u8>>) -> Self {
            Self {
                reader: cursor,
                writes: Vec::new(),
                flushes: 0,
            }
        }

        fn writes(&self) -> &[u8] {
            &self.writes
        }

        fn flushes(&self) -> usize {
            self.flushes
        }
    }

    impl Read for RecordingTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reader.read(buf)
        }
    }

    impl Write for RecordingTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            let mut total = 0;
            for slice in bufs {
                self.writes.extend_from_slice(slice);
                total += slice.len();
            }
            Ok(total)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn sniff_negotiation_detects_binary_prefix() {
        let mut stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
        assert_eq!(stream.sniffed_prefix(), &[0x00]);
        assert_eq!(stream.sniffed_prefix_len(), 1);
        assert_eq!(stream.sniffed_prefix_remaining(), 1);
        assert!(!stream.legacy_prefix_complete());
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
        assert_eq!(stream.sniffed_prefix_remaining(), 0);
        assert!(!stream.legacy_prefix_complete());

        let mut tail = [0u8; 2];
        let read = stream
            .read(&mut tail)
            .expect("read after buffer consumes inner");
        assert_eq!(read, 0);
        assert!(tail.iter().all(|byte| *byte == 0));

        let parts = stream.into_parts();
        assert!(!parts.legacy_prefix_complete());
    }

    #[test]
    fn ensure_decision_accepts_matching_style() {
        let stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
        stream
            .ensure_decision(
                NegotiationPrologue::LegacyAscii,
                "legacy daemon negotiation requires @RSYNCD: prefix",
            )
            .expect("legacy decision matches expectation");
    }

    #[test]
    fn ensure_decision_rejects_mismatched_style() {
        let stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
        let err = stream
            .ensure_decision(
                NegotiationPrologue::LegacyAscii,
                "legacy daemon negotiation requires @RSYNCD: prefix",
            )
            .expect_err("binary decision must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            err.to_string(),
            "legacy daemon negotiation requires @RSYNCD: prefix"
        );
    }

    #[test]
    fn buffered_consumed_tracks_reads() {
        let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nremaining").expect("sniff succeeds");
        assert_eq!(stream.buffered_consumed(), 0);

        let total = stream.buffered_len();
        assert!(total > 0);

        let mut remaining = total;
        let mut scratch = [0u8; 4];
        while remaining > 0 {
            let chunk = remaining.min(scratch.len());
            let read = stream
                .read(&mut scratch[..chunk])
                .expect("buffered bytes are readable");
            assert!(read > 0);
            remaining -= read;
            assert_eq!(stream.buffered_consumed(), total - remaining);
        }

        assert_eq!(stream.buffered_consumed(), total);
    }

    #[test]
    fn parts_buffered_consumed_matches_stream_state() {
        let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
        let total = stream.buffered_len();
        assert!(total > 1);

        let mut prefix = vec![0u8; total - 1];
        stream
            .read_exact(&mut prefix)
            .expect("buffered prefix is replayed");
        assert_eq!(stream.buffered_consumed(), total - 1);

        let parts = stream.into_parts();
        assert_eq!(parts.buffered_consumed(), total - 1);
        assert_eq!(parts.buffered_remaining(), 1);
    }

    #[test]
    fn sniffed_prefix_remaining_tracks_consumed_bytes() {
        let mut stream = sniff_bytes(b"@RSYNCD: 29.0\nrest").expect("sniff succeeds");
        assert_eq!(stream.sniffed_prefix_remaining(), LEGACY_DAEMON_PREFIX_LEN);
        assert!(stream.legacy_prefix_complete());

        let mut prefix_fragment = [0u8; 3];
        stream
            .read_exact(&mut prefix_fragment)
            .expect("prefix fragment is replayed first");
        assert_eq!(
            stream.sniffed_prefix_remaining(),
            LEGACY_DAEMON_PREFIX_LEN - prefix_fragment.len()
        );
        assert!(stream.legacy_prefix_complete());

        let remaining_len = LEGACY_DAEMON_PREFIX_LEN - prefix_fragment.len();
        let mut rest_of_prefix = vec![0u8; remaining_len];
        stream
            .read_exact(&mut rest_of_prefix)
            .expect("remaining prefix bytes are replayed");
        assert_eq!(stream.sniffed_prefix_remaining(), 0);
        assert_eq!(rest_of_prefix, b"YNCD:");
        assert!(stream.legacy_prefix_complete());
    }

    #[test]
    fn sniffed_prefix_remaining_visible_on_parts() {
        let initial_parts = sniff_bytes(b"@RSYNCD: 31.0\n")
            .expect("sniff succeeds")
            .into_parts();
        assert_eq!(
            initial_parts.sniffed_prefix_remaining(),
            LEGACY_DAEMON_PREFIX_LEN
        );
        assert!(initial_parts.legacy_prefix_complete());

        let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
        let mut prefix_fragment = [0u8; 5];
        stream
            .read_exact(&mut prefix_fragment)
            .expect("prefix fragment is replayed");
        let parts = stream.into_parts();
        assert_eq!(
            parts.sniffed_prefix_remaining(),
            LEGACY_DAEMON_PREFIX_LEN - prefix_fragment.len()
        );
        assert!(parts.legacy_prefix_complete());
    }

    #[test]
    fn negotiated_stream_copy_buffered_into_preserves_replay_state() {
        let mut stream = sniff_bytes(b"@RSYNCD: 31.0\ntrailing").expect("sniff succeeds");
        let expected = stream.buffered().to_vec();
        let buffered_remaining = stream.buffered_remaining();

        let mut scratch = Vec::from([0xAAu8, 0xBB]);
        let copied = stream
            .copy_buffered_into(&mut scratch)
            .expect("copying buffered bytes succeeds");

        assert_eq!(copied, expected.len());
        assert_eq!(scratch, expected);
        assert_eq!(stream.buffered_remaining(), buffered_remaining);

        let mut replay = vec![0u8; expected.len()];
        stream
            .read_exact(&mut replay)
            .expect("buffered bytes remain available after copying");
        assert_eq!(replay, expected);
    }

    #[test]
    fn negotiated_stream_copy_buffered_into_grows_from_sparse_len() {
        let stream = sniff_bytes(b"@RSYNCD: 31.0\nlegacy daemon payload").expect("sniff succeeds");
        let expected = stream.buffered().to_vec();

        let mut scratch = Vec::with_capacity(expected.len() / 2);
        scratch.extend_from_slice(b"seed");
        scratch.truncate(1);
        assert!(scratch.capacity() < expected.len());
        assert_eq!(scratch.len(), 1);

        let copied = stream
            .copy_buffered_into(&mut scratch)
            .expect("copying buffered bytes succeeds");

        assert_eq!(copied, expected.len());
        assert_eq!(scratch, expected);
    }

    #[test]
    fn negotiated_stream_copy_buffered_into_slice_copies_bytes() {
        let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nreplay").expect("sniff succeeds");
        let expected = stream.buffered().to_vec();
        let buffered_remaining = stream.buffered_remaining();

        let mut scratch = vec![0u8; expected.len()];
        let copied = stream
            .copy_buffered_into_slice(&mut scratch)
            .expect("copying into slice succeeds");

        assert_eq!(copied, expected.len());
        assert_eq!(scratch, expected);
        assert_eq!(stream.buffered_remaining(), buffered_remaining);

        let mut replay = vec![0u8; expected.len()];
        stream
            .read_exact(&mut replay)
            .expect("buffered bytes remain available after slicing copy");
        assert_eq!(replay, expected);
    }

    #[test]
    fn negotiated_stream_copy_buffered_into_vec_copies_bytes() {
        let stream = sniff_bytes(b"@RSYNCD: 31.0\nreplay").expect("sniff succeeds");
        let expected = stream.buffered().to_vec();

        let mut target = Vec::with_capacity(expected.len() + 8);
        target.extend_from_slice(b"junk data");
        let initial_capacity = target.capacity();
        let initial_ptr = target.as_ptr();

        let copied = stream
            .copy_buffered_into_vec(&mut target)
            .expect("copying into vec succeeds");

        assert_eq!(copied, expected.len());
        assert_eq!(target, expected);
        assert_eq!(target.capacity(), initial_capacity);
        assert_eq!(target.as_ptr(), initial_ptr);
    }

    #[test]
    fn negotiated_stream_copy_buffered_into_array_copies_bytes() {
        let mut stream = sniff_bytes(b"@RSYNCD: 31.0\narray").expect("sniff succeeds");
        let expected = stream.buffered().to_vec();
        let buffered_remaining = stream.buffered_remaining();

        let mut scratch = [0u8; 64];
        let copied = stream
            .copy_buffered_into_array(&mut scratch)
            .expect("copying into array succeeds");

        assert_eq!(copied, expected.len());
        assert_eq!(&scratch[..copied], expected.as_slice());
        assert_eq!(stream.buffered_remaining(), buffered_remaining);

        let mut replay = vec![0u8; expected.len()];
        stream
            .read_exact(&mut replay)
            .expect("buffered bytes remain available after array copy");
        assert_eq!(replay, expected);
    }

    #[test]
    fn negotiated_stream_copy_buffered_into_vectored_copies_bytes() {
        let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nvectored payload").expect("sniff succeeds");
        let expected = stream.buffered().to_vec();
        let buffered_remaining = stream.buffered_remaining();

        let mut prefix = [0u8; 12];
        let mut suffix = [0u8; 64];
        let mut bufs = [IoSliceMut::new(&mut prefix), IoSliceMut::new(&mut suffix)];
        let copied = stream
            .copy_buffered_into_vectored(&mut bufs)
            .expect("vectored copy succeeds");

        assert_eq!(copied, expected.len());

        let prefix_len = prefix.len().min(copied);
        let remainder_len = copied - prefix_len;
        let mut assembled = Vec::new();
        assembled.extend_from_slice(&prefix[..prefix_len]);
        if remainder_len > 0 {
            assembled.extend_from_slice(&suffix[..remainder_len]);
        }
        assert_eq!(assembled, expected);
        assert_eq!(stream.buffered_remaining(), buffered_remaining);

        let mut replay = vec![0u8; expected.len()];
        stream
            .read_exact(&mut replay)
            .expect("buffered bytes remain available after vectored copy");
        assert_eq!(replay, expected);
    }

    #[test]
    fn negotiated_stream_copy_buffered_into_vectored_reports_small_buffers() {
        let stream = sniff_bytes(b"@RSYNCD: 31.0\nshort").expect("sniff succeeds");
        let required = stream.buffered().len();

        let mut prefix = [0u8; 4];
        let mut suffix = [0u8; 3];
        let mut bufs = [IoSliceMut::new(&mut prefix), IoSliceMut::new(&mut suffix)];
        let err = stream
            .copy_buffered_into_vectored(&mut bufs)
            .expect_err("insufficient capacity must error");

        assert_eq!(err.required(), required);
        assert_eq!(err.provided(), prefix.len() + suffix.len());
        assert_eq!(err.missing(), required - (prefix.len() + suffix.len()));
    }

    #[test]
    fn negotiated_stream_copy_buffered_into_slice_reports_small_buffer() {
        let stream = sniff_bytes(b"@RSYNCD: 30.0\nrest").expect("sniff succeeds");
        let expected_len = stream.buffered_len();
        let buffered_remaining = stream.buffered_remaining();

        let mut scratch = vec![0u8; expected_len.saturating_sub(1)];
        let err = stream
            .copy_buffered_into_slice(&mut scratch)
            .expect_err("insufficient slice capacity must error");

        assert_eq!(err.required(), expected_len);
        assert_eq!(err.provided(), scratch.len());
        assert_eq!(err.missing(), expected_len - scratch.len());
        assert_eq!(stream.buffered_remaining(), buffered_remaining);
    }

    #[test]
    fn negotiated_stream_copy_buffered_into_array_reports_small_array() {
        let stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
        let expected_len = stream.buffered_len();
        let buffered_remaining = stream.buffered_remaining();

        let mut scratch = [0u8; 4];
        let err = stream
            .copy_buffered_into_array(&mut scratch)
            .expect_err("insufficient array capacity must error");

        assert_eq!(err.required(), expected_len);
        assert_eq!(err.provided(), scratch.len());
        assert_eq!(err.missing(), expected_len - scratch.len());
        assert_eq!(stream.buffered_remaining(), buffered_remaining);
    }

    #[test]
    fn negotiated_stream_copy_buffered_into_writer_copies_bytes() {
        let stream = sniff_bytes(b"@RSYNCD: 31.0\npayload").expect("sniff succeeds");
        let expected = stream.buffered().to_vec();
        let buffered_remaining = stream.buffered_remaining();

        let mut output = Vec::new();
        let written = stream
            .copy_buffered_into_writer(&mut output)
            .expect("writing buffered bytes succeeds");

        assert_eq!(written, expected.len());
        assert_eq!(output, expected);
        assert_eq!(stream.buffered_remaining(), buffered_remaining);
    }

    #[test]
    fn negotiated_stream_parts_copy_buffered_into_preserves_replay_state() {
        let parts = sniff_bytes(b"@RSYNCD: 30.0\nleftovers")
            .expect("sniff succeeds")
            .into_parts();
        let expected = parts.buffered().to_vec();

        let mut scratch = Vec::with_capacity(1);
        scratch.extend_from_slice(b"junk");
        let copied = parts
            .copy_buffered_into(&mut scratch)
            .expect("copying buffered bytes succeeds");

        assert_eq!(copied, expected.len());
        assert_eq!(scratch, expected);

        let mut rebuilt = parts.into_stream();
        let mut replay = vec![0u8; expected.len()];
        rebuilt
            .read_exact(&mut replay)
            .expect("rebuilt stream still replays buffered bytes");
        assert_eq!(replay, expected);
    }

    #[test]
    fn negotiated_stream_parts_copy_buffered_into_slice_copies_bytes() {
        let parts = sniff_bytes(b"@RSYNCD: 30.0\nlisting")
            .expect("sniff succeeds")
            .into_parts();
        let expected = parts.buffered().to_vec();

        let mut scratch = vec![0u8; expected.len()];
        let copied = parts
            .copy_buffered_into_slice(&mut scratch)
            .expect("copying into slice succeeds");

        assert_eq!(copied, expected.len());
        assert_eq!(scratch, expected);

        let mut rebuilt = parts.into_stream();
        let mut replay = vec![0u8; expected.len()];
        rebuilt
            .read_exact(&mut replay)
            .expect("rebuilt stream still replays buffered bytes after slice copy");
        assert_eq!(replay, expected);
    }

    #[test]
    fn negotiated_stream_parts_copy_buffered_into_vec_copies_bytes() {
        let parts = sniff_bytes(b"@RSYNCD: 30.0\nlisting")
            .expect("sniff succeeds")
            .into_parts();
        let expected = parts.buffered().to_vec();

        let mut target = Vec::with_capacity(expected.len() + 8);
        target.extend_from_slice(b"junk data");
        let initial_capacity = target.capacity();
        let initial_ptr = target.as_ptr();

        let copied = parts
            .copy_buffered_into_vec(&mut target)
            .expect("copying into vec succeeds");

        assert_eq!(copied, expected.len());
        assert_eq!(target, expected);
        assert_eq!(target.capacity(), initial_capacity);
        assert_eq!(target.as_ptr(), initial_ptr);
    }

    #[test]
    fn negotiated_stream_parts_copy_buffered_into_array_copies_bytes() {
        let parts = sniff_bytes(b"@RSYNCD: 30.0\nlisting")
            .expect("sniff succeeds")
            .into_parts();
        let expected = parts.buffered().to_vec();

        let mut scratch = [0u8; 64];
        let copied = parts
            .copy_buffered_into_array(&mut scratch)
            .expect("copying into array succeeds");

        assert_eq!(copied, expected.len());
        assert_eq!(&scratch[..copied], expected.as_slice());

        let mut rebuilt = parts.into_stream();
        let mut replay = vec![0u8; expected.len()];
        rebuilt
            .read_exact(&mut replay)
            .expect("rebuilt stream still replays buffered bytes after array copy");
        assert_eq!(replay, expected);
    }

    #[test]
    fn negotiated_stream_parts_copy_buffered_into_vectored_copies_bytes() {
        let parts = sniff_bytes(b"@RSYNCD: 30.0\nrecord")
            .expect("sniff succeeds")
            .into_parts();
        let expected = parts.buffered().to_vec();

        let mut first = [0u8; 10];
        let mut second = [0u8; 64];
        let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
        let copied = parts
            .copy_buffered_into_vectored(&mut bufs)
            .expect("vectored copy succeeds");

        assert_eq!(copied, expected.len());

        let prefix_len = first.len().min(copied);
        let remainder_len = copied - prefix_len;
        let mut assembled = Vec::new();
        assembled.extend_from_slice(&first[..prefix_len]);
        if remainder_len > 0 {
            assembled.extend_from_slice(&second[..remainder_len]);
        }
        assert_eq!(assembled, expected);
    }

    #[test]
    fn negotiated_stream_parts_copy_buffered_into_vectored_reports_small_buffers() {
        let parts = sniff_bytes(b"@RSYNCD: 31.0\nlimited")
            .expect("sniff succeeds")
            .into_parts();
        let expected_len = parts.buffered().len();

        let mut first = [0u8; 4];
        let mut second = [0u8; 3];
        let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
        let err = parts
            .copy_buffered_into_vectored(&mut bufs)
            .expect_err("insufficient capacity must error");

        assert_eq!(err.required(), expected_len);
        assert_eq!(err.provided(), first.len() + second.len());
        assert_eq!(err.missing(), expected_len - (first.len() + second.len()));
    }

    #[test]
    fn negotiated_stream_parts_copy_buffered_into_slice_reports_small_buffer() {
        let parts = sniff_bytes(b"@RSYNCD: 31.0\nlisting")
            .expect("sniff succeeds")
            .into_parts();
        let expected_len = parts.buffered_len();

        let mut scratch = vec![0u8; expected_len.saturating_sub(1)];
        let err = parts
            .copy_buffered_into_slice(&mut scratch)
            .expect_err("insufficient slice capacity must error");

        assert_eq!(err.required(), expected_len);
        assert_eq!(err.provided(), scratch.len());
        assert_eq!(err.missing(), expected_len - scratch.len());
    }

    #[test]
    fn negotiated_stream_parts_copy_buffered_into_array_reports_small_array() {
        let parts = sniff_bytes(b"@RSYNCD: 31.0\nlisting")
            .expect("sniff succeeds")
            .into_parts();
        let expected_len = parts.buffered_len();

        let mut scratch = [0u8; 4];
        let err = parts
            .copy_buffered_into_array(&mut scratch)
            .expect_err("insufficient array capacity must error");

        assert_eq!(err.required(), expected_len);
        assert_eq!(err.provided(), scratch.len());
        assert_eq!(err.missing(), expected_len - scratch.len());
    }

    #[test]
    fn negotiated_stream_parts_copy_buffered_into_writer_copies_bytes() {
        let parts = sniff_bytes(b"@RSYNCD: 30.0\ntrailing")
            .expect("sniff succeeds")
            .into_parts();
        let expected = parts.buffered().to_vec();

        let mut output = Vec::new();
        let written = parts
            .copy_buffered_into_writer(&mut output)
            .expect("writing buffered bytes succeeds");

        assert_eq!(written, expected.len());
        assert_eq!(output, expected);

        let mut rebuilt = parts.into_stream();
        let mut replay = vec![0u8; expected.len()];
        rebuilt
            .read_exact(&mut replay)
            .expect("rebuilt stream still replays buffered bytes after writer copy");
        assert_eq!(replay, expected);
    }

    #[test]
    fn legacy_prefix_complete_reports_status_for_legacy_sessions() {
        let mut stream = sniff_bytes(b"@RSYNCD: 30.0\nrest").expect("sniff succeeds");
        assert!(stream.legacy_prefix_complete());

        let mut consumed = [0u8; 4];
        stream
            .read_exact(&mut consumed)
            .expect("read_exact consumes part of the prefix");
        assert!(stream.legacy_prefix_complete());

        let parts = stream.into_parts();
        assert!(parts.legacy_prefix_complete());
    }

    #[test]
    fn legacy_prefix_complete_reports_status_for_binary_sessions() {
        let mut stream = sniff_bytes(&[0x00, 0x42, 0x99]).expect("sniff succeeds");
        assert!(!stream.legacy_prefix_complete());

        let mut consumed = [0u8; 1];
        stream
            .read_exact(&mut consumed)
            .expect("read_exact consumes buffered byte");
        assert!(!stream.legacy_prefix_complete());

        let parts = stream.into_parts();
        assert!(!parts.legacy_prefix_complete());
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
    fn try_map_inner_transforms_transport_without_losing_buffer() {
        let legacy = b"@RSYNCD: 31.0\nrest";
        let stream = sniff_bytes(legacy).expect("sniff succeeds");

        let mut mapped = stream
            .try_map_inner(
                |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                    Ok(RecordingTransport::from_cursor(cursor))
                },
            )
            .expect("mapping succeeds");

        let mut replay = Vec::new();
        mapped
            .read_to_end(&mut replay)
            .expect("replay remains available");
        assert_eq!(replay, legacy);

        mapped.write_all(b"payload").expect("writes propagate");
        mapped.flush().expect("flush propagates");
        assert_eq!(mapped.inner().writes(), b"payload");
        assert_eq!(mapped.inner().flushes(), 1);
    }

    #[test]
    fn try_map_inner_preserves_original_on_error() {
        let legacy = b"@RSYNCD: 31.0\n";
        let stream = sniff_bytes(legacy).expect("sniff succeeds");

        let err = stream
            .try_map_inner(
                |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                    Err((io::Error::other("boom"), cursor))
                },
            )
            .expect_err("mapping fails");

        assert_eq!(err.error().kind(), io::ErrorKind::Other);
        let mut original = err.into_original();
        let mut replay = Vec::new();
        original
            .read_to_end(&mut replay)
            .expect("original stream still readable");
        assert_eq!(replay, legacy);
    }

    #[test]
    fn try_map_inner_on_parts_transforms_transport() {
        let legacy = b"@RSYNCD: 31.0\nrest";
        let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();

        let mapped = parts
            .try_map_inner(
                |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                    Ok(RecordingTransport::from_cursor(cursor))
                },
            )
            .expect("mapping succeeds");

        let mut replay = Vec::new();
        mapped
            .into_stream()
            .read_to_end(&mut replay)
            .expect("stream reconstruction works");
        assert_eq!(replay, legacy);
    }

    #[test]
    fn try_map_inner_on_parts_preserves_original_on_error() {
        let legacy = b"@RSYNCD: 31.0\n";
        let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();

        let err = parts
            .try_map_inner(
                |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                    Err((io::Error::other("boom"), cursor))
                },
            )
            .expect_err("mapping fails");

        assert_eq!(err.error().kind(), io::ErrorKind::Other);
        let mut original = err.into_original().into_stream();
        let mut replay = Vec::new();
        original
            .read_to_end(&mut replay)
            .expect("original stream still readable");
        assert_eq!(replay, legacy);
    }

    #[test]
    fn try_map_inner_error_can_transform_error_without_losing_original() {
        let legacy = b"@RSYNCD: 31.0\n";
        let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();

        let err = parts
            .try_map_inner(
                |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                    Err((io::Error::other("boom"), cursor))
                },
            )
            .expect_err("mapping fails");

        let mapped = err.map_error(|error| error.kind());
        assert_eq!(*mapped.error(), io::ErrorKind::Other);

        let mut original = mapped.into_original().into_stream();
        let mut replay = Vec::new();
        original
            .read_to_end(&mut replay)
            .expect("mapped error preserves original stream");
        assert_eq!(replay, legacy);
    }

    #[test]
    fn try_map_inner_error_mut_accessors_preserve_state() {
        let legacy = b"@RSYNCD: 31.0\n";
        let stream = sniff_bytes(legacy).expect("sniff succeeds");

        let mut err = stream
            .try_map_inner(
                |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                    Err((io::Error::other("boom"), cursor))
                },
            )
            .expect_err("mapping fails");

        *err.error_mut() = io::Error::new(io::ErrorKind::TimedOut, "timeout");
        assert_eq!(err.error().kind(), io::ErrorKind::TimedOut);

        {
            let original = err.original_mut();
            let mut first = [0u8; 1];
            original
                .read_exact(&mut first)
                .expect("reading from preserved stream succeeds");
            assert_eq!(&first, b"@");
        }

        let mut restored = err.into_original();
        let mut replay = Vec::new();
        restored
            .read_to_end(&mut replay)
            .expect("mutations persist when recovering the original stream");
        assert_eq!(replay, &legacy[1..]);
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
        assert!(after_read_prefix.is_empty());
        assert_eq!(after_read_remainder, b"abc");

        let mut partial = [0u8; 2];
        stream
            .read_exact(&mut partial)
            .expect("read_exact drains part of the buffered remainder");
        assert_eq!(&partial, b"ab");
        assert_eq!(stream.buffered_remainder(), b"c");

        let mut final_byte = [0u8; 1];
        stream
            .read_exact(&mut final_byte)
            .expect("read_exact consumes the last buffered byte");
        assert_eq!(&final_byte, b"c");
        assert!(stream.buffered_remainder().is_empty());

        let (after_read_prefix, after_read_remainder) = stream.buffered_split();
        assert!(after_read_prefix.is_empty());
        assert!(after_read_remainder.is_empty());
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
    fn sniffed_stream_supports_writing_via_wrapper() {
        let transport = RecordingTransport::new(b"@RSYNCD: 31.0\nrest");
        let mut stream = sniff_negotiation_stream(transport).expect("sniff succeeds");

        stream
            .write_all(b"CLIENT\n")
            .expect("write forwards to inner transport");

        let vectored = [IoSlice::new(b"V1"), IoSlice::new(b"V2")];
        let written = stream
            .write_vectored(&vectored)
            .expect("vectored write forwards to inner transport");
        assert_eq!(written, 4);

        stream.flush().expect("flush forwards to inner transport");

        let mut line = Vec::new();
        stream
            .read_legacy_daemon_line(&mut line)
            .expect("legacy line remains readable after writes");
        assert_eq!(line, b"@RSYNCD: 31.0\n");

        let inner = stream.into_inner();
        assert_eq!(inner.writes(), b"CLIENT\nV1V2");
        assert_eq!(inner.flushes(), 1);
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
    fn parts_can_be_cloned_without_sharing_state() {
        let data = b"@RSYNCD: 30.0\nrest";
        let mut stream = sniff_bytes(data).expect("sniff succeeds");

        let mut prefix_fragment = [0u8; 3];
        stream
            .read_exact(&mut prefix_fragment)
            .expect("read_exact consumes part of the buffered prefix");
        assert_eq!(&prefix_fragment, b"@RS");

        let parts = stream.into_parts();
        let cloned = parts.clone();

        assert_eq!(cloned.decision(), parts.decision());
        assert_eq!(cloned.sniffed_prefix(), parts.sniffed_prefix());
        assert_eq!(cloned.buffered_remainder(), parts.buffered_remainder());
        assert_eq!(cloned.buffered_remaining(), parts.buffered_remaining());

        let mut original_stream = parts.into_stream();
        let mut original_replay = Vec::new();
        original_stream
            .read_to_end(&mut original_replay)
            .expect("original stream replays buffered bytes");

        let mut cloned_stream = cloned.into_stream();
        let mut cloned_replay = Vec::new();
        cloned_stream
            .read_to_end(&mut cloned_replay)
            .expect("cloned stream replays its buffered bytes");

        assert_eq!(original_replay, cloned_replay);
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
        assert_eq!(rehydrated_prefix, b"NCD:");
        assert_eq!(rehydrated_remainder, rehydrated.buffered_remainder());

        let mut remainder = Vec::new();
        rehydrated
            .read_to_end(&mut remainder)
            .expect("reconstructed stream yields the remaining bytes");
        assert_eq!(remainder, b"NCD: 29.0\nrest");
    }

    #[test]
    fn raw_parts_round_trip_binary_state() {
        let data = [0x00, 0x12, 0x34, 0x56];
        let stream = sniff_bytes(&data).expect("sniff succeeds");
        let expected_decision = stream.decision();
        assert_eq!(expected_decision, NegotiationPrologue::Binary);
        assert_eq!(stream.sniffed_prefix(), &[0x00]);

        let (decision, sniffed_prefix_len, buffered_pos, buffered, inner) = stream.into_raw_parts();
        assert_eq!(decision, expected_decision);
        assert_eq!(sniffed_prefix_len, 1);
        assert_eq!(buffered_pos, 0);
        assert_eq!(buffered, vec![0x00]);

        let mut reconstructed = NegotiatedStream::from_raw_parts(
            inner,
            decision,
            sniffed_prefix_len,
            buffered_pos,
            buffered,
        );
        let mut replay = Vec::new();
        reconstructed
            .read_to_end(&mut replay)
            .expect("reconstructed stream replays buffered prefix and remainder");
        assert_eq!(replay, data);
    }

    #[test]
    fn raw_parts_preserve_consumed_progress() {
        let data = b"@RSYNCD: 31.0\nrest";
        let mut stream = sniff_bytes(data).expect("sniff succeeds");
        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);

        let mut consumed = [0u8; 3];
        stream
            .read_exact(&mut consumed)
            .expect("prefix consumption succeeds");
        assert_eq!(&consumed, b"@RS");

        let (decision, sniffed_prefix_len, buffered_pos, buffered, inner) = stream.into_raw_parts();
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(sniffed_prefix_len, LEGACY_DAEMON_PREFIX_LEN);
        assert_eq!(buffered_pos, consumed.len());
        assert_eq!(buffered, b"@RSYNCD:".to_vec());

        let mut reconstructed = NegotiatedStream::from_raw_parts(
            inner,
            decision,
            sniffed_prefix_len,
            buffered_pos,
            buffered,
        );
        let mut remainder = Vec::new();
        reconstructed
            .read_to_end(&mut remainder)
            .expect("reconstructed stream resumes after consumed prefix");
        assert_eq!(remainder, b"YNCD: 31.0\nrest");

        let mut combined = Vec::new();
        combined.extend_from_slice(&consumed);
        combined.extend_from_slice(&remainder);
        assert_eq!(combined, data);
    }

    #[test]
    fn raw_parts_round_trip_legacy_state() {
        let data = b"@RSYNCD: 32.0\nrest";
        let stream = sniff_bytes(data).expect("sniff succeeds");
        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
        assert_eq!(stream.sniffed_prefix(), b"@RSYNCD:");

        let (decision, sniffed_prefix_len, buffered_pos, buffered, inner) = stream.into_raw_parts();
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(sniffed_prefix_len, LEGACY_DAEMON_PREFIX_LEN);
        assert_eq!(buffered_pos, 0);
        assert_eq!(buffered, b"@RSYNCD:".to_vec());

        let mut reconstructed = NegotiatedStream::from_raw_parts(
            inner,
            decision,
            sniffed_prefix_len,
            buffered_pos,
            buffered,
        );
        let mut replay = Vec::new();
        reconstructed
            .read_to_end(&mut replay)
            .expect("reconstructed stream replays full buffer");
        assert_eq!(replay, data);
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
    fn read_and_parse_legacy_daemon_message_after_greeting() {
        let mut stream =
            sniff_bytes(b"@RSYNCD: 31.0\n@RSYNCD: AUTHREQD module\n@ERROR: access denied\n")
                .expect("sniff succeeds");

        let mut line = Vec::new();
        let version = stream
            .read_and_parse_legacy_daemon_greeting(&mut line)
            .expect("greeting parses");
        let expected = ProtocolVersion::from_supported(31).expect("supported version");
        assert_eq!(version, expected);
        assert_eq!(line, b"@RSYNCD: 31.0\n");

        let message = stream
            .read_and_parse_legacy_daemon_message(&mut line)
            .expect("message parses");
        match message {
            LegacyDaemonMessage::AuthRequired { module } => {
                assert_eq!(module, Some("module"));
            }
            other => panic!("unexpected message: {other:?}"),
        }
        assert_eq!(line, b"@RSYNCD: AUTHREQD module\n");

        let error = stream
            .read_and_parse_legacy_daemon_error_message(&mut line)
            .expect("error parses")
            .expect("payload present");
        assert_eq!(error, "access denied");
        assert_eq!(line, b"@ERROR: access denied\n");
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
    fn read_legacy_daemon_line_errors_for_incomplete_prefix_state() {
        let mut stream = NegotiatedStream::from_raw_parts(
            Cursor::new(b" 31.0\n".to_vec()),
            NegotiationPrologue::LegacyAscii,
            LEGACY_DAEMON_PREFIX_LEN - 1,
            0,
            b"@RSYNCD".to_vec(),
        );

        let mut line = Vec::new();
        let err = stream
            .read_legacy_daemon_line(&mut line)
            .expect_err("incomplete prefix must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(line.is_empty());
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
    fn read_and_parse_legacy_daemon_message_errors_when_prefix_partially_consumed() {
        let mut stream = sniff_bytes(b"@RSYNCD: AUTHREQD module\n").expect("sniff succeeds");
        let mut prefix_fragment = [0u8; 3];
        stream
            .read_exact(&mut prefix_fragment)
            .expect("prefix fragment is replayed before parsing");

        let mut line = Vec::new();
        let err = stream
            .read_and_parse_legacy_daemon_message(&mut line)
            .expect_err("partial prefix consumption must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(line.is_empty());
    }

    #[test]
    fn read_and_parse_legacy_daemon_message_clears_line_on_error() {
        let mut stream = sniff_bytes(b"\x00rest").expect("sniff succeeds");
        let mut line = b"stale".to_vec();

        let err = stream
            .read_and_parse_legacy_daemon_message(&mut line)
            .expect_err("binary negotiation cannot parse legacy message");

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(line.is_empty());
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
