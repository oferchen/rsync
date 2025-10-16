use core::{fmt, mem, slice};
use std::collections::TryReserveError;
use std::io::{self, Read, Write};

use crate::legacy::{LEGACY_DAEMON_PREFIX_LEN, parse_legacy_daemon_greeting_bytes};
use crate::version::ProtocolVersion;

use super::{BufferedPrefixTooSmall, NegotiationPrologue, NegotiationPrologueDetector};

/// Incrementally reads bytes from a [`Read`] implementation until the
/// negotiation style can be determined.
///
/// Upstream rsync only needs to observe the very first octet to decide between
/// the legacy ASCII negotiation (`@RSYNCD:`) and the modern binary handshake.
/// Real transports, however, may deliver that byte in small fragments or after
/// transient `EINTR` interruptions. This helper mirrors upstream behavior while
/// providing a higher level interface that owns the buffered prefix so callers
/// can replay the bytes into the legacy greeting parser without reallocating.
///
/// # Examples
///
/// ```
/// use rsync_protocol::{NegotiationPrologue, NegotiationPrologueSniffer};
/// use std::io::Cursor;
///
/// let mut sniffer = NegotiationPrologueSniffer::new();
/// let mut reader = Cursor::new(&b"@RSYNCD: 31.0\n"[..]);
/// let decision = sniffer
///     .read_from(&mut reader)
///     .expect("legacy negotiation detection succeeds");
///
/// assert_eq!(decision, NegotiationPrologue::LegacyAscii);
/// assert_eq!(sniffer.buffered(), b"@RSYNCD:");
/// ```
#[derive(Clone, Debug)]
pub struct NegotiationPrologueSniffer {
    detector: NegotiationPrologueDetector,
    buffered: Vec<u8>,
    prefix_bytes_retained: usize,
}

impl NegotiationPrologueSniffer {
    /// Creates a sniffer with an empty buffer and undecided negotiation state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a sniffer that reuses the caller-provided buffer for prefix storage.
    ///
    /// The allocation is cleared and its capacity is normalized to the canonical
    /// legacy prefix length so the resulting sniffer mirrors the behavior of
    /// [`Self::new`]. This mirrors upstream rsync's approach of recycling fixed-size
    /// storage for the `@RSYNCD:` marker while avoiding unnecessary allocations when a
    /// pooling layer already owns reusable buffers. The returned sniffer starts in the
    /// undecided state just like [`Self::new`].
    #[must_use]
    pub fn with_buffer(buffer: Vec<u8>) -> Self {
        let mut sniffer = Self {
            detector: NegotiationPrologueDetector::new(),
            buffered: buffer,
            prefix_bytes_retained: 0,
        };
        sniffer.reset();
        sniffer
    }

    /// Returns the buffered bytes that were consumed while detecting the
    /// negotiation style.
    #[must_use]
    pub fn buffered(&self) -> &[u8] {
        &self.buffered
    }

    /// Returns the bytes buffered beyond the sniffed negotiation prefix.
    ///
    /// The slice excludes the canonical prefix captured while detecting the
    /// negotiation style, mirroring upstream rsync's behavior where the peeked
    /// bytes are replayed before processing additional payload. Once the prefix
    /// has been drained—via [`take_buffered`](Self::take_buffered) or one of its
    /// variants—the returned slice covers the entire buffered remainder.
    #[must_use]
    pub fn buffered_remainder(&self) -> &[u8] {
        let prefix_len = self.sniffed_prefix_len();
        debug_assert!(prefix_len <= self.buffered.len());
        &self.buffered[prefix_len..]
    }

    /// Reports whether the negotiation style has been determined.
    ///
    /// The return value mirrors [`NegotiationPrologue::is_decided`] and becomes `true` as soon as
    /// the initial byte rules out the undecided state. For legacy negotiations this happens when
    /// the leading `@` byte is observed even if additional prefix bytes still need to be buffered
    /// before the greeting parser can run. Callers that need to know whether more I/O is required
    /// can pair this with [`requires_more_data`](Self::requires_more_data).
    #[must_use]
    pub fn is_decided(&self) -> bool {
        self.detector
            .decision()
            .is_some_and(NegotiationPrologue::is_decided)
    }

