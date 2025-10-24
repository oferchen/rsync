use crate::binary::{BinaryHandshake, negotiate_binary_session_from_stream};
use crate::daemon::{LegacyDaemonHandshake, negotiate_legacy_daemon_session_from_stream};
use crate::handshake_util::RemoteProtocolAdvertisement;
use crate::negotiation::{
    NegotiatedStream, TryMapInnerError, sniff_negotiation_stream,
    sniff_negotiation_stream_with_sniffer,
};
use core::convert::TryFrom;
use rsync_protocol::{
    LegacyDaemonGreetingOwned, NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion,
};
use std::collections::TryReserveError;
use std::io::{self, Read, Write};

use super::parts::SessionHandshakeParts;

/// Result of negotiating an rsync session over an arbitrary transport.
///
/// The enum wraps either the binary remote-shell handshake or the legacy ASCII
/// daemon negotiation while exposing convenience accessors that mirror the
/// per-variant helpers. Higher layers can match on the [`decision`] to branch on
/// the negotiated style without re-sniffing the transport. Conversions are
/// provided via [`From`] and [`TryFrom`] so variant-specific wrappers can be
/// promoted or recovered ergonomically.
///
/// When the underlying transport implements [`Clone`], the session wrapper can
/// also be cloned. The clone retains the negotiated metadata and replay buffer
/// so both instances may continue processing without interfering with each
/// other—useful for tooling that needs to inspect the transcript while keeping
/// the original session active.
#[derive(Clone, Debug)]
pub enum SessionHandshake<R> {
    /// Binary remote-shell style negotiation (protocols ≥ 30).
    Binary(BinaryHandshake<R>),
    /// Legacy `@RSYNCD:` daemon negotiation.
    #[doc(alias = "@RSYNCD")]
    Legacy(LegacyDaemonHandshake<R>),
}

