//! Stream access, variant downcasts, and transport mapping for [`SessionHandshake`].

use crate::binary::BinaryHandshake;
use crate::daemon::LegacyDaemonHandshake;
use crate::negotiation::{NegotiatedStream, TryMapInnerError};
use protocol::NegotiationPrologueSniffer;
use std::collections::TryReserveError;

use super::SessionHandshake;

impl<R> SessionHandshake<R> {
    /// Returns a shared reference to the replaying stream regardless of variant.
    #[must_use]
    pub const fn stream(&self) -> &NegotiatedStream<R> {
        match self {
            Self::Binary(handshake) => handshake.stream(),
            Self::Legacy(handshake) => handshake.stream(),
        }
    }

    /// Returns a mutable reference to the replaying stream regardless of variant.
    #[must_use]
    pub const fn stream_mut(&mut self) -> &mut NegotiatedStream<R> {
        match self {
            Self::Binary(handshake) => handshake.stream_mut(),
            Self::Legacy(handshake) => handshake.stream_mut(),
        }
    }

    /// Rehydrates a [`NegotiationPrologueSniffer`] using the captured negotiation snapshot.
    ///
    /// The helper mirrors the variant-specific [`BinaryHandshake::rehydrate_sniffer`] and
    /// [`LegacyDaemonHandshake::rehydrate_sniffer`] methods, allowing callers to rebuild sniffers
    /// without matching on the enum or replaying the underlying transport. The replay buffer and
    /// sniffed prefix length recorded during negotiation are forwarded to the shared
    /// [`NegotiationPrologueSniffer::rehydrate_from_parts`] logic, ensuring the reconstructed
    /// sniffer observes the same transcript as the original detection pass.
    pub fn rehydrate_sniffer(
        &self,
        sniffer: &mut NegotiationPrologueSniffer,
    ) -> Result<(), TryReserveError> {
        match self {
            Self::Binary(handshake) => handshake.rehydrate_sniffer(sniffer),
            Self::Legacy(handshake) => handshake.rehydrate_sniffer(sniffer),
        }
    }

    /// Releases the wrapper and returns the replaying stream.
    #[must_use]
    pub fn into_stream(self) -> NegotiatedStream<R> {
        match self {
            Self::Binary(handshake) => handshake.into_stream(),
            Self::Legacy(handshake) => handshake.into_stream(),
        }
    }

    /// Releases the handshake and returns the underlying transport.
    ///
    /// Any buffered negotiation bytes captured during the sniffing phase are
    /// discarded. Call [`SessionHandshake::into_stream`] or
    /// [`SessionHandshake::into_stream_parts`] when the replay data must be
    /// preserved for subsequent consumers. The helper mirrors
    /// [`NegotiatedStream::into_inner`](crate::NegotiatedStream::into_inner)
    /// and is intended for scenarios where the caller has already consumed or
    /// logged the handshake transcript and only needs to continue using the
    /// raw transport.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::ProtocolVersion;
    /// use rsync_io::negotiate_session;
    /// use std::io::{self, Cursor, Read, Write};
    ///
    /// #[derive(Debug)]
    /// struct Loopback {
    ///     reader: Cursor<Vec<u8>>,
    ///     writes: Vec<u8>,
    /// }
    ///
    /// impl Loopback {
    ///     fn new(advertisement: [u8; 4]) -> Self {
    ///         Self {
    ///             reader: Cursor::new(advertisement.to_vec()),
    ///             writes: Vec::new(),
    ///         }
    ///     }
    ///
    ///     fn writes(&self) -> &[u8] {
    ///         &self.writes
    ///     }
    /// }
    ///
    /// impl Read for Loopback {
    ///     fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    ///         self.reader.read(buf)
    ///     }
    /// }
    ///
    /// impl Write for Loopback {
    ///     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    ///         self.writes.extend_from_slice(buf);
    ///         Ok(buf.len())
    ///     }
    ///
    ///     fn flush(&mut self) -> io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let protocol = ProtocolVersion::from_supported(31).unwrap();
    /// let transport = Loopback::new(u32::from(protocol.as_u8()).to_le_bytes());
    /// let raw = negotiate_session(transport, protocol)
    ///     .unwrap()
    ///     .into_inner();
    ///
    /// // The returned transport is the original stream, including any bytes the
    /// // client wrote while negotiating.
    /// assert_eq!(raw.writes(), &u32::from(protocol.as_u8()).to_le_bytes());
    /// ```
    #[must_use]
    pub fn into_inner(self) -> R {
        self.into_stream().into_inner()
    }

    /// Maps the inner transport while preserving the negotiated metadata.
    ///
    /// The returned handshake replaces `self`; callers must use the value to
    /// retain access to the negotiated stream and metadata.
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> SessionHandshake<T>
    where
        F: FnOnce(R) -> T,
    {
        match self {
            Self::Binary(handshake) => SessionHandshake::Binary(handshake.map_stream_inner(map)),
            Self::Legacy(handshake) => SessionHandshake::Legacy(handshake.map_stream_inner(map)),
        }
    }

    /// Attempts to transform the inner transport for both handshake variants.
    pub fn try_map_stream_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<SessionHandshake<T>, TryMapInnerError<SessionHandshake<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        match self {
            Self::Binary(handshake) => handshake
                .try_map_stream_inner(map)
                .map(SessionHandshake::Binary)
                .map_err(|err| err.map_original(SessionHandshake::Binary)),
            Self::Legacy(handshake) => handshake
                .try_map_stream_inner(map)
                .map(SessionHandshake::Legacy)
                .map_err(|err| err.map_original(SessionHandshake::Legacy)),
        }
    }

    /// Returns the underlying binary handshake if the negotiation used that style.
    pub const fn as_binary(&self) -> Option<&BinaryHandshake<R>> {
        match self {
            Self::Binary(handshake) => Some(handshake),
            Self::Legacy(_) => None,
        }
    }

    /// Returns a mutable reference to the binary handshake when the negotiation used that style.
    pub const fn as_binary_mut(&mut self) -> Option<&mut BinaryHandshake<R>> {
        match self {
            Self::Binary(handshake) => Some(handshake),
            Self::Legacy(_) => None,
        }
    }

    /// Returns the underlying legacy daemon handshake if the negotiation used that style.
    pub const fn as_legacy(&self) -> Option<&LegacyDaemonHandshake<R>> {
        match self {
            Self::Binary(_) => None,
            Self::Legacy(handshake) => Some(handshake),
        }
    }

    /// Returns a mutable reference to the legacy daemon handshake when the negotiation used that style.
    pub const fn as_legacy_mut(&mut self) -> Option<&mut LegacyDaemonHandshake<R>> {
        match self {
            Self::Binary(_) => None,
            Self::Legacy(handshake) => Some(handshake),
        }
    }

    /// Consumes the wrapper, returning the binary handshake when applicable.
    pub fn into_binary(self) -> Result<BinaryHandshake<R>, SessionHandshake<R>> {
        match self {
            Self::Binary(handshake) => Ok(handshake),
            Self::Legacy(_) => Err(self),
        }
    }

    /// Consumes the wrapper, returning the legacy daemon handshake when applicable.
    pub fn into_legacy(self) -> Result<LegacyDaemonHandshake<R>, SessionHandshake<R>> {
        match self {
            Self::Binary(_) => Err(self),
            Self::Legacy(handshake) => Ok(handshake),
        }
    }
}