    /// Returns `true` when additional bytes must be read before the handshake can progress.
    ///
    /// New connections start in a pending state, so the method initially returns `true`. Once the
    /// first byte arrives, binary negotiations are considered complete and the function flips to
    /// `false`. For legacy exchanges it keeps returning `true` until the canonical `@RSYNCD:` prefix
    /// has been fully buffered, mirroring the behavior of [`read_from`](Self::read_from) which keeps
    /// pulling data until the legacy marker can be replayed.
    #[must_use]
    pub fn requires_more_data(&self) -> bool {
        match self.detector.decision() {
            Some(NegotiationPrologue::LegacyAscii) => !self.detector.legacy_prefix_complete(),
            Some(NegotiationPrologue::Binary) => false,
            Some(NegotiationPrologue::NeedMoreData) => true,
            None => true,
        }
    }

    /// Returns the number of bytes retained while sniffing the negotiation prologue.
    ///
    /// The total includes any additional data that was pulled from the transport while deciding
    /// between the binary and legacy ASCII handshakes. When [`read_from`](Self::read_from) reads a
    /// chunk that extends beyond the canonical `@RSYNCD:` marker, the excess bytes are preserved so
    /// higher layers can replay them without re-reading from the transport. Callers that only need
    /// the number of bytes consumed for the prefix itself (excluding the buffered remainder) can
    /// use [`sniffed_prefix_len`](Self::sniffed_prefix_len).
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    /// Returns the number of bytes that were required to classify the negotiation prologue.
    ///
    /// The value mirrors [`NegotiationPrologueDetector::buffered_len`], allowing callers to
    /// distinguish between the canonical prefix that must be replayed and any additional payload
    /// preserved by [`buffered_len`](Self::buffered_len). If the buffered prefix has already been
    /// drained the helper reports `0`, mirroring upstream's behavior where no bytes remain queued
    /// for replay. When the exchange selects the binary protocol this yields the number of bytes
    /// that triggered the decision (typically `1`).
    #[must_use]
    pub fn sniffed_prefix_len(&self) -> usize {
        match self.detector.decision() {
            Some(NegotiationPrologue::Binary) => self.buffered.len().min(1),
            Some(NegotiationPrologue::LegacyAscii)
            | Some(NegotiationPrologue::NeedMoreData)
            | None => self.prefix_bytes_retained.min(self.buffered.len()),
        }
    }

    /// Returns the bytes that were required to classify the negotiation prologue.
    ///
    /// The returned slice is limited to the canonical prefix captured while deciding between the
    /// legacy ASCII (`@RSYNCD:`) and binary negotiations. Any additional payload buffered by the
    /// sniffer—such as trailing data that arrived in the same read—is excluded so callers can
    /// operate on the detection prefix without trimming the backing allocation themselves. The
    /// slice remains valid for as long as the sniffer is alive and is typically paired with
    /// [`sniffed_prefix_len`](Self::sniffed_prefix_len) when replaying the prefix into the legacy
    /// greeting parser.
    #[must_use]
    pub fn sniffed_prefix(&self) -> &[u8] {
        let prefix_len = self.sniffed_prefix_len();
        debug_assert!(prefix_len <= self.buffered.len());
        &self.buffered[..prefix_len]
    }

    #[cfg(test)]
    pub(crate) fn buffered_storage(&self) -> &Vec<u8> {
        &self.buffered
    }

    #[cfg(test)]
    pub(crate) fn buffered_storage_mut(&mut self) -> &mut Vec<u8> {
        &mut self.buffered
    }

    /// Consumes the sniffer and returns the owned buffer containing the bytes
    /// that were read while determining the negotiation style.
    ///
    /// The returned allocation is trimmed to the canonical legacy prefix
    /// length so callers never inherit oversized buffers that may have been
    /// required while parsing malformed greetings. This mirrors the
    /// shrink-to-fit behavior provided by [`take_buffered`](Self::take_buffered)
    /// and keeps the helper suitable for long-lived connection pools.
    #[must_use = "the drained negotiation prefix must be replayed"]
    pub fn into_buffered(mut self) -> Vec<u8> {
        if self.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN {
            self.buffered.shrink_to(LEGACY_DAEMON_PREFIX_LEN);
        }

        self.buffered
    }

