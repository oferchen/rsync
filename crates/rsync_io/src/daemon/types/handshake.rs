use super::parts::LegacyDaemonHandshakeParts;
use crate::handshake_util::{RemoteProtocolAdvertisement, local_cap_reduced_protocol};
use crate::negotiation::{NegotiatedStream, NegotiatedStreamParts, TryMapInnerError};
use protocol::{
    LegacyDaemonGreetingOwned, NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion,
};
use std::collections::TryReserveError;

/// Result of performing the legacy ASCII daemon negotiation.
///
/// The structure exposes the negotiated protocol version together with the
/// parsed greeting metadata while retaining the replaying stream so higher
/// layers can continue consuming control messages or file lists.
///
/// When the underlying transport implements [`Clone`], the handshake can be
/// cloned to stage multiple consumers for the same negotiated session. The
/// replay buffer, greeting, and negotiated protocol are duplicated so both
/// instances progress independently without rereading from the transport.
#[doc(alias = "@RSYNCD")]
#[derive(Clone, Debug)]
pub struct LegacyDaemonHandshake<R> {
    stream: NegotiatedStream<R>,
    server_greeting: LegacyDaemonGreetingOwned,
    negotiated_protocol: ProtocolVersion,
}

impl<R> LegacyDaemonHandshake<R> {
    /// Returns the negotiated protocol version after applying the caller's cap.
    #[must_use]
    pub const fn negotiated_protocol(&self) -> ProtocolVersion {
        self.negotiated_protocol
    }

