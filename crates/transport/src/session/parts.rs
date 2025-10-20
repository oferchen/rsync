use crate::binary::{BinaryHandshake, BinaryHandshakeParts};
use crate::daemon::{LegacyDaemonHandshake, LegacyDaemonHandshakeParts};
use crate::handshake_util::RemoteProtocolAdvertisement;
use crate::negotiation::{NegotiatedStream, NegotiatedStreamParts, TryMapInnerError};
use rsync_protocol::{
    LegacyDaemonGreetingOwned, NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion,
};
use std::{collections::TryReserveError, convert::TryFrom};

use super::handshake::SessionHandshake;

type BinaryHandshakeComponents<R> = (
    u32,
    ProtocolVersion,
    ProtocolVersion,
    ProtocolVersion,
    NegotiatedStreamParts<R>,
);

type LegacyHandshakeComponents<R> = (
    LegacyDaemonGreetingOwned,
    ProtocolVersion,
    NegotiatedStreamParts<R>,
);

type HandshakePartsResult<T, R> = Result<T, SessionHandshakeParts<R>>;

/// Components extracted from a [`SessionHandshake`].
///
/// The structure mirrors the variant-specific handshake wrappers so callers can
/// temporarily take ownership of the buffered negotiation bytes while keeping
/// the negotiated protocol metadata. The parts can be converted back into a
/// [`SessionHandshake`] via [`SessionHandshake::from_stream_parts`] or the
/// [`From`] implementation. The enum directly embeds
/// [`BinaryHandshakeParts`] and [`LegacyDaemonHandshakeParts`], delegating to
/// their helpers so protocol diagnostics remain centralised in the
/// variant-specific implementations. Variant-specific conversions are also
/// exposed via [`From`] and [`TryFrom`] so binary or legacy wrappers can be
/// promoted or recovered without manual pattern matching.
#[derive(Clone, Debug)]
pub enum SessionHandshakeParts<R> {
    /// Binary handshake metadata and replaying stream parts.
    Binary(BinaryHandshakeParts<R>),
    /// Legacy daemon handshake metadata and replaying stream parts.
    Legacy(LegacyDaemonHandshakeParts<R>),
}

impl<R> SessionHandshakeParts<R> {
    /// Constructs [`SessionHandshakeParts`] from binary handshake components.
    ///
    /// This complements [`Self::into_binary`] by enabling callers to rebuild the decomposed
    /// representation after temporarily taking ownership of the raw protocol numbers and
    /// [`NegotiatedStreamParts`]. The helper delegates to
    /// [`BinaryHandshake::from_stream_parts`] so the reconstruction path exercises the exact same
    /// validation as the variant-specific wrapper. Debug builds therefore continue to assert that
    /// the supplied stream captured a binary negotiation, mirroring the protections provided by
    /// [`BinaryHandshakeParts`].
    ///
    /// # Examples
    ///
    /// Rebuild a binary session from its components after wrapping the underlying transport:
    ///
    /// ```
    /// use rsync_protocol::ProtocolVersion;
    /// use rsync_transport::{negotiate_session_parts, SessionHandshakeParts};
    /// use std::io::{self, Cursor, Read, Write};
    ///
    /// #[derive(Clone, Debug)]
    /// struct Loopback {
    ///     reader: Cursor<Vec<u8>>,
    ///     writes: Vec<u8>,
    /// }
    ///
    /// impl Loopback {
    ///     fn new(advertised: ProtocolVersion) -> Self {
    ///         let bytes = u32::from(advertised.as_u8()).to_be_bytes();
    ///         Self { reader: Cursor::new(bytes.to_vec()), writes: Vec::new() }
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
    /// let remote = ProtocolVersion::from_supported(31).unwrap();
    /// let parts = negotiate_session_parts(Loopback::new(remote), ProtocolVersion::NEWEST)
    ///     .expect("binary negotiation succeeds");
    /// let (
    ///     remote_advertised,
    ///     remote_protocol,
    ///     local_advertised,
    ///     negotiated,
    ///     stream_parts,
    /// ) = parts.clone().into_binary().expect("binary components");
    ///
    /// let rebuilt = SessionHandshakeParts::from_binary_components(
    ///     remote_advertised,
    ///     remote_protocol,
    ///     local_advertised,
    ///     negotiated,
    ///     stream_parts,
    /// );
    ///
    /// assert!(rebuilt.is_binary());
    /// assert_eq!(rebuilt.negotiated_protocol(), negotiated);
    /// ```
    #[must_use]
    pub fn from_binary_components(
        remote_advertised: u32,
        remote_protocol: ProtocolVersion,
        local_advertised: ProtocolVersion,
        negotiated_protocol: ProtocolVersion,
        stream: NegotiatedStreamParts<R>,
    ) -> Self {
        SessionHandshakeParts::Binary(
            BinaryHandshake::from_stream_parts(
                remote_advertised,
                remote_protocol,
                local_advertised,
                negotiated_protocol,
                stream,
            )
            .into_parts(),
        )
    }