    /// Drains the buffered bytes while keeping the sniffer available for reuse.
    ///
    /// Callers that need to replay the captured prefix into the legacy greeting
    /// parser (or feed the initial binary byte back into the negotiation
    /// handler) can drain the buffer without relinquishing ownership of the
    /// sniffer. The internal storage is replaced with an empty vector whose
    /// capacity is capped at the canonical legacy prefix length so subsequent
    /// detections do not retain unbounded allocations while still satisfying the
    /// workspace's buffer reuse guidance.
    #[must_use = "the drained negotiation prefix must be replayed"]
    pub fn take_buffered(&mut self) -> Vec<u8> {
        let target_capacity = self.buffered.capacity().min(LEGACY_DAEMON_PREFIX_LEN);
        let mut drained = Vec::with_capacity(target_capacity);
        mem::swap(&mut self.buffered, &mut drained);
        self.reset_buffer_for_reuse();

        // Defensively cap the returned capacity at the canonical prefix length so callers never
        // retain an excessively large allocation even if the sniffer previously observed a
        // malformed banner that forced the buffer to grow. The buffered length never exceeds the
        // prefix length, making the shrink operation a no-op for successful detections while
        // mirroring upstream's fixed-size peek storage.
        drained.shrink_to(LEGACY_DAEMON_PREFIX_LEN);

        drained
    }

    /// Drains the buffered bytes into an existing vector supplied by the caller.
    ///
    /// The helper mirrors [`take_buffered`] but avoids allocating a new vector when the
    /// caller already owns a reusable buffer. The destination vector is cleared before the
    /// captured prefix is copied into it, ensuring the slice matches the bytes that were
    /// consumed during negotiation sniffing. The returned length mirrors the number of bytes
    /// that were replayed into `target`, keeping the API consistent with the I/O traits used
    /// throughout the transport layer. After the transfer the sniffer retains an empty
    /// buffer whose capacity is clamped to the canonical legacy prefix length so repeated
    /// connections continue to benefit from buffer reuse. If growing the destination buffer
    /// fails, the allocation error is forwarded to the caller instead of panicking so the
    /// transport layer can surface the failure as an I/O error. To avoid surprising the
    /// caller, the existing contents of `target` are only cleared after the reservation
    /// succeeds, mirroring upstream's failure semantics where buffers remain untouched when
    /// memory is exhausted.
    #[must_use = "negotiation prefix length is required to replay the handshake"]
    pub fn take_buffered_into(&mut self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        let required = self.buffered.len();

        if target.capacity() < required {
            // `Vec::try_reserve_exact` interprets the requested value as additional
            // elements beyond the current *length*, not the spare capacity. When the
            // caller hands us a vector that already stores data, reserving the
            // difference between the required length and the existing capacity would
            // therefore undershoot the amount of space we need. The subsequent
            // `extend_from_slice` would then have to grow the vector again, which in
            // turn panics on allocation failure instead of surfacing a
            // `TryReserveError` to the caller. Reserving relative to the vector's
            // length guarantees the resulting capacity can hold the replayed prefix
            // without further allocations. Using `saturating_sub` keeps the helper
            // resilient if future call sites accidentally invoke it with a vector
            // whose length already exceeds the buffered prefix; in that scenario we
            // simply skip the reservation instead of panicking on an underflowing
            // subtraction.
            debug_assert!(target.len() < required);
            let additional = required.saturating_sub(target.len());
            if additional > 0 {
                target.try_reserve_exact(additional)?;
            }
        }
        target.clear();
        target.extend_from_slice(&self.buffered);
        let drained = target.len();
        self.reset_buffer_for_reuse();

        Ok(drained)
    }

