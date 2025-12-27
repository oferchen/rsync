use crate::binary::{BinaryHandshake, BinaryHandshakeParts};
use crate::daemon::{LegacyDaemonHandshake, LegacyDaemonHandshakeParts};
use crate::negotiation::{NegotiatedStream, NegotiatedStreamParts, TryMapInnerError};
use protocol::NegotiationPrologueSniffer;
use std::collections::TryReserveError;

use super::SessionHandshakeParts;
use super::state::{BinaryHandshakeComponents, HandshakePartsResult, LegacyHandshakeComponents};

impl<R> SessionHandshakeParts<R> {
    /// Releases the parts structure and returns the replaying stream parts captured during negotiation.
    ///
    /// The returned [`NegotiatedStreamParts`] retain the buffered prologue, decision, and transport,
    /// allowing callers to inspect or transform the replay data without first rebuilding a
    /// [`crate::session::SessionHandshake`]. This mirrors [`Self::stream`] for owned access and keeps the
    /// high-level API aligned with the variant-specific helpers exposed by
    /// [`BinaryHandshakeParts`] and [`LegacyDaemonHandshakeParts`].
    ///
    /// # Examples
    ///
    /// Reconstruct a binary negotiation and extract the replaying stream parts while preserving the
    /// buffered handshake prefix.
    ///
    /// ```
    /// use protocol::ProtocolVersion;
    /// use rsync_io::negotiate_session;
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
                let (
                    remote_advertised,
                    remote_protocol,
                    local_advertised,
                    negotiated,
                    remote_compatibility_flags,
                    stream,
                ) = parts.into_components();
                Ok((
                    remote_advertised,
                    remote_protocol,
                    local_advertised,
                    negotiated,
                    remote_compatibility_flags,
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
    /// This mirrors [`crate::session::SessionHandshake::local_protocol_was_capped`] while operating on the decomposed
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

    /// Reassembles a [`crate::session::SessionHandshake`] from the stored components.
    #[must_use]
    pub fn into_handshake(self) -> crate::session::SessionHandshake<R> {
        match self {
            SessionHandshakeParts::Binary(parts) => {
                crate::session::SessionHandshake::Binary(BinaryHandshake::from_parts(parts))
            }
            SessionHandshakeParts::Legacy(parts) => {
                crate::session::SessionHandshake::Legacy(LegacyDaemonHandshake::from_parts(parts))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff_negotiation_stream;
    use protocol::{CompatibilityFlags, LegacyDaemonGreetingOwned, ProtocolVersion};
    use std::io::Cursor;

    // Helper to create binary handshake parts
    fn create_binary_parts() -> SessionHandshakeParts<Cursor<Vec<u8>>> {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        SessionHandshakeParts::from_binary_components(
            31,
            proto31,
            proto31,
            proto31,
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        )
    }

    // Helper to create legacy handshake parts
    fn create_legacy_parts() -> SessionHandshakeParts<Cursor<Vec<u8>>> {
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let greeting =
            LegacyDaemonGreetingOwned::from_parts(31, Some(0), None).expect("valid greeting");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        SessionHandshakeParts::from_legacy_components(greeting, proto31, stream.into_parts())
    }

    // ==== into_stream_parts tests ====

    #[test]
    fn into_stream_parts_extracts_stream_for_binary() {
        let parts = create_binary_parts();
        let stream_parts = parts.into_stream_parts();
        assert!(!stream_parts.buffered().is_empty());
    }

    #[test]
    fn into_stream_parts_extracts_stream_for_legacy() {
        let parts = create_legacy_parts();
        let stream_parts = parts.into_stream_parts();
        assert!(stream_parts.buffered().starts_with(b"@RSYNCD:"));
    }

    // ==== into_stream tests ====

    #[test]
    fn into_stream_reconstructs_negotiated_stream_for_binary() {
        let parts = create_binary_parts();
        let stream = parts.into_stream();
        // After sniffing, only the first byte was read to determine binary mode
        assert!(stream.buffered_len() > 0);
    }

    #[test]
    fn into_stream_reconstructs_negotiated_stream_for_legacy() {
        let parts = create_legacy_parts();
        let stream = parts.into_stream();
        assert!(stream.buffered_len() > 0);
    }

    // ==== map_stream_inner tests ====

    #[test]
    fn map_stream_inner_transforms_binary_variant() {
        let parts = create_binary_parts();
        let mapped: SessionHandshakeParts<String> =
            parts.map_stream_inner(|cursor| format!("wrapped:{}", cursor.position()));
        assert!(mapped.is_binary());
    }

    #[test]
    fn map_stream_inner_transforms_legacy_variant() {
        let parts = create_legacy_parts();
        let mapped: SessionHandshakeParts<String> =
            parts.map_stream_inner(|cursor| format!("wrapped:{}", cursor.position()));
        assert!(mapped.is_legacy());
    }

    #[test]
    fn map_stream_inner_preserves_decision() {
        let binary = create_binary_parts();
        let mapped = binary.map_stream_inner(Box::new);
        assert!(mapped.is_binary());

        let legacy = create_legacy_parts();
        let mapped = legacy.map_stream_inner(Box::new);
        assert!(mapped.is_legacy());
    }

    // ==== try_map_stream_inner tests ====

    #[test]
    fn try_map_stream_inner_success_for_binary() {
        let parts = create_binary_parts();
        let result = parts.try_map_stream_inner(|cursor| Ok::<_, ((), _)>(Box::new(cursor)));
        assert!(result.is_ok());
        assert!(result.unwrap().is_binary());
    }

    #[test]
    fn try_map_stream_inner_success_for_legacy() {
        let parts = create_legacy_parts();
        let result = parts.try_map_stream_inner(|cursor| Ok::<_, ((), _)>(Box::new(cursor)));
        assert!(result.is_ok());
        assert!(result.unwrap().is_legacy());
    }

    #[test]
    fn try_map_stream_inner_failure_preserves_binary() {
        let parts = create_binary_parts();
        let result = parts.try_map_stream_inner(|cursor| Err::<(), _>(("error", cursor)));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.error(), &"error");
        assert!(err.into_original().is_binary());
    }

    #[test]
    fn try_map_stream_inner_failure_preserves_legacy() {
        let parts = create_legacy_parts();
        let result = parts.try_map_stream_inner(|cursor| Err::<(), _>(("error", cursor)));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.into_original().is_legacy());
    }

    // ==== into_binary tests ====

    #[test]
    fn into_binary_succeeds_for_binary_variant() {
        let parts = create_binary_parts();
        let result = parts.into_binary();
        assert!(result.is_ok());
        let (raw, remote, local, negotiated, flags, _stream) = result.unwrap();
        assert_eq!(raw, 31);
        assert_eq!(remote.as_u8(), 31);
        assert_eq!(local.as_u8(), 31);
        assert_eq!(negotiated.as_u8(), 31);
        assert_eq!(flags, CompatibilityFlags::EMPTY);
    }

    #[test]
    fn into_binary_fails_for_legacy_variant() {
        let parts = create_legacy_parts();
        let result = parts.into_binary();
        assert!(result.is_err());
        assert!(result.unwrap_err().is_legacy());
    }

    // ==== into_binary_parts tests ====

    #[test]
    fn into_binary_parts_succeeds_for_binary_variant() {
        let parts = create_binary_parts();
        let result = parts.into_binary_parts();
        assert!(result.is_ok());
    }

    #[test]
    fn into_binary_parts_fails_for_legacy_variant() {
        let parts = create_legacy_parts();
        let result = parts.into_binary_parts();
        assert!(result.is_err());
    }

    // ==== into_legacy tests ====

    #[test]
    fn into_legacy_succeeds_for_legacy_variant() {
        let parts = create_legacy_parts();
        let result = parts.into_legacy();
        assert!(result.is_ok());
        let (greeting, negotiated, _stream) = result.unwrap();
        assert_eq!(greeting.advertised_protocol(), 31);
        assert_eq!(negotiated.as_u8(), 31);
    }

    #[test]
    fn into_legacy_fails_for_binary_variant() {
        let parts = create_binary_parts();
        let result = parts.into_legacy();
        assert!(result.is_err());
        assert!(result.unwrap_err().is_binary());
    }

    // ==== into_legacy_parts tests ====

    #[test]
    fn into_legacy_parts_succeeds_for_legacy_variant() {
        let parts = create_legacy_parts();
        let result = parts.into_legacy_parts();
        assert!(result.is_ok());
    }

    #[test]
    fn into_legacy_parts_fails_for_binary_variant() {
        let parts = create_binary_parts();
        let result = parts.into_legacy_parts();
        assert!(result.is_err());
    }

    // ==== remote_protocol_was_clamped tests ====

    #[test]
    fn remote_protocol_was_clamped_false_for_supported_binary() {
        let parts = create_binary_parts();
        assert!(!parts.remote_protocol_was_clamped());
    }

    #[test]
    fn remote_protocol_was_clamped_false_for_supported_legacy() {
        let parts = create_legacy_parts();
        assert!(!parts.remote_protocol_was_clamped());
    }

    #[test]
    fn remote_protocol_was_clamped_true_for_unsupported_binary() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let proto30 = ProtocolVersion::from_supported(30).unwrap();
        // remote_protocol (30) < raw (31) indicates clamping
        let parts = SessionHandshakeParts::from_binary_components(
            999,     // Raw unsupported value
            proto30, // Clamped to 30
            proto31,
            proto30,
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        );
        assert!(parts.remote_protocol_was_clamped());
    }

    // ==== local_protocol_was_capped tests ====

    #[test]
    fn local_protocol_was_capped_false_when_equal() {
        let parts = create_binary_parts();
        assert!(!parts.local_protocol_was_capped());
    }

    #[test]
    fn local_protocol_was_capped_true_when_negotiated_lower() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let proto30 = ProtocolVersion::from_supported(30).unwrap();
        let parts = SessionHandshakeParts::from_binary_components(
            31,
            proto31,
            proto30, // local advertised lower
            proto30, // negotiated at local
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        );
        // Capped if negotiated < remote
        assert!(parts.local_protocol_was_capped());
    }

    // ==== into_handshake tests ====

    #[test]
    fn into_handshake_rebuilds_binary_handshake() {
        let parts = create_binary_parts();
        let handshake = parts.into_handshake();
        assert!(handshake.is_binary());
    }

    #[test]
    fn into_handshake_rebuilds_legacy_handshake() {
        let parts = create_legacy_parts();
        let handshake = parts.into_handshake();
        assert!(handshake.is_legacy());
    }

    #[test]
    fn into_handshake_preserves_negotiated_protocol() {
        let parts = create_binary_parts();
        let negotiated = parts.negotiated_protocol();
        let handshake = parts.into_handshake();
        assert_eq!(handshake.negotiated_protocol(), negotiated);
    }

    // ==== into_inner tests ====

    #[test]
    fn into_inner_extracts_transport_for_binary() {
        let parts = create_binary_parts();
        let inner: Cursor<Vec<u8>> = parts.into_inner();
        // The position may have advanced during sniffing
        assert!(!inner.get_ref().is_empty());
    }

    #[test]
    fn into_inner_extracts_transport_for_legacy() {
        let parts = create_legacy_parts();
        let inner: Cursor<Vec<u8>> = parts.into_inner();
        // The position may have advanced during sniffing
        assert!(!inner.get_ref().is_empty());
    }
}
