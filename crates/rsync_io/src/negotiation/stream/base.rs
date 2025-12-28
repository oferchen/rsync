use std::io;
use std::vec::Vec;

use protocol::NegotiationPrologue;

use super::super::parts::{NegotiatedStreamParts, TryMapInnerError};
use super::super::{NegotiationBuffer, NegotiationBufferAccess};

/// Result produced when sniffing the negotiation prologue from a transport stream.
///
/// The structure owns the underlying reader together with the bytes that were
/// consumed while determining whether the peer speaks the legacy ASCII
/// `@RSYNCD:` protocol or the binary negotiation introduced in protocol 30. The
/// buffered data is replayed before any further reads from the inner stream,
/// mirroring upstream rsync's behavior where the detection prefix is fed back
/// into the parsing logic.
///
/// When the inner reader implements [`Clone`], the entire [`NegotiatedStream`]
/// can be cloned. The clone retains the buffered negotiation bytes and replay
/// cursor so both instances continue independently—matching upstream helpers
/// that occasionally need to inspect the handshake transcript while preserving
/// the original transport for continued use.
#[derive(Clone, Debug)]
pub struct NegotiatedStream<R> {
    inner: R,
    decision: NegotiationPrologue,
    buffer: NegotiationBuffer,
}

pub const NEGOTIATION_PROLOGUE_UNDETERMINED_MSG: &str =
    "connection closed before rsync negotiation prologue was determined";

impl<R> NegotiationBufferAccess for NegotiatedStream<R> {
    #[inline]
    fn buffer_ref(&self) -> &NegotiationBuffer {
        &self.buffer
    }
}

impl<R> NegotiatedStream<R> {
    /// Returns the negotiation style determined while sniffing the transport.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        self.decision
    }

    /// Reports whether the sniffed negotiation selected the binary protocol.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_binary`] while avoiding the
    /// need for callers to inspect [`Self::decision`] directly. Binary sessions
    /// correspond to remote-shell style negotiations introduced in protocol 30.
    /// When the stream was negotiated through the legacy ASCII daemon flow the
    /// method returns `false`.
    #[must_use]
    pub const fn is_binary(&self) -> bool {
        self.decision.is_binary()
    }

    /// Reports whether the sniffed negotiation selected the legacy ASCII protocol.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_legacy`] so higher layers can
    /// branch on the handshake style without matching on [`Self::decision`]. The
    /// method returns `true` when the transport presented the canonical
    /// `@RSYNCD:` prefix and `false` for binary negotiations.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        self.decision.is_legacy()
    }

    /// Ensures the sniffed negotiation matches the expected style.
    ///
    /// The helper mirrors the checks performed by the binary and legacy
    /// handshake wrappers. When the sniffed style matches the expectation the
    /// call succeeds. If the negotiation remains undecided it returns
    /// [`io::ErrorKind::UnexpectedEof`] with the canonical transport error
    /// message. Otherwise it produces an [`io::ErrorKind::InvalidData`] error
    /// with the supplied message. Centralising the logic keeps the error
    /// strings used across the transport crate in sync and avoids drift when
    /// additional call sites are introduced.
    pub fn ensure_decision(
        &self,
        expected: NegotiationPrologue,
        error_message: &'static str,
    ) -> io::Result<()> {
        match self.decision {
            decision if decision == expected => Ok(()),
            NegotiationPrologue::NeedMoreData => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                NEGOTIATION_PROLOGUE_UNDETERMINED_MSG,
            )),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, error_message)),
        }
    }

    /// Provides shared access to the buffered negotiation storage.
    #[must_use]
    pub(crate) const fn buffer_storage(&self) -> &NegotiationBuffer {
        &self.buffer
    }

    /// Provides mutable access to the buffered negotiation storage.
    #[must_use]
    pub(crate) const fn buffer_storage_mut(&mut self) -> &mut NegotiationBuffer {
        &mut self.buffer
    }

    /// Consumes replay bytes from the buffered negotiation transcript.
    pub(crate) fn consume_buffered(&mut self, amount: usize) -> usize {
        self.buffer.consume(amount)
    }

    /// Releases the wrapper and returns its components.
    ///
    /// The conversion can also be performed via [`From`] and [`Into`], enabling
    /// callers to decompose the replaying stream without invoking this method
    /// directly.
    #[must_use]
    pub fn into_parts(self) -> NegotiatedStreamParts<R> {
        NegotiatedStreamParts::new(self.decision, self.buffer, self.inner)
    }

    /// Releases the wrapper and returns the raw negotiation components together with the inner reader.
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
    pub const fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Releases the wrapper and returns the inner reader.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Transforms the inner reader while preserving the buffered negotiation state.
    #[must_use]
    pub fn map_inner<F, T>(self, map: F) -> NegotiatedStream<T>
    where
        F: FnOnce(R) -> T,
    {
        self.into_parts().map_inner(map).into_stream()
    }

    /// Attempts to transform the inner reader while keeping the buffered negotiation state intact.
    #[must_use = "the result contains either the mapped stream or the preserved error and original stream"]
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

    /// Clones the replaying stream by duplicating the inner reader through the provided closure.
    #[doc(alias = "try_clone")]
    #[must_use = "the result reports whether cloning the inner reader succeeded"]
    pub fn try_clone_with<F, T, E>(&self, clone_inner: F) -> Result<NegotiatedStream<T>, E>
    where
        F: FnOnce(&R) -> Result<T, E>,
    {
        let inner = clone_inner(&self.inner)?;
        Ok(NegotiatedStream {
            inner,
            decision: self.decision,
            buffer: self.buffer.clone(),
        })
    }

    /// Reconstructs a [`NegotiatedStream`] from previously extracted raw components.
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

    pub(crate) fn from_raw_components(
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

    pub(crate) const fn from_buffer(
        inner: R,
        decision: NegotiationPrologue,
        buffer: NegotiationBuffer,
    ) -> Self {
        Self {
            inner,
            decision,
            buffer,
        }
    }

    /// Reconstructs a [`NegotiatedStream`] from its previously extracted parts.
    #[must_use]
    pub fn from_parts(parts: NegotiatedStreamParts<R>) -> Self {
        let (decision, buffer, inner) = parts.into_components();
        Self::from_buffer(inner, decision, buffer)
    }
}