    /// Drains the buffered bytes into the caller-provided slice without allocating.
    ///
    /// The helper mirrors [`take_buffered_into`] but writes the captured prefix directly into
    /// `target`, allowing callers with stack-allocated storage to replay the negotiation prologue
    /// without constructing a temporary [`Vec`]. When `target` is too small to hold the buffered
    /// prefix a [`BufferedPrefixTooSmall`] error is returned and the internal buffer remains
    /// untouched so the caller can retry after resizing their storage.
    #[must_use = "negotiation prefix length is required to replay the handshake"]
    pub fn take_buffered_into_slice(
        &mut self,
        target: &mut [u8],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        let required = self.buffered.len();
        if target.len() < required {
            return Err(BufferedPrefixTooSmall::new(required, target.len()));
        }

        target[..required].copy_from_slice(&self.buffered);
        self.reset_buffer_for_reuse();

        Ok(required)
    }

    /// Drains the buffered bytes into an array supplied by the caller without allocating.
    ///
    /// This is a convenience wrapper around
    /// [`take_buffered_into_slice`](Self::take_buffered_into_slice) that accepts a
    /// [`[u8; N]`](array) directly. Callers that keep a stack-allocated
    /// `LEGACY_DAEMON_PREFIX_LEN` scratch buffer can therefore pass it without converting to a
    /// slice at every call site. Just like the slice variant the helper returns the number of
    /// bytes copied and leaves the internal buffer untouched when the array is too small so the
    /// operation can be retried after provisioning a larger workspace.
    #[must_use = "negotiation prefix length is required to replay the handshake"]
    pub fn take_buffered_into_array<const N: usize>(
        &mut self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        self.take_buffered_into_slice(target.as_mut_slice())
    }

    /// Drains the buffered bytes into an arbitrary [`Write`] implementation without allocating.
    ///
    /// The helper mirrors [`take_buffered_into_slice`](Self::take_buffered_into_slice) but hands
    /// the captured prefix directly to a writer supplied by the caller. This is particularly
    /// useful for transports that forward the sniffed bytes into an in-flight I/O buffer or a
    /// [`Vec<u8>`](Vec) managed by a pooling layer. When writing succeeds the sniffer is reset for
    /// reuse while preserving the canonical capacity used for the legacy prefix. Should the writer
    /// report an error, the buffered bytes remain intact so the caller can retry or surface the
    /// failure.
    #[must_use = "negotiation prefix length is required to replay the handshake"]
    pub fn take_buffered_into_writer<W: Write>(&mut self, target: &mut W) -> io::Result<usize> {
        target.write_all(&self.buffered)?;
        let written = self.buffered.len();
        self.reset_buffer_for_reuse();

        Ok(written)
    }

    /// Reports the cached negotiation decision, if any.
    #[must_use]
    pub fn decision(&self) -> Option<NegotiationPrologue> {
        self.detector.decision()
    }

    /// Returns `true` when the sniffer has determined that the peer selected the legacy
    /// ASCII negotiation.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_legacy`] while accounting for the
    /// fact that the decision may still be pending. Callers that only need a boolean view
    /// can rely on this method instead of matching on [`Self::decision`]. The return value
    /// stays `true` even while the canonical `@RSYNCD:` prefix is still being buffered,
    /// matching upstream rsync's behavior where the negotiation style is considered decided
    /// as soon as the leading `@` byte is observed.
    #[must_use]
    pub fn is_legacy(&self) -> bool {
        self.detector.is_legacy()
    }

    /// Returns `true` when the sniffer has determined that the peer selected the binary
    /// negotiation path.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_binary`] while tolerating undecided
    /// states. It becomes `true` as soon as the initial byte rules out the legacy ASCII
    /// prefix, ensuring higher layers can react immediately without waiting for additional
    /// I/O.
    #[must_use]
    pub fn is_binary(&self) -> bool {
        self.detector.is_binary()
    }

