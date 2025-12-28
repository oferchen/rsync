use crate::binary::BinaryHandshake;
use crate::daemon::LegacyDaemonHandshake;
use crate::handshake_util::RemoteProtocolAdvertisement;
use crate::negotiation::{NegotiatedStream, TryMapInnerError};
use ::core::convert::TryFrom;
use protocol::{
    LegacyDaemonGreetingOwned, NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion,
};
use std::collections::TryReserveError;

use super::super::parts::SessionHandshakeParts;

/// Result of negotiating an rsync session over an arbitrary transport.
///
/// The enum wraps either the binary remote-shell handshake or the legacy ASCII
/// daemon negotiation while exposing convenience accessors that mirror the
/// per-variant helpers. Higher layers can match on the
/// [`SessionHandshake::decision`] to branch on
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
    /// use protocol::ProtocolVersion;
    /// use rsync_io::negotiate_session;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff_negotiation_stream;
    use protocol::{CompatibilityFlags, parse_legacy_daemon_greeting_owned};
    use std::io::Cursor;

    fn create_binary_handshake() -> BinaryHandshake<Cursor<Vec<u8>>> {
        // Binary negotiation is triggered by first byte != '@'
        // Protocol 31 as BE u32: 0x00 0x00 0x00 0x1f
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        BinaryHandshake::from_components(
            31,
            proto31,
            proto31,
            proto31,
            CompatibilityFlags::EMPTY,
            stream,
        )
    }

    fn create_legacy_handshake() -> LegacyDaemonHandshake<Cursor<Vec<u8>>> {
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let greeting = parse_legacy_daemon_greeting_owned("@RSYNCD: 31.0").expect("valid greeting");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        LegacyDaemonHandshake::from_components(greeting, proto31, stream)
    }

    // ==== decision() tests ====

    #[test]
    fn decision_returns_binary_for_binary_variant() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert_eq!(session.decision(), NegotiationPrologue::Binary);
    }

    #[test]
    fn decision_returns_legacy_for_legacy_variant() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert_eq!(session.decision(), NegotiationPrologue::LegacyAscii);
    }

    // ==== is_binary() / is_legacy() tests ====

    #[test]
    fn is_binary_true_for_binary_variant() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert!(session.is_binary());
        assert!(!session.is_legacy());
    }

    #[test]
    fn is_legacy_true_for_legacy_variant() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert!(session.is_legacy());
        assert!(!session.is_binary());
    }

    // ==== negotiated_protocol() tests ====

    #[test]
    fn negotiated_protocol_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert_eq!(session.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn negotiated_protocol_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert_eq!(session.negotiated_protocol().as_u8(), 31);
    }

    // ==== remote_protocol() tests ====

    #[test]
    fn remote_protocol_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert_eq!(session.remote_protocol().as_u8(), 31);
    }

    #[test]
    fn remote_protocol_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert_eq!(session.remote_protocol().as_u8(), 31);
    }

    // ==== remote_advertised_protocol() tests ====

    #[test]
    fn remote_advertised_protocol_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert_eq!(session.remote_advertised_protocol(), 31);
    }

    #[test]
    fn remote_advertised_protocol_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert_eq!(session.remote_advertised_protocol(), 31);
    }

    // ==== local_advertised_protocol() tests ====

    #[test]
    fn local_advertised_protocol_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert_eq!(session.local_advertised_protocol().as_u8(), 31);
    }

    #[test]
    fn local_advertised_protocol_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert_eq!(session.local_advertised_protocol().as_u8(), 31);
    }

    // ==== server_greeting() tests ====

    #[test]
    fn server_greeting_none_for_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert!(session.server_greeting().is_none());
    }

    #[test]
    fn server_greeting_some_for_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert!(session.server_greeting().is_some());
    }

    // ==== stream() tests ====

    #[test]
    fn stream_returns_reference_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let stream = session.stream();
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    #[test]
    fn stream_returns_reference_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let stream = session.stream();
        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    }

    // ==== stream_mut() tests ====

    #[test]
    fn stream_mut_returns_mutable_reference_binary() {
        let binary = create_binary_handshake();
        let mut session = SessionHandshake::Binary(binary);
        let stream = session.stream_mut();
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    #[test]
    fn stream_mut_returns_mutable_reference_legacy() {
        let legacy = create_legacy_handshake();
        let mut session = SessionHandshake::Legacy(legacy);
        let stream = session.stream_mut();
        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    }

    // ==== into_stream() tests ====

    #[test]
    fn into_stream_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let stream = session.into_stream();
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    #[test]
    fn into_stream_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let stream = session.into_stream();
        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    }

    // ==== as_binary() / as_binary_mut() tests ====

    #[test]
    fn as_binary_returns_some_for_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert!(session.as_binary().is_some());
    }

    #[test]
    fn as_binary_returns_none_for_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert!(session.as_binary().is_none());
    }

    #[test]
    fn as_binary_mut_returns_some_for_binary() {
        let binary = create_binary_handshake();
        let mut session = SessionHandshake::Binary(binary);
        assert!(session.as_binary_mut().is_some());
    }

    #[test]
    fn as_binary_mut_returns_none_for_legacy() {
        let legacy = create_legacy_handshake();
        let mut session = SessionHandshake::Legacy(legacy);
        assert!(session.as_binary_mut().is_none());
    }

    // ==== as_legacy() / as_legacy_mut() tests ====

    #[test]
    fn as_legacy_returns_some_for_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert!(session.as_legacy().is_some());
    }

    #[test]
    fn as_legacy_returns_none_for_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert!(session.as_legacy().is_none());
    }

    #[test]
    fn as_legacy_mut_returns_some_for_legacy() {
        let legacy = create_legacy_handshake();
        let mut session = SessionHandshake::Legacy(legacy);
        assert!(session.as_legacy_mut().is_some());
    }

    #[test]
    fn as_legacy_mut_returns_none_for_binary() {
        let binary = create_binary_handshake();
        let mut session = SessionHandshake::Binary(binary);
        assert!(session.as_legacy_mut().is_none());
    }

    // ==== into_binary() / into_legacy() tests ====

    #[test]
    fn into_binary_ok_for_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert!(session.into_binary().is_ok());
    }

    #[test]
    fn into_binary_err_for_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert!(session.into_binary().is_err());
    }

    #[test]
    fn into_legacy_ok_for_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        assert!(session.into_legacy().is_ok());
    }

    #[test]
    fn into_legacy_err_for_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        assert!(session.into_legacy().is_err());
    }

    // ==== From implementations tests ====

    #[test]
    fn from_binary_handshake() {
        let binary = create_binary_handshake();
        let session: SessionHandshake<_> = binary.into();
        assert!(session.is_binary());
    }

    #[test]
    fn from_legacy_handshake() {
        let legacy = create_legacy_handshake();
        let session: SessionHandshake<_> = legacy.into();
        assert!(session.is_legacy());
    }

    // ==== TryFrom implementations tests ====

    #[test]
    fn try_from_session_to_binary_ok() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let result: Result<BinaryHandshake<_>, _> = session.try_into();
        assert!(result.is_ok());
    }

    #[test]
    fn try_from_session_to_binary_err() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let result: Result<BinaryHandshake<_>, _> = session.try_into();
        assert!(result.is_err());
    }

    #[test]
    fn try_from_session_to_legacy_ok() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let result: Result<LegacyDaemonHandshake<_>, _> = session.try_into();
        assert!(result.is_ok());
    }

    #[test]
    fn try_from_session_to_legacy_err() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let result: Result<LegacyDaemonHandshake<_>, _> = session.try_into();
        assert!(result.is_err());
    }

    // ==== into_parts / from_parts tests ====

    #[test]
    fn into_parts_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let parts = session.into_parts();
        assert!(matches!(parts, SessionHandshakeParts::Binary(_)));
    }

    #[test]
    fn into_parts_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let parts = session.into_parts();
        assert!(matches!(parts, SessionHandshakeParts::Legacy(_)));
    }

    #[test]
    fn into_stream_parts_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let parts = session.into_stream_parts();
        assert!(matches!(parts, SessionHandshakeParts::Binary(_)));
    }

    #[test]
    fn into_stream_parts_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let parts = session.into_stream_parts();
        assert!(matches!(parts, SessionHandshakeParts::Legacy(_)));
    }

    #[test]
    fn from_parts_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let parts = session.into_parts();
        let rebuilt = SessionHandshake::from_parts(parts);
        assert!(rebuilt.is_binary());
    }

    #[test]
    fn from_parts_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let parts = session.into_parts();
        let rebuilt = SessionHandshake::from_parts(parts);
        assert!(rebuilt.is_legacy());
    }

    #[test]
    fn from_stream_parts_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let parts = session.into_stream_parts();
        let rebuilt = SessionHandshake::from_stream_parts(parts);
        assert!(rebuilt.is_binary());
    }

    #[test]
    fn from_stream_parts_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let parts = session.into_stream_parts();
        let rebuilt = SessionHandshake::from_stream_parts(parts);
        assert!(rebuilt.is_legacy());
    }

    // ==== From/Into SessionHandshakeParts tests ====

    #[test]
    fn parts_into_session_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let parts = session.into_parts();
        let rebuilt: SessionHandshake<_> = parts.into();
        assert!(rebuilt.is_binary());
    }

    #[test]
    fn parts_into_session_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let parts = session.into_parts();
        let rebuilt: SessionHandshake<_> = parts.into();
        assert!(rebuilt.is_legacy());
    }

    #[test]
    fn session_into_parts() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let parts: SessionHandshakeParts<_> = session.into();
        assert!(matches!(parts, SessionHandshakeParts::Binary(_)));
    }

    // ==== map_stream_inner tests ====

    #[test]
    fn map_stream_inner_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let mapped = session.map_stream_inner(|_cursor| {
            // Return a simple unit type to verify transformation
        });
        assert!(mapped.is_binary());
    }

    #[test]
    fn map_stream_inner_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let mapped = session.map_stream_inner(|_cursor| {
            // Return a simple unit type to verify transformation
        });
        assert!(mapped.is_legacy());
    }

    // ==== Clone / Debug tests ====

    #[test]
    fn clone_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let cloned = session.clone();
        assert!(cloned.is_binary());
        assert_eq!(session.negotiated_protocol(), cloned.negotiated_protocol());
    }

    #[test]
    fn clone_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let cloned = session.clone();
        assert!(cloned.is_legacy());
        assert_eq!(session.negotiated_protocol(), cloned.negotiated_protocol());
    }

    #[test]
    fn debug_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let debug = format!("{session:?}");
        assert!(debug.contains("Binary"));
    }

    #[test]
    fn debug_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let debug = format!("{session:?}");
        assert!(debug.contains("Legacy"));
    }

    // ==== remote_advertisement tests ====

    #[test]
    fn remote_advertisement_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let adv = session.remote_advertisement();
        assert!(matches!(adv, RemoteProtocolAdvertisement::Supported(_)));
    }

    #[test]
    fn remote_advertisement_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let adv = session.remote_advertisement();
        assert!(matches!(adv, RemoteProtocolAdvertisement::Supported(_)));
    }

    // ==== remote_protocol_was_clamped tests ====

    #[test]
    fn remote_protocol_was_clamped_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        // Our test handshake uses protocol 31 which is in supported range
        assert!(!session.remote_protocol_was_clamped());
    }

    #[test]
    fn remote_protocol_was_clamped_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        // Our test handshake uses protocol 31 which is in supported range
        assert!(!session.remote_protocol_was_clamped());
    }

    // ==== local_protocol_was_capped tests ====

    #[test]
    fn local_protocol_was_capped_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        // Our test handshake uses the same protocol for desired and negotiated
        assert!(!session.local_protocol_was_capped());
    }

    #[test]
    fn local_protocol_was_capped_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        // Our test handshake uses the same protocol for desired and negotiated
        assert!(!session.local_protocol_was_capped());
    }

    // ==== into_inner tests ====

    #[test]
    fn into_inner_binary() {
        let binary = create_binary_handshake();
        let session = SessionHandshake::Binary(binary);
        let _inner: Cursor<Vec<u8>> = session.into_inner();
    }

    #[test]
    fn into_inner_legacy() {
        let legacy = create_legacy_handshake();
        let session = SessionHandshake::Legacy(legacy);
        let _inner: Cursor<Vec<u8>> = session.into_inner();
    }
}