    /// Constructs [`SessionHandshakeParts`] from legacy daemon handshake components.
    ///
    /// The helper mirrors [`Self::into_legacy`] by reassembling the parts after the caller temporarily
    /// extracts the parsed greeting, negotiated protocol, and replaying stream. Internally the
    /// function delegates to [`LegacyDaemonHandshake::from_stream_parts`], ensuring the legacy
    /// reconstruction path performs the same validation (including debug assertions that the stream
    /// captured a legacy negotiation) as the dedicated wrapper type.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::ProtocolVersion;
    /// use rsync_transport::{negotiate_session_parts, SessionHandshakeParts};
    /// use std::io::{self, Cursor, Read, Write};
    ///
    /// #[derive(Clone, Debug)]
    /// struct Loopback {
    ///     reader: Cursor<Vec<u8>>,
    ///     writes: Vec<u8>,
    /// }
    ///
    /// impl Loopback {
    ///     fn new() -> Self {
    ///         Self { reader: Cursor::new(b"@RSYNCD: 31.0\n".to_vec()), writes: Vec::new() }
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
    /// let parts = negotiate_session_parts(Loopback::new(), ProtocolVersion::NEWEST)
    ///     .expect("legacy negotiation succeeds");
    /// let (greeting, negotiated, stream_parts) =
    ///     parts.clone().into_legacy().expect("legacy components");
    ///
    /// let rebuilt = SessionHandshakeParts::from_legacy_components(
    ///     greeting,
    ///     negotiated,
    ///     stream_parts,
    /// );
    ///
    /// assert!(rebuilt.is_legacy());
    /// assert_eq!(rebuilt.negotiated_protocol(), negotiated);
    /// ```
    #[must_use]
    pub fn from_legacy_components(
        server_greeting: LegacyDaemonGreetingOwned,
        negotiated_protocol: ProtocolVersion,
        stream: NegotiatedStreamParts<R>,
    ) -> Self {
        SessionHandshakeParts::Legacy(
            LegacyDaemonHandshake::from_stream_parts(server_greeting, negotiated_protocol, stream)
                .into_parts(),
        )
    }