impl<R> SessionHandshake<R> {
    /// Returns the detected negotiation style.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        match self {
            Self::Binary(_) => NegotiationPrologue::Binary,
            Self::Legacy(_) => NegotiationPrologue::LegacyAscii,
        }
    }

    /// Reports whether the session negotiated the binary remote-shell protocol.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_binary`], allowing callers
    /// to branch on the handshake style without matching on [`Self`]
    /// explicitly. Binary negotiations correspond to protocols 30 and newer.
    #[must_use]
    pub const fn is_binary(&self) -> bool {
        matches!(self, Self::Binary(_))
    }

    /// Reports whether the session negotiated the legacy ASCII daemon protocol.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_legacy`] and returns `true`
    /// when the handshake flowed through the `@RSYNCD:` daemon exchange.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy(_))
    }

    /// Returns the negotiated protocol version after applying the caller cap.
    #[must_use]
    pub fn negotiated_protocol(&self) -> ProtocolVersion {
        match self {
            Self::Binary(handshake) => handshake.negotiated_protocol(),
            Self::Legacy(handshake) => handshake.negotiated_protocol(),
        }
    }

    /// Returns the protocol version advertised by the peer before client caps are applied.
    #[must_use]
    pub fn remote_protocol(&self) -> ProtocolVersion {
        match self {
            Self::Binary(handshake) => handshake.remote_protocol(),
            Self::Legacy(handshake) => handshake.server_protocol(),
        }
    }

    /// Returns the raw protocol number advertised by the remote peer before clamping.
    #[must_use]
    pub fn remote_advertised_protocol(&self) -> u32 {
        match self {
            Self::Binary(handshake) => handshake.remote_advertised_protocol(),
            Self::Legacy(handshake) => handshake.remote_advertised_protocol(),
        }
    }

    /// Returns the protocol version advertised by the local peer before the negotiation settled.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub fn local_advertised_protocol(&self) -> ProtocolVersion {
        match self {
            Self::Binary(handshake) => handshake.local_advertised_protocol(),
            Self::Legacy(handshake) => handshake.local_advertised_protocol(),
        }
    }

    /// Returns the classification of the peer's protocol advertisement.
    #[must_use]
    pub fn remote_advertisement(&self) -> RemoteProtocolAdvertisement {
        match self {
            Self::Binary(handshake) => handshake.remote_advertisement(),
            Self::Legacy(handshake) => handshake.remote_advertisement(),
        }
    }

    /// Reports whether the remote advertisement had to be clamped to the supported range.
    #[must_use]
    pub fn remote_protocol_was_clamped(&self) -> bool {
        match self {
            Self::Binary(handshake) => handshake.remote_protocol_was_clamped(),
            Self::Legacy(handshake) => handshake.remote_protocol_was_clamped(),
        }
    }

    /// Reports whether the negotiated protocol was reduced due to the caller's desired cap.
    ///
    /// This mirrors the per-variant helpers and keeps the aggregated handshake API aligned with
    /// upstream rsync, where `--protocol` forces the session to downgrade even when the peer
    /// advertises a newer version.
    ///
    /// # Examples
    ///
    /// Force the session to run at protocol 29 despite the peer advertising 31.
    ///
    /// ```
    /// use rsync_protocol::ProtocolVersion;
    /// use rsync_transport::negotiate_session;
    /// use std::io::{self, Cursor, Read, Write};
    ///
    /// #[derive(Debug)]
    /// struct Loopback {
    ///     reader: Cursor<Vec<u8>>,
    ///     written: Vec<u8>,
    /// }
    ///
    /// impl Loopback {
    ///     fn new(advertised: ProtocolVersion) -> Self {
    ///         let bytes = u32::from(advertised.as_u8()).to_be_bytes();
    ///         Self { reader: Cursor::new(bytes.to_vec()), written: Vec::new() }
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
    /// let desired = ProtocolVersion::from_supported(29).unwrap();
    /// let handshake = negotiate_session(Loopback::new(remote), desired).unwrap();
    ///
    /// assert!(handshake.local_protocol_was_capped());
    /// assert_eq!(handshake.negotiated_protocol(), desired);
    /// ```
    #[doc(alias = "--protocol")]
    #[must_use]
    pub fn local_protocol_was_capped(&self) -> bool {
        match self {
            Self::Binary(handshake) => handshake.local_protocol_was_capped(),
            Self::Legacy(handshake) => handshake.local_protocol_was_capped(),
        }
    }

    /// Returns the parsed legacy daemon greeting when the negotiation used the legacy ASCII handshake.
    ///
    /// Binary negotiations do not exchange a greeting, so the method returns [`None`] in that case.
    #[must_use]
    pub fn server_greeting(&self) -> Option<&LegacyDaemonGreetingOwned> {
        match self {
            Self::Binary(_) => None,
            Self::Legacy(handshake) => Some(handshake.server_greeting()),
        }
    }

    /// Returns a shared reference to the replaying stream regardless of variant.
    #[must_use]
    pub fn stream(&self) -> &NegotiatedStream<R> {
        match self {
            Self::Binary(handshake) => handshake.stream(),
            Self::Legacy(handshake) => handshake.stream(),
        }
    }

    /// Returns a mutable reference to the replaying stream regardless of variant.
    #[must_use]
    pub fn stream_mut(&mut self) -> &mut NegotiatedStream<R> {
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
    /// use rsync_protocol::ProtocolVersion;
    /// use rsync_transport::negotiate_session;
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
    /// let transport = Loopback::new(u32::from(protocol.as_u8()).to_be_bytes());
    /// let raw = negotiate_session(transport, protocol)
    ///     .unwrap()
    ///     .into_inner();
    ///
    /// // The returned transport is the original stream, including any bytes the
    /// // client wrote while negotiating.
    /// assert_eq!(raw.writes(), &u32::from(protocol.as_u8()).to_be_bytes());
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
    #[must_use]
    pub fn as_binary(&self) -> Option<&BinaryHandshake<R>> {
        match self {
            Self::Binary(handshake) => Some(handshake),
            Self::Legacy(_) => None,
        }
    }

    /// Returns a mutable reference to the binary handshake when the negotiation used that style.
    #[must_use]
    pub fn as_binary_mut(&mut self) -> Option<&mut BinaryHandshake<R>> {
        match self {
            Self::Binary(handshake) => Some(handshake),
            Self::Legacy(_) => None,
        }
    }

    /// Returns the underlying legacy daemon handshake if the negotiation used that style.
    #[must_use]
    pub fn as_legacy(&self) -> Option<&LegacyDaemonHandshake<R>> {
        match self {
            Self::Binary(_) => None,
            Self::Legacy(handshake) => Some(handshake),
        }
    }

    /// Returns a mutable reference to the legacy daemon handshake when the negotiation used that style.
    #[must_use]
    pub fn as_legacy_mut(&mut self) -> Option<&mut LegacyDaemonHandshake<R>> {
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

    /// Decomposes the handshake into variant-specific metadata and replaying stream parts.
    ///
    /// The returned [`SessionHandshakeParts`] mirrors the helpers exposed by the variant-specific
    /// handshakes while allowing higher layers to stage the buffered negotiation bytes and
    /// negotiated metadata without matching on [`SessionHandshake`] immediately. This is useful
    /// when temporary ownership of the underlying transport is required (for example to wrap it
    /// with instrumentation) before resuming the rsync protocol exchange.
    /// Decomposes the handshake into variant-specific metadata and replaying stream parts.
    ///
    /// The returned [`SessionHandshakeParts`] mirrors the helpers exposed by the
    /// variant-specific handshakes while allowing higher layers to stage the
    /// buffered negotiation bytes and negotiated metadata without matching on
    /// [`SessionHandshake`] immediately. This is useful when temporary ownership
    /// of the underlying transport is required (for example to wrap it with
    /// instrumentation) before resuming the rsync protocol exchange.
    #[must_use]
    pub fn into_parts(self) -> SessionHandshakeParts<R> {
        match self {
            SessionHandshake::Binary(handshake) => {
                SessionHandshakeParts::Binary(handshake.into_parts())
            }
            SessionHandshake::Legacy(handshake) => {
                SessionHandshakeParts::Legacy(handshake.into_parts())
            }
        }
    }

    /// Decomposes the handshake into variant-specific metadata and replaying stream parts.
    ///
    /// This is an alias for [`SessionHandshake::into_parts`] retained for historical parity with
    /// earlier drafts of the transport API where the method carried the `into_stream_parts` name.
    /// Keeping the shim avoids churn for downstream users while allowing the documentation and
    /// examples to reference the more succinct terminology shared by variant-specific handshakes.
    #[must_use]
    #[doc(alias = "into_parts")]
    pub fn into_stream_parts(self) -> SessionHandshakeParts<R> {
        self.into_parts()
    }

    /// Reassembles a [`SessionHandshake`] from the variant-specific parts previously extracted via
    /// [`Self::into_parts`].
    ///
    /// Callers can invoke this helper directly or rely on the [`From`] conversion implemented for
    /// [`SessionHandshakeParts`], which internally delegates to this constructor. The explicit
    /// method remains available for situations where type inference benefits from naming the
    /// conversion target.
    #[must_use]
    pub fn from_parts(parts: SessionHandshakeParts<R>) -> Self {
        match parts {
            SessionHandshakeParts::Binary(parts) => {
                SessionHandshake::Binary(BinaryHandshake::from_parts(parts))
            }
            SessionHandshakeParts::Legacy(parts) => {
                SessionHandshake::Legacy(LegacyDaemonHandshake::from_parts(parts))
            }
        }
    }

    /// Reassembles a [`SessionHandshake`] from the variant-specific stream parts previously
    /// extracted via [`Self::into_stream_parts`].
    ///
    /// This method aliases [`SessionHandshake::from_parts`]; it remains available for symmetry with
    /// older code that referred to the split representation as "stream parts". New call sites should
    /// prefer [`SessionHandshake::from_parts`] for consistency with the variant-specific helpers.
    #[must_use]
    #[doc(alias = "from_parts")]
    pub fn from_stream_parts(parts: SessionHandshakeParts<R>) -> Self {
        SessionHandshake::from_parts(parts)
    }
}

impl<R> From<SessionHandshakeParts<R>> for SessionHandshake<R> {
    fn from(parts: SessionHandshakeParts<R>) -> Self {
        SessionHandshake::from_parts(parts)
    }
}

impl<R> From<SessionHandshake<R>> for SessionHandshakeParts<R> {
    fn from(handshake: SessionHandshake<R>) -> Self {
        handshake.into_stream_parts()
    }
}

impl<R> From<BinaryHandshake<R>> for SessionHandshake<R> {
    fn from(handshake: BinaryHandshake<R>) -> Self {
        SessionHandshake::Binary(handshake)
    }
}

impl<R> From<LegacyDaemonHandshake<R>> for SessionHandshake<R> {
    fn from(handshake: LegacyDaemonHandshake<R>) -> Self {
        SessionHandshake::Legacy(handshake)
    }
}

impl<R> TryFrom<SessionHandshake<R>> for BinaryHandshake<R> {
    type Error = SessionHandshake<R>;

    fn try_from(handshake: SessionHandshake<R>) -> Result<Self, Self::Error> {
        handshake.into_binary()
    }
}

impl<R> TryFrom<SessionHandshake<R>> for LegacyDaemonHandshake<R> {
    type Error = SessionHandshake<R>;

    fn try_from(handshake: SessionHandshake<R>) -> Result<Self, Self::Error> {
        handshake.into_legacy()
    }
}

/// Negotiates an rsync session, automatically detecting the handshake style.
///
/// The helper mirrors upstream rsync's behaviour when dialing a transport whose
/// negotiation style is unknown. It sniffs the prologue, dispatches to either
/// the binary or legacy negotiation helper, and returns a [`SessionHandshake`]
/// carrying the negotiated metadata together with the replaying stream.
///
/// # Errors
///
/// Propagates any I/O error reported by the underlying sniffing or variant
/// specific negotiation helper.
pub fn negotiate_session<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream(reader)?;
    negotiate_session_from_stream(stream, desired_protocol)
}