    /// Returns the protocol version the client advertised to the daemon.
    ///
    /// For the legacy exchange the client echoes the final negotiated protocol back to the server, so
    /// this value mirrors [`Self::negotiated_protocol`] while exposing the same API surface as the
    /// binary handshake helpers that track the client's advertisement explicitly.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_advertised_protocol(&self) -> ProtocolVersion {
        self.negotiated_protocol
    }

    /// Returns the parsed legacy daemon greeting advertised by the server.
    #[must_use]
    pub const fn server_greeting(&self) -> &LegacyDaemonGreetingOwned {
        &self.server_greeting
    }

    /// Returns the protocol version announced by the server before client capping is applied.
    #[must_use]
    pub const fn server_protocol(&self) -> ProtocolVersion {
        self.server_greeting.protocol()
    }

    /// Returns the raw protocol number advertised by the remote daemon before clamping.
    #[must_use]
    pub const fn remote_advertised_protocol(&self) -> u32 {
        self.server_greeting.advertised_protocol()
    }

    /// Reports whether the remote daemon advertised a protocol newer than we support.
    #[must_use]
    pub const fn remote_protocol_was_clamped(&self) -> bool {
        self.remote_advertisement().was_clamped()
    }

    /// Returns the classification of the daemon's protocol advertisement.
    #[must_use]
    pub const fn remote_advertisement(&self) -> RemoteProtocolAdvertisement {
        RemoteProtocolAdvertisement::from_raw(
            self.remote_advertised_protocol(),
            self.server_protocol(),
        )
    }

    /// Reports whether the caller's desired cap reduced the negotiated protocol version.
    ///
    /// The negotiated protocol equals the minimum of the daemon's advertised protocol and the
    /// caller's requested cap (configured via `--protocol`). When the caller limits the session to an
    /// older version, certain protocol features become unavailable. This helper mirrors upstream
    /// rsync by exposing that condition so higher layers can render matching diagnostics.
    ///
    /// # Examples
    ///
    /// Limit the daemon negotiation to protocol 29 even though the server banner advertises 31.
    ///
    /// ```
    /// use protocol::ProtocolVersion;
    /// use rsync_io::negotiate_legacy_daemon_session;
    /// use std::io::{self, Cursor, Read, Write};
    ///
    /// #[derive(Debug)]
    /// struct MemoryTransport {
    ///     reader: Cursor<Vec<u8>>,
    ///     written: Vec<u8>,
    ///     flushes: usize,
    /// }
    ///
    /// impl MemoryTransport {
    ///     fn new(input: &[u8]) -> Self {
    ///         Self { reader: Cursor::new(input.to_vec()), written: Vec::new(), flushes: 0 }
    ///     }
    /// }
    ///
    /// impl Read for MemoryTransport {
    ///     fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    ///         self.reader.read(buf)
    ///     }
    /// }
    ///
    /// impl Write for MemoryTransport {
    ///     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    ///         self.written.extend_from_slice(buf);
    ///         Ok(buf.len())
    ///     }
    ///
    ///     fn flush(&mut self) -> io::Result<()> {
    ///         self.flushes += 1;
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let desired = ProtocolVersion::from_supported(29).unwrap();
    /// let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    /// let handshake = negotiate_legacy_daemon_session(transport, desired).unwrap();
    ///
    /// assert!(handshake.local_protocol_was_capped());
    /// assert_eq!(handshake.negotiated_protocol(), desired);
    /// ```
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_protocol_was_capped(&self) -> bool {
        local_cap_reduced_protocol(self.server_greeting.protocol(), self.negotiated_protocol)
    }

    /// Returns a shared reference to the replaying stream.
    #[must_use]
    pub const fn stream(&self) -> &NegotiatedStream<R> {
        &self.stream
    }

    /// Returns a mutable reference to the replaying stream.
    #[must_use]
    pub fn stream_mut(&mut self) -> &mut NegotiatedStream<R> {
        &mut self.stream
    }

    /// Releases the handshake wrapper and returns the replaying stream.
    #[must_use]
    pub fn into_stream(self) -> NegotiatedStream<R> {
        self.stream
    }

    /// Decomposes the handshake into its components.
    #[must_use]
    pub fn into_components(
        self,
    ) -> (
        LegacyDaemonGreetingOwned,
        ProtocolVersion,
        NegotiatedStream<R>,
    ) {
        (self.server_greeting, self.negotiated_protocol, self.stream)
    }

    /// Decomposes the handshake into a [`LegacyDaemonHandshakeParts`] structure.
    #[must_use]
    pub fn into_parts(self) -> LegacyDaemonHandshakeParts<R> {
        let (server_greeting, negotiated_protocol, parts) = self.into_stream_parts();
        LegacyDaemonHandshakeParts::from_components(server_greeting, negotiated_protocol, parts)
    }

    /// Rehydrates a [`NegotiationPrologueSniffer`] using the captured negotiation snapshot.
    ///
    /// The helper mirrors the functionality exposed by
    /// [`LegacyDaemonHandshakeParts::stream_parts`], reusing
    /// [`NegotiationPrologueSniffer::rehydrate_from_parts`] with the buffered transcript captured
    /// during negotiation. Callers can therefore rebuild sniffers without decomposing the session
    /// into parts or replaying the underlying transport.
    pub fn rehydrate_sniffer(
        &self,
        sniffer: &mut NegotiationPrologueSniffer,
    ) -> Result<(), TryReserveError> {
        sniffer.rehydrate_from_parts(
            self.stream.decision(),
            self.stream.sniffed_prefix_len(),
            self.stream.buffered(),
        )
    }

    /// Reconstructs a [`LegacyDaemonHandshake`] from previously extracted parts.
    #[must_use]
    pub fn from_parts(parts: LegacyDaemonHandshakeParts<R>) -> Self {
        let (server_greeting, negotiated_protocol, parts) = parts.into_components();
        Self::from_stream_parts(server_greeting, negotiated_protocol, parts)
    }

    /// Reconstructs a [`LegacyDaemonHandshake`] from the parsed greeting, negotiated protocol,
    /// and replaying stream returned by [`Self::into_components`].
    ///
    /// Debug builds assert that the supplied stream captured a legacy ASCII negotiation so a binary
    /// session cannot be rewrapped accidentally. Higher layers can therefore temporarily take
    /// ownership of the components—for example to wrap the transport with timeouts—before
    /// reassembling the handshake without replaying the daemon greeting.
    #[must_use]
    pub fn from_components(
        server_greeting: LegacyDaemonGreetingOwned,
        negotiated_protocol: ProtocolVersion,
        stream: NegotiatedStream<R>,
    ) -> Self {
        debug_assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);

        Self {
            stream,
            server_greeting,
            negotiated_protocol,
        }
    }

    /// Maps the inner transport while keeping the negotiated metadata intact.
    ///
    /// The helper mirrors [`NegotiatedStream::map_inner`], making it convenient to
    /// wrap the transport with instrumentation or adapters (for example timeout
    /// guards) after the handshake completes. The sniffed negotiation prefix and
    /// buffered bytes remain available so higher layers can resume protocol
    /// processing without re-reading the greeting.
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> LegacyDaemonHandshake<T>
    where
        F: FnOnce(R) -> T,
    {
        let Self {
            stream,
            server_greeting,
            negotiated_protocol,
        } = self;

        LegacyDaemonHandshake {
            stream: stream.map_inner(map),
            server_greeting,
            negotiated_protocol,
        }
    }

    /// Attempts to transform the inner transport while keeping the negotiated metadata intact.
    ///
    /// The closure returns the replacement reader on success or a tuple containing the error and
    /// original reader on failure, matching [`NegotiatedStream::try_map_inner`].
    pub fn try_map_stream_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<LegacyDaemonHandshake<T>, TryMapInnerError<LegacyDaemonHandshake<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        let Self {
            stream,
            server_greeting,
            negotiated_protocol,
        } = self;

        match stream.try_map_inner(map) {
            Ok(stream) => Ok(LegacyDaemonHandshake {
                stream,
                server_greeting,
                negotiated_protocol,
            }),
            Err(err) => Err(err.map_original(|stream| LegacyDaemonHandshake {
                stream,
                server_greeting,
                negotiated_protocol,
            })),
        }
    }

    /// Decomposes the handshake into the parsed greeting, negotiated protocol, and replaying stream parts.
    ///
    /// Returning [`NegotiatedStreamParts`] mirrors the convenience provided by [`Self::into_stream`]
    /// while giving callers access to the buffered negotiation bytes without immediately
    /// reconstructing a [`NegotiatedStream`]. This is useful when temporary ownership of the
    /// underlying transport is required (for example to wrap it with a timeout adapter) before the
    /// rsync daemon exchange continues.
    #[must_use]
    pub fn into_stream_parts(
        self,
    ) -> (
        LegacyDaemonGreetingOwned,
        ProtocolVersion,
        NegotiatedStreamParts<R>,
    ) {
        let Self {
            stream,
            server_greeting,
            negotiated_protocol,
        } = self;

        (server_greeting, negotiated_protocol, stream.into_parts())
    }

    /// Reconstructs a [`LegacyDaemonHandshake`] from previously extracted stream parts.
    ///
    /// This helper complements [`Self::into_stream_parts`] by allowing higher layers to stash the
    /// parsed greeting and negotiated protocol while temporarily taking ownership of the
    /// [`NegotiatedStreamParts`]. Once the caller has finished wrapping or inspecting the underlying
    /// transport they can rebuild the handshake without replaying the daemon's greeting or
    /// re-parsing any metadata. The negotiation decision is asserted in debug builds to catch
    /// accidental misuse where binary session parts are supplied.
    #[must_use]
    pub fn from_stream_parts(
        server_greeting: LegacyDaemonGreetingOwned,
        negotiated_protocol: ProtocolVersion,
        parts: NegotiatedStreamParts<R>,
    ) -> Self {
        debug_assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);

        Self {
            stream: parts.into_stream(),
            server_greeting,
            negotiated_protocol,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff_negotiation_stream;
    use std::io::{self, Cursor};

    fn create_test_handshake() -> LegacyDaemonHandshake<Cursor<Vec<u8>>> {
        let greeting = LegacyDaemonGreetingOwned::from_parts(31, Some(0), None)
            .expect("valid greeting");
        let proto = ProtocolVersion::from_supported(31).unwrap();
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        LegacyDaemonHandshake::from_components(greeting, proto, stream)
    }

    // ==== Protocol version accessors ====

    #[test]
    fn negotiated_protocol_returns_version() {
        let hs = create_test_handshake();
        assert_eq!(hs.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn local_advertised_protocol_mirrors_negotiated() {
        let hs = create_test_handshake();
        assert_eq!(hs.local_advertised_protocol(), hs.negotiated_protocol());
    }

    #[test]
    fn server_protocol_from_greeting() {
        let hs = create_test_handshake();
        assert_eq!(hs.server_protocol().as_u8(), 31);
    }

    #[test]
    fn remote_advertised_protocol_raw() {
        let hs = create_test_handshake();
        assert_eq!(hs.remote_advertised_protocol(), 31);
    }

    #[test]
    fn remote_protocol_was_clamped_false_when_supported() {
        let hs = create_test_handshake();
        assert!(!hs.remote_protocol_was_clamped());
    }

    #[test]
    fn remote_advertisement_returns_classification() {
        let hs = create_test_handshake();
        let adv = hs.remote_advertisement();
        assert!(!adv.was_clamped());
    }

    // ==== Server greeting ====

    #[test]
    fn server_greeting_returns_reference() {
        let hs = create_test_handshake();
        let greeting = hs.server_greeting();
        assert_eq!(greeting.protocol().as_u8(), 31);
    }

    // ==== Stream accessors ====

    #[test]
    fn stream_returns_shared_reference() {
        let hs = create_test_handshake();
        let stream = hs.stream();
        assert!(stream.buffered_len() > 0);
    }

    #[test]
    fn stream_mut_returns_mutable_reference() {
        let mut hs = create_test_handshake();
        let stream = hs.stream_mut();
        assert!(stream.buffered_len() > 0);
    }

    #[test]
    fn into_stream_returns_owned_stream() {
        let hs = create_test_handshake();
        let stream = hs.into_stream();
        assert!(stream.buffered_len() > 0);
    }

    // ==== Decomposition ====

    #[test]
    fn into_components_returns_all_parts() {
        let hs = create_test_handshake();
        let (greeting, proto, stream) = hs.into_components();
        assert_eq!(greeting.protocol().as_u8(), 31);
        assert_eq!(proto.as_u8(), 31);
        assert!(stream.buffered_len() > 0);
    }

    #[test]
    fn into_parts_returns_handshake_parts() {
        let hs = create_test_handshake();
        let parts = hs.into_parts();
        assert_eq!(parts.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn into_stream_parts_returns_components() {
        let hs = create_test_handshake();
        let (greeting, proto, parts) = hs.into_stream_parts();
        assert_eq!(greeting.protocol().as_u8(), 31);
        assert_eq!(proto.as_u8(), 31);
        assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
    }

    // ==== Reconstruction ====

    #[test]
    fn from_components_reconstructs_handshake() {
        let greeting = LegacyDaemonGreetingOwned::from_parts(31, Some(0), None)
            .expect("valid greeting");
        let proto = ProtocolVersion::from_supported(31).unwrap();
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let hs = LegacyDaemonHandshake::from_components(greeting, proto, stream);
        assert_eq!(hs.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn from_parts_reconstructs_from_handshake_parts() {
        let hs = create_test_handshake();
        let parts = hs.into_parts();
        let reconstructed = LegacyDaemonHandshake::from_parts(parts);
        assert_eq!(reconstructed.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn from_stream_parts_reconstructs_handshake() {
        let hs = create_test_handshake();
        let (greeting, proto, parts) = hs.into_stream_parts();
        let reconstructed = LegacyDaemonHandshake::from_stream_parts(greeting, proto, parts);
        assert_eq!(reconstructed.negotiated_protocol().as_u8(), 31);
    }

    // ==== Mapping ====

    #[test]
    fn map_stream_inner_transforms_transport() {
        let hs = create_test_handshake();
        let mapped = hs.map_stream_inner(|cursor| {
            let pos = cursor.position();
            let mut new_cursor = Cursor::new(cursor.into_inner());
            new_cursor.set_position(pos);
            new_cursor
        });
        assert_eq!(mapped.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn try_map_stream_inner_succeeds() {
        let hs = create_test_handshake();
        let result = hs.try_map_stream_inner(|cursor| -> Result<_, (io::Error, _)> { Ok(cursor) });
        assert!(result.is_ok());
        let mapped = result.unwrap();
        assert_eq!(mapped.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn try_map_stream_inner_fails_preserves_handshake() {
        let hs = create_test_handshake();
        let result = hs.try_map_stream_inner(|cursor| -> Result<Cursor<Vec<u8>>, _> {
            Err((io::Error::new(io::ErrorKind::Other, "test error"), cursor))
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.error().kind(), io::ErrorKind::Other);
        let recovered = err.into_original();
        assert_eq!(recovered.negotiated_protocol().as_u8(), 31);
    }

    // ==== Rehydrate sniffer ====

    #[test]
    fn rehydrate_sniffer_succeeds() {
        let hs = create_test_handshake();
        let mut sniffer = NegotiationPrologueSniffer::new();
        let result = hs.rehydrate_sniffer(&mut sniffer);
        assert!(result.is_ok());
    }

    // ==== Clone and Debug ====

    #[test]
    fn clone_produces_independent_copy() {
        let hs = create_test_handshake();
        let cloned = hs.clone();
        assert_eq!(hs.negotiated_protocol(), cloned.negotiated_protocol());
    }

    #[test]
    fn debug_format_contains_type_name() {
        let hs = create_test_handshake();
        let debug = format!("{hs:?}");
        assert!(debug.contains("LegacyDaemonHandshake"));
    }

    // ==== Local protocol capping ====

    #[test]
    fn local_protocol_was_capped_true_when_reduced() {
        // Create handshake where server advertises 31 but we negotiate 29
        let greeting = LegacyDaemonGreetingOwned::from_parts(31, Some(0), None)
            .expect("valid greeting");
        let proto = ProtocolVersion::from_supported(29).unwrap();
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let hs = LegacyDaemonHandshake::from_components(greeting, proto, stream);
        assert!(hs.local_protocol_was_capped());
    }

    #[test]
    fn local_protocol_was_capped_false_when_not_reduced() {
        let hs = create_test_handshake();
        assert!(!hs.local_protocol_was_capped());
    }
}