    /// Observes bytes that have already been read from the transport while tracking how
    /// many of them were required to determine the negotiation style.
    ///
    /// Callers that perform buffered reads or speculative peeks can forward the captured
    /// bytes to this helper instead of re-reading from the underlying transport. The sniffer
    /// mirrors [`NegotiationPrologueDetector::observe`] by consuming bytes until a definitive
    /// decision is available or the canonical legacy prefix (`@RSYNCD:`) has been fully
    /// buffered. Until that prefix has been captured, the returned decision is
    /// [`NegotiationPrologue::NeedMoreData`] even if the detector has already determined that the
    /// exchange uses the legacy ASCII handshake. Callers that need to know how many additional
    /// bytes are required can query [`legacy_prefix_remaining`](Self::legacy_prefix_remaining).
    /// Any remaining data in `chunk` is left untouched so higher layers can process it according
    /// to the negotiated protocol. If reserving capacity for the buffered prefix fails, the
    /// allocation error is surfaced instead of panicking so higher layers can convert it into an
    /// [`io::Error`].
    #[must_use = "process the negotiation decision and potential allocation error"]
    pub fn observe(
        &mut self,
        chunk: &[u8],
    ) -> Result<(NegotiationPrologue, usize), TryReserveError> {
        let cached = self.detector.decision();
        let needs_more_prefix_bytes =
            cached.is_some_and(|decision| self.needs_more_legacy_prefix_bytes(decision));

        if chunk.is_empty() {
            if needs_more_prefix_bytes {
                return Ok((NegotiationPrologue::NeedMoreData, 0));
            }

            return Ok((cached.unwrap_or(NegotiationPrologue::NeedMoreData), 0));
        }

        if let Some(decision) = cached.filter(|_| !needs_more_prefix_bytes) {
            return Ok((decision, 0));
        }

        let planned = self.planned_prefix_bytes_for_observation(cached, chunk.len());
        if planned > 0 {
            self.buffered.try_reserve(planned)?;
        }

        let mut consumed = 0;

        for &byte in chunk {
            self.buffered.push(byte);
            consumed += 1;

            let decision = self.detector.observe_byte(byte);
            self.update_prefix_retention();
            let needs_more_prefix_bytes = self.needs_more_legacy_prefix_bytes(decision);

            if decision != NegotiationPrologue::NeedMoreData && !needs_more_prefix_bytes {
                return Ok((decision, consumed));
            }
        }

        let final_decision = self.detector.decision();
        if final_decision.is_some_and(|decision| self.needs_more_legacy_prefix_bytes(decision)) {
            Ok((NegotiationPrologue::NeedMoreData, consumed))
        } else {
            Ok((
                final_decision.unwrap_or(NegotiationPrologue::NeedMoreData),
                consumed,
            ))
        }
    }

    /// Observes a single byte that has already been read from the transport.
    ///
    /// The helper mirrors [`observe`](Self::observe) but keeps the common
    /// "one-octet-at-a-time" call pattern used by upstream rsync ergonomic.
    /// Callers can therefore forward individual bytes without allocating a
    /// temporary slice. The returned result mirrors [`observe`](Self::observe):
    /// on success it yields the negotiation decision while ensuring at most a
    /// single byte is accounted for as consumed, and any allocation failure is
    /// surfaced instead of panicking.
    #[must_use = "process the negotiation decision or surface allocation failures"]
    #[inline]
    pub fn observe_byte(&mut self, byte: u8) -> Result<NegotiationPrologue, TryReserveError> {
        let (decision, consumed) = self.observe(slice::from_ref(&byte))?;
        debug_assert!(consumed <= 1);
        Ok(decision)
    }

    /// Clears the buffered prefix and resets the negotiation detector so the
    /// sniffer can be reused for another connection attempt.
    ///
    /// The internal buffer retains its allocation when it already matches the
    /// canonical legacy prefix length so that back-to-back legacy negotiations
    /// do not pay repeated allocations. If the buffer had previously grown
    /// beyond that size—for instance when an attacker sent a very large
    /// malformed banner before the session was aborted—the capacity is trimmed
    /// back to the prefix length to avoid carrying an unnecessarily large
    /// allocation into subsequent connections. Conversely, if an earlier
    /// operation shrank the allocation below the canonical size, the buffer is
    /// grown back to the prefix length so future legacy negotiations do not
    /// trigger repeated incremental reallocations while replaying the prefix.
    pub fn reset(&mut self) {
        self.detector.reset();
        self.reset_buffer_for_reuse();
    }