/// Negotiates an rsync session and returns the decomposed handshake parts.
///
/// This is a convenience wrapper around [`negotiate_session`] that immediately
/// converts the negotiated handshake into [`SessionHandshakeParts`]. Callers
/// that intend to wrap the underlying transport typically need this split
/// representation to stash the replayed negotiation bytes while instrumenting
/// the stream before resuming the protocol exchange.
///
/// # Errors
///
/// Propagates any I/O error reported by [`negotiate_session`].
pub fn negotiate_session_parts<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshakeParts<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream(reader)?;
    negotiate_session_parts_from_stream(stream, desired_protocol)
}

/// Negotiates an rsync session while reusing a caller supplied sniffer.
///
/// This mirrors [`negotiate_session`] but allows higher layers to reuse a
/// [`NegotiationPrologueSniffer`] across multiple negotiations, matching the
/// existing binary and legacy helper variants.
pub fn negotiate_session_with_sniffer<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<SessionHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream_with_sniffer(reader, sniffer)?;
    negotiate_session_from_stream(stream, desired_protocol)
}

/// Negotiates an rsync session using a caller supplied sniffer and returns the
/// decomposed handshake parts.
///
/// This mirrors [`negotiate_session_parts`] but reuses the provided
/// [`NegotiationPrologueSniffer`], matching the semantics of
/// [`negotiate_session_with_sniffer`].
///
/// # Errors
///
/// Propagates any I/O error reported by
/// [`negotiate_session_with_sniffer`].
pub fn negotiate_session_parts_with_sniffer<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<SessionHandshakeParts<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream_with_sniffer(reader, sniffer)?;
    negotiate_session_parts_from_stream(stream, desired_protocol)
}

