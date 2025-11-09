use super::super::super::NegotiatedStream;
use super::super::try_map_error::TryMapInnerError;
use super::NegotiatedStreamParts as Parts;
use rsync_protocol::NegotiationPrologue;

impl<R> Parts<R> {
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
    pub fn map_inner<F, T>(self, map: F) -> Parts<T>
    where
        F: FnOnce(R) -> T,
    {
        let Self {
            decision,
            buffer,
            inner,
        } = self;

        Parts {
            decision,
            buffer,
            inner: map(inner),
        }
    }

    /// Attempts to transform the inner reader while preserving the buffered negotiation state.
    ///
    /// When the mapping fails the original reader is returned alongside the error, ensuring callers
    /// retain access to the sniffed bytes without needing to re-run negotiation detection.
    #[must_use = "the result contains either the mapped parts or the preserved error and original parts"]
    pub fn try_map_inner<F, T, E>(self, map: F) -> Result<Parts<T>, TryMapInnerError<Parts<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        let Self {
            decision,
            buffer,
            inner,
        } = self;

        match map(inner) {
            Ok(mapped) => Ok(Parts {
                decision,
                buffer,
                inner: mapped,
            }),
            Err((error, original)) => Err(TryMapInnerError::new(
                error,
                Parts {
                    decision,
                    buffer,
                    inner: original,
                },
            )),
        }
    }

    /// Clones the decomposed negotiation state using a caller-provided duplication strategy.
    ///
    /// The helper mirrors [`NegotiatedStream::try_clone_with`] while operating on extracted parts.
    /// It is particularly useful when the inner transport exposes an inherent
    /// [`try_clone`](std::net::TcpStream::try_clone)-style API instead of [`Clone`]. The buffered
    /// negotiation bytes are copied so the original and cloned parts can be converted into replaying
    /// streams without affecting each other's progress. Errors from `clone_inner` are propagated
    /// unchanged.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, Read};
    ///
    /// let parts = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 30.0\nhello".to_vec()))
    ///     .expect("sniff succeeds")
    ///     .into_parts();
    /// let mut cloned = parts
    ///     .try_clone_with(|cursor| -> std::io::Result<_> { Ok(cursor.clone()) })
    ///     .expect("cursor clone succeeds");
    ///
    /// let mut replay = Vec::new();
    /// cloned
    ///     .into_stream()
    ///     .read_to_end(&mut replay)
    ///     .expect("cloned parts replay buffered bytes");
    /// assert_eq!(replay, b"@RSYNCD: 30.0\nhello");
    /// ```
    #[doc(alias = "try_clone")]
    #[must_use = "the result reports whether cloning the inner reader succeeded"]
    pub fn try_clone_with<F, T, E>(&self, clone_inner: F) -> Result<Parts<T>, E>
    where
        F: FnOnce(&R) -> Result<T, E>,
    {
        let inner = clone_inner(&self.inner)?;
        Ok(Parts {
            decision: self.decision,
            buffer: self.buffer.clone(),
            inner,
        })
    }

    /// Reassembles a [`NegotiatedStream`] from the extracted components.
    ///
    /// Callers can temporarily inspect or adjust the buffered negotiation
    /// state (for example, updating transport-level settings on the inner
    /// reader) and then continue consuming data through the replaying wrapper
    /// without cloning the sniffed bytes. The same reconstruction is available
    /// through [`From`] and [`Into`], allowing callers to rebuild the replaying
    /// stream via trait-based conversions.
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

impl<R> From<NegotiatedStream<R>> for Parts<R> {
    fn from(stream: NegotiatedStream<R>) -> Self {
        stream.into_parts()
    }
}

impl<R> From<Parts<R>> for NegotiatedStream<R> {
    fn from(parts: Parts<R>) -> Self {
        parts.into_stream()
    }
}