    /// Reads from `reader` until the negotiation style can be determined.
    ///
    /// Bytes consumed during detection are appended to the internal buffer so
    /// callers can replay them into the legacy greeting parser if necessary.
    /// Once a decision has been cached, subsequent calls return immediately
    /// without performing additional I/O **unless** the exchange has been
    /// classified as legacy ASCII and the canonical `@RSYNCD:` prefix still
    /// needs to be buffered. This mirrors upstream rsync, which keeps reading
    /// until the marker has been captured so the greeting parser can reuse the
    /// already consumed bytes.
    pub fn read_from<R: Read>(&mut self, reader: &mut R) -> io::Result<NegotiationPrologue> {
        match self.detector.decision() {
            Some(decision) if !self.needs_more_legacy_prefix_bytes(decision) => {
                return Ok(decision);
            }
            _ => {}
        }

        let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN];

        loop {
            let cached = self.detector.decision();
            let needs_more_prefix_bytes =
                cached.is_some_and(|decision| self.needs_more_legacy_prefix_bytes(decision));
            if let Some(decision) = cached.filter(|_| !needs_more_prefix_bytes) {
                return Ok(decision);
            }

            let bytes_to_read = if needs_more_prefix_bytes {
                LEGACY_DAEMON_PREFIX_LEN - self.detector.buffered_len()
            } else {
                1
            };

            match reader.read(&mut scratch[..bytes_to_read]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed before rsync negotiation prologue was determined",
                    ));
                }
                Ok(read) => {
                    let observed = &scratch[..read];
                    let (decision, consumed) =
                        self.observe(observed).map_err(map_reserve_error_for_io)?;
                    debug_assert!(consumed <= observed.len());

                    if consumed < observed.len() {
                        let remainder = &observed[consumed..];
                        if !remainder.is_empty() {
                            self.buffered
                                .try_reserve_exact(remainder.len())
                                .map_err(map_reserve_error_for_io)?;
                            self.buffered.extend_from_slice(remainder);
                        }
                    }
                    let needs_more_prefix_bytes = self.needs_more_legacy_prefix_bytes(decision);
                    if decision != NegotiationPrologue::NeedMoreData && !needs_more_prefix_bytes {
                        return Ok(decision);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }

    /// Reports whether the canonical legacy prefix (`@RSYNCD:`) has already
    /// been fully observed.
    ///
    /// Legacy ASCII negotiations reuse the bytes captured during detection when
    /// parsing the daemon greeting. Higher layers therefore need to know when
    /// the marker has been buffered or ruled out so they can decide whether to
    /// keep reading from the transport before handing the accumulated bytes to
    /// the legacy greeting parser. The helper simply forwards to
    /// [`NegotiationPrologueDetector::legacy_prefix_complete`], keeping the
    /// sniffer's API in sync with the lower-level detector without exposing the
    /// internal field directly.
    #[must_use]
    pub fn legacy_prefix_complete(&self) -> bool {
        self.detector.legacy_prefix_complete()
    }

    /// Reports how many additional bytes are still required to finish buffering
    /// the canonical legacy prefix.
    ///
    /// When the detector has already classified the stream as legacy ASCII but
    /// the full `@RSYNCD:` prefix has not yet been captured, callers can use the
    /// returned count to decide whether another read is necessary before
    /// replaying the buffered bytes into the legacy greeting parser. Once the
    /// prefix has been fully observed—or when the exchange is binary—the helper
    /// yields `None`, mirroring
    /// [`NegotiationPrologueDetector::legacy_prefix_remaining`].
    #[must_use]
    pub fn legacy_prefix_remaining(&self) -> Option<usize> {
        self.detector.legacy_prefix_remaining()
    }

    #[inline]
    fn needs_more_legacy_prefix_bytes(&self, decision: NegotiationPrologue) -> bool {
        decision == NegotiationPrologue::LegacyAscii && !self.detector.legacy_prefix_complete()
    }

    fn reset_buffer_for_reuse(&mut self) {
        // Clearing always happens first so subsequent capacity adjustments observe the canonical
        // empty-length state expected by `shrink_to` and `reserve_exact`.
        self.buffered.clear();
        self.prefix_bytes_retained = 0;

        if self.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN {
            // Trim oversized allocations that may have been introduced when parsing malformed
            // banners. `shrink_to` keeps the existing buffer when possible so we only fall back to
            // a new allocation if the allocator cannot downsize in place.
            self.buffered.shrink_to(LEGACY_DAEMON_PREFIX_LEN);
        }

        if self.buffered.capacity() < LEGACY_DAEMON_PREFIX_LEN {
            // Grow undersized buffers back to the canonical prefix length. This mirrors upstream
            // rsync's fixed-size stack storage and avoids repeated incremental reallocations when
            // the sniffer is reused across connections. Reserving space for the full prefix in one
            // step guarantees the resulting capacity can store `@RSYNCD:` without further
            // allocations even when the previous buffer had already shrunk below the target size.
            let required = LEGACY_DAEMON_PREFIX_LEN.saturating_sub(self.buffered.len());
            if required > 0 {
                self.buffered.reserve_exact(required);
            }
        }
    }

    #[inline]
    fn planned_prefix_bytes_for_observation(
        &self,
        cached: Option<NegotiationPrologue>,
        chunk_len: usize,
    ) -> usize {
        if chunk_len == 0 {
            return 0;
        }

        let buffered_prefix = self.detector.buffered_len();
        match cached {
            Some(NegotiationPrologue::Binary) => 0,
            Some(NegotiationPrologue::LegacyAscii) => {
                chunk_len.min(LEGACY_DAEMON_PREFIX_LEN.saturating_sub(buffered_prefix))
            }
            Some(NegotiationPrologue::NeedMoreData) | None => {
                let remaining = LEGACY_DAEMON_PREFIX_LEN
                    .saturating_sub(buffered_prefix)
                    .max(1);
                chunk_len.min(remaining)
            }
        }
    }

    fn update_prefix_retention(&mut self) {
        self.prefix_bytes_retained = match self.detector.decision() {
            Some(NegotiationPrologue::LegacyAscii) | Some(NegotiationPrologue::NeedMoreData) => {
                self.detector
                    .buffered_len()
                    .min(self.buffered.len())
                    .min(LEGACY_DAEMON_PREFIX_LEN)
            }
            Some(NegotiationPrologue::Binary) | None => 0,
        };
    }
}