    /// Returns the negotiation style associated with the extracted handshake.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        match self {
            SessionHandshakeParts::Binary(_) => NegotiationPrologue::Binary,
            SessionHandshakeParts::Legacy(_) => NegotiationPrologue::LegacyAscii,
        }
    }

    /// Reports whether the extracted handshake originated from a binary negotiation.
    ///
    /// The helper mirrors [`SessionHandshake::is_binary`], keeping the
    /// convenience available even after the handshake has been decomposed into
    /// its parts.
    #[must_use]
    pub const fn is_binary(&self) -> bool {
        matches!(self, SessionHandshakeParts::Binary(_))
    }

    /// Reports whether the extracted handshake originated from the legacy ASCII negotiation.
    ///
    /// This mirrors [`SessionHandshake::is_legacy`] and returns `true` when the
    /// parts were produced from a legacy `@RSYNCD:` daemon exchange.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        matches!(self, SessionHandshakeParts::Legacy(_))
    }

    /// Returns the negotiated protocol version retained by the parts structure.
    #[must_use]
    pub fn negotiated_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.negotiated_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.negotiated_protocol(),
        }
    }

    /// Returns the protocol advertised by the remote peer.
    #[must_use]
    pub fn remote_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.remote_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.server_protocol(),
        }
    }

    /// Returns the raw protocol number advertised by the remote peer.
    #[must_use]
    pub fn remote_advertised_protocol(&self) -> u32 {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.remote_advertised_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.remote_advertised_protocol(),
        }
    }

    /// Returns the protocol version advertised by the local peer before the negotiation settled.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub fn local_advertised_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.local_advertised_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.local_advertised_protocol(),
        }
    }

    /// Returns the classification of the peer's protocol advertisement.
    #[must_use]
    pub fn remote_advertisement(&self) -> RemoteProtocolAdvertisement {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.remote_advertisement(),
            SessionHandshakeParts::Legacy(parts) => parts.remote_advertisement(),
        }
    }

    /// Returns the legacy daemon greeting advertised by the server when available.
    #[must_use]
    pub fn server_greeting(&self) -> Option<&LegacyDaemonGreetingOwned> {
        match self {
            SessionHandshakeParts::Binary(_) => None,
            SessionHandshakeParts::Legacy(parts) => Some(parts.server_greeting()),
        }
    }

    /// Returns a shared reference to the replaying stream parts.
    #[must_use]
    pub fn stream(&self) -> &NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.stream_parts(),
            SessionHandshakeParts::Legacy(parts) => parts.stream_parts(),
        }
    }

    /// Returns a mutable reference to the replaying stream parts.
    #[must_use]
    pub fn stream_mut(&mut self) -> &mut NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.stream_parts_mut(),
            SessionHandshakeParts::Legacy(parts) => parts.stream_parts_mut(),
        }
    }

    /// Releases the parts structure and returns the replaying stream parts captured during negotiation.
    ///
    /// The returned [`NegotiatedStreamParts`] retain the buffered prologue, decision, and transport,
    /// allowing callers to inspect or transform the replay data without first rebuilding a
    /// [`SessionHandshake`]. This mirrors [`Self::stream`] for owned access and keeps the
    /// high-level API aligned with the variant-specific helpers exposed by
    /// [`BinaryHandshakeParts`] and [`LegacyDaemonHandshakeParts`].
    ///
    /// # Examples
    ///
    /// Reconstruct a binary negotiation and extract the replaying stream parts while preserving the
    /// buffered handshake prefix.
    ///
    /// ```
    /// use rsync_protocol::ProtocolVersion;
    /// use rsync_transport::negotiate_session;
    /// use std::io::{self, Cursor, Read, Write};
    ///
    /// #[derive(Clone, Debug)]
    /// struct Loopback {
    ///     reader: Cursor<Vec<u8>>,
    ///     written: Vec<u8>,
    /// }
    ///
    /// impl Loopback {
    ///     fn new(advertisement: [u8; 4]) -> Self {
    ///         Self {
    ///             reader: Cursor::new(advertisement.to_vec()),
    ///             written: Vec::new(),
    ///         }
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
    ///         self.written.extend_from_slice(buf);
    ///         Ok(buf.len())
    ///     }
    ///
    ///     fn flush(&mut self) -> io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let remote = ProtocolVersion::from_supported(31).unwrap();
    /// let transport = Loopback::new(u32::from(remote.as_u8()).to_be_bytes());
    /// let parts = negotiate_session(transport, ProtocolVersion::NEWEST)
    ///     .unwrap()
    ///     .into_stream_parts();
    /// let stream_parts = parts.clone().into_stream_parts();
    ///
    /// assert_eq!(stream_parts.decision(), parts.decision());
    /// assert_eq!(stream_parts.buffered(), parts.stream().buffered());
    /// ```
    #[must_use]
    pub fn into_stream_parts(self) -> NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.into_stream_parts(),
            SessionHandshakeParts::Legacy(parts) => parts.into_stream_parts(),
        }
    }

    /// Releases the parts structure and reconstructs the replaying stream.
    #[must_use]
    pub fn into_stream(self) -> NegotiatedStream<R> {
        self.into_stream_parts().into_stream()
    }

    /// Rehydrates a [`NegotiationPrologueSniffer`] using the stored negotiation snapshot.
    pub fn rehydrate_sniffer(
        &self,
        sniffer: &mut NegotiationPrologueSniffer,
    ) -> Result<(), TryReserveError> {
        self.stream().rehydrate_sniffer(sniffer)
    }

    /// Maps the inner transport for both variants while preserving the negotiated metadata.
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> SessionHandshakeParts<T>
    where
        F: FnOnce(R) -> T,
    {
        match self {
            SessionHandshakeParts::Binary(parts) => {
                SessionHandshakeParts::Binary(parts.map_stream_inner(map))
            }
            SessionHandshakeParts::Legacy(parts) => {
                SessionHandshakeParts::Legacy(parts.map_stream_inner(map))
            }
        }
    }

    /// Attempts to transform the inner transport for both handshake variants while preserving metadata.
    pub fn try_map_stream_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<SessionHandshakeParts<T>, TryMapInnerError<SessionHandshakeParts<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        match self {
            SessionHandshakeParts::Binary(parts) => parts
                .try_map_stream_inner(map)
                .map(SessionHandshakeParts::Binary)
                .map_err(|err| err.map_original(SessionHandshakeParts::Binary)),
            SessionHandshakeParts::Legacy(parts) => parts
                .try_map_stream_inner(map)
                .map(SessionHandshakeParts::Legacy)
                .map_err(|err| err.map_original(SessionHandshakeParts::Legacy)),
        }
    }

    /// Consumes the parts structure, returning the binary handshake components when available.
    pub fn into_binary(self) -> HandshakePartsResult<BinaryHandshakeComponents<R>, R> {
        match self {
            SessionHandshakeParts::Binary(parts) => {
                let (remote_advertised, remote_protocol, local_advertised, negotiated, stream) =
                    parts.into_components();
                Ok((
                    remote_advertised,
                    remote_protocol,
                    local_advertised,
                    negotiated,
                    stream,
                ))
            }
            SessionHandshakeParts::Legacy(parts) => Err(SessionHandshakeParts::Legacy(parts)),
        }
    }

    /// Consumes the parts structure, returning the binary handshake parts when available.
    ///
    /// The helper mirrors [`Self::into_binary`] but rebuilds the strongly typed
    /// [`BinaryHandshakeParts`] wrapper so callers can reuse convenience
    /// accessors without recreating the full [`BinaryHandshake`]. Returning the
    /// original value on mismatch matches the ergonomics of [`TryFrom`]
    /// conversions provided elsewhere in the crate.
    pub fn into_binary_parts(self) -> Result<BinaryHandshakeParts<R>, SessionHandshakeParts<R>> {
        match self {
            SessionHandshakeParts::Binary(parts) => Ok(parts),
            SessionHandshakeParts::Legacy(parts) => Err(SessionHandshakeParts::Legacy(parts)),
        }
    }

    /// Consumes the parts structure, returning the legacy handshake components when available.
    pub fn into_legacy(self) -> HandshakePartsResult<LegacyHandshakeComponents<R>, R> {
        match self {
            SessionHandshakeParts::Binary(parts) => Err(SessionHandshakeParts::Binary(parts)),
            SessionHandshakeParts::Legacy(parts) => {
                let (greeting, negotiated, stream) = parts.into_components();
                Ok((greeting, negotiated, stream))
            }
        }
    }

    /// Consumes the parts structure, returning the legacy handshake parts when available.
    ///
    /// The returned [`LegacyDaemonHandshakeParts`] retains the parsed greeting
    /// and negotiated protocol while exposing the additional helper methods
    /// implemented by the legacy-specific wrapper. Returning the original value
    /// when the negotiation was binary mirrors the ergonomics of
    /// [`Self::into_legacy`] and the [`TryFrom`] conversions below.
    pub fn into_legacy_parts(
        self,
    ) -> Result<LegacyDaemonHandshakeParts<R>, SessionHandshakeParts<R>> {
        match self {
            SessionHandshakeParts::Binary(parts) => Err(SessionHandshakeParts::Binary(parts)),
            SessionHandshakeParts::Legacy(parts) => Ok(parts),
        }
    }

    /// Reports whether the remote advertisement had to be clamped to the supported range.
    #[must_use]
    pub fn remote_protocol_was_clamped(&self) -> bool {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.remote_protocol_was_clamped(),
            SessionHandshakeParts::Legacy(parts) => parts.remote_protocol_was_clamped(),
        }
    }

    /// Reports whether the negotiated protocol was reduced due to the caller's desired cap.
    ///
    /// This mirrors [`SessionHandshake::local_protocol_was_capped`] while operating on the decomposed
    /// parts. The check observes the same `--protocol` semantics: a caller-specified cap forces the
    /// session to run at the requested protocol even if the peer advertised something newer.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub fn local_protocol_was_capped(&self) -> bool {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.local_protocol_was_capped(),
            SessionHandshakeParts::Legacy(parts) => parts.local_protocol_was_capped(),
        }
    }

    /// Reassembles a [`SessionHandshake`] from the stored components.
    #[must_use]
    pub fn into_handshake(self) -> SessionHandshake<R> {
        match self {
            SessionHandshakeParts::Binary(parts) => {
                SessionHandshake::Binary(BinaryHandshake::from_parts(parts))
            }
            SessionHandshakeParts::Legacy(parts) => {
                SessionHandshake::Legacy(LegacyDaemonHandshake::from_parts(parts))
            }
        }
    }

    /// Releases the parts structure and returns the underlying transport.
    ///
    /// Buffered negotiation bytes captured during sniffing are discarded. Use
    /// [`SessionHandshakeParts::into_handshake`] or
    /// [`SessionHandshakeParts::into_stream`] when the replay data must be
    /// preserved. This convenience wrapper mirrors
    /// [`NegotiatedStream::into_inner`](crate::NegotiatedStream::into_inner)
    /// for callers that only require continued access to the raw transport.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.into_stream_parts().into_inner()
    }
}

impl<R> From<BinaryHandshake<R>> for SessionHandshakeParts<R> {
    fn from(handshake: BinaryHandshake<R>) -> Self {
        SessionHandshakeParts::Binary(handshake.into_parts())
    }
}

impl<R> From<LegacyDaemonHandshake<R>> for SessionHandshakeParts<R> {
    fn from(handshake: LegacyDaemonHandshake<R>) -> Self {
        SessionHandshakeParts::Legacy(handshake.into_parts())
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for BinaryHandshake<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        match parts {
            SessionHandshakeParts::Binary(parts) => Ok(BinaryHandshake::from_parts(parts)),
            SessionHandshakeParts::Legacy(parts) => Err(SessionHandshakeParts::Legacy(parts)),
        }
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for LegacyDaemonHandshake<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        match parts {
            SessionHandshakeParts::Legacy(parts) => Ok(LegacyDaemonHandshake::from_parts(parts)),
            SessionHandshakeParts::Binary(parts) => Err(SessionHandshakeParts::Binary(parts)),
        }
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for BinaryHandshakeParts<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        parts.into_binary_parts()
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for LegacyDaemonHandshakeParts<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        parts.into_legacy_parts()
    }
}