/// Negotiates an rsync session using a pre-sniffed [`NegotiatedStream`].
///
/// Callers that already possess the [`NegotiatedStream`] returned by
/// [`sniff_negotiation_stream`](crate::sniff_negotiation_stream) (or its
/// sniffer-backed variant) can use this helper to complete the handshake without
/// repeating the prologue detection. The function dispatches to the binary or
/// legacy negotiation path based on the recorded decision and returns the
/// corresponding [`SessionHandshake`].
///
/// # Errors
///
/// Propagates any I/O error reported while driving the variant-specific
/// negotiation helper. If the negotiation prologue was not fully determined
/// the function returns [`io::ErrorKind::UnexpectedEof`] with the canonical
/// transport error message used by [`NegotiatedStream::ensure_decision`].
pub fn negotiate_session_from_stream<R>(
    stream: NegotiatedStream<R>,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshake<R>>
where
    R: Read + Write,
{
    match stream.decision() {
        NegotiationPrologue::Binary => {
            negotiate_binary_session_from_stream(stream, desired_protocol)
                .map(SessionHandshake::Binary)
        }
        NegotiationPrologue::LegacyAscii => {
            negotiate_legacy_daemon_session_from_stream(stream, desired_protocol)
                .map(SessionHandshake::Legacy)
        }
        NegotiationPrologue::NeedMoreData => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            crate::negotiation::NEGOTIATION_PROLOGUE_UNDETERMINED_MSG,
        )),
    }
}

/// Negotiates an rsync session from a pre-sniffed stream and returns the decomposed parts.
///
/// This convenience wrapper mirrors [`negotiate_session_from_stream`] but immediately converts the
/// resulting [`SessionHandshake`] into [`SessionHandshakeParts`]. Callers that already possess a
/// [`NegotiatedStream`]—for instance after invoking
/// [`sniff_negotiation_stream`](crate::sniff_negotiation_stream)—can therefore obtain the replaying
/// stream parts and negotiated metadata without rebuilding the handshake manually.
///
/// # Errors
///
/// Propagates any I/O error produced while driving [`negotiate_session_from_stream`].
///
/// # Examples
///
/// ```
/// use rsync_protocol::ProtocolVersion;
/// use rsync_transport::{
///     negotiate_session_parts_from_stream, sniff_negotiation_stream, SessionHandshakeParts,
/// };
/// use std::io::{self, Cursor, Read, Write};
///
/// #[derive(Debug)]
/// struct Loopback {
///     reader: Cursor<Vec<u8>>,
///     written: Vec<u8>,
/// }
///
/// impl Loopback {
///     fn new(advertised: ProtocolVersion) -> Self {
///         let bytes = u32::from(advertised.as_u8()).to_be_bytes();
///         Self { reader: Cursor::new(bytes.to_vec()), written: Vec::new() }
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
/// let stream = sniff_negotiation_stream(Loopback::new(remote)).unwrap();
/// let parts: SessionHandshakeParts<_> =
///     negotiate_session_parts_from_stream(stream, ProtocolVersion::NEWEST).unwrap();
///
/// assert!(parts.is_binary());
/// assert_eq!(parts.negotiated_protocol(), remote);
/// ```
pub fn negotiate_session_parts_from_stream<R>(
    stream: NegotiatedStream<R>,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshakeParts<R>>
where
    R: Read + Write,
{
    negotiate_session_from_stream(stream, desired_protocol).map(SessionHandshake::into_stream_parts)
}