impl Default for NegotiationPrologueSniffer {
    fn default() -> Self {
        Self {
            detector: NegotiationPrologueDetector::new(),
            buffered: Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN),
            prefix_bytes_retained: 0,
        }
    }
}

/// Reads the complete legacy daemon line after the `@RSYNCD:` prefix has been buffered.
///
/// The sniffer must already have classified the exchange as legacy ASCII and captured the
/// canonical prefix. The buffered bytes are drained into `line`, after which additional data is
/// read from `reader` until a newline (`\n`) byte is encountered. Short reads and `EINTR`
/// interruptions are retried automatically. If the stream closes before a newline is observed,
/// [`io::ErrorKind::UnexpectedEof`] is returned. Invoking the helper before the negotiation style is
/// known (or when the peer is speaking the binary protocol) yields
/// [`io::ErrorKind::InvalidInput`].
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

    if let Some(newline_index) = line.iter().position(|&byte| byte == b'\n') {
        let remainder = line.split_off(newline_index + 1);
        if !remainder.is_empty() {
            sniffer
                .buffered
                .try_reserve_exact(remainder.len())
                .map_err(map_reserve_error_for_io)?;
            sniffer.buffered.extend_from_slice(&remainder);
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
///
/// The helper combines [`read_legacy_daemon_line`] with
/// [`parse_legacy_daemon_greeting_bytes`](crate::parse_legacy_daemon_greeting_bytes) so callers
/// can obtain the negotiated protocol version without manually wiring the intermediate buffer.
/// It assumes that [`NegotiationPrologueSniffer::read_from`] has already classified the exchange
/// as legacy ASCII and captured the canonical `@RSYNCD:` prefix. I/O failures are returned as
/// [`io::Error`] values, while malformed greetings propagate
/// [`NegotiationError`](crate::NegotiationError) via the same conversion used by the rest of the
/// crate.
pub fn read_and_parse_legacy_daemon_greeting<R: Read>(
    sniffer: &mut NegotiationPrologueSniffer,
    reader: &mut R,
    line: &mut Vec<u8>,
) -> io::Result<ProtocolVersion> {
    read_legacy_daemon_line(sniffer, reader, line)?;
    parse_legacy_daemon_greeting_bytes(line).map_err(io::Error::from)
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

impl fmt::Display for LegacyBufferReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
