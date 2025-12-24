use crate::binary::{BinaryHandshake, BinaryHandshakeParts};
use crate::daemon::{LegacyDaemonHandshake, LegacyDaemonHandshakeParts};
use crate::negotiation::NegotiatedStreamParts;
use protocol::{CompatibilityFlags, LegacyDaemonGreetingOwned, ProtocolVersion};

pub(super) type BinaryHandshakeComponents<R> = (
    u32,
    ProtocolVersion,
    ProtocolVersion,
    ProtocolVersion,
    CompatibilityFlags,
    NegotiatedStreamParts<R>,
);

pub(super) type LegacyHandshakeComponents<R> = (
    LegacyDaemonGreetingOwned,
    ProtocolVersion,
    NegotiatedStreamParts<R>,
);

pub(super) type HandshakePartsResult<T, R> = Result<T, SessionHandshakeParts<R>>;

/// Components extracted from a [crate::session::SessionHandshake].
///
/// The structure mirrors the variant-specific handshake wrappers so callers can
/// temporarily take ownership of the buffered negotiation bytes while keeping
/// the negotiated protocol metadata. The parts can be converted back into a
/// [crate::session::SessionHandshake] via [crate::session::SessionHandshake::from_stream_parts] or the
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
    /// use protocol::ProtocolVersion;
    /// use rsync_io::{negotiate_session_parts, SessionHandshakeParts};
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
    ///     remote_flags,
    ///     stream_parts,
    /// ) = parts.clone().into_binary().expect("binary components");
    ///
    /// let rebuilt = SessionHandshakeParts::from_binary_components(
    ///     remote_advertised,
    ///     remote_protocol,
    ///     local_advertised,
    ///     negotiated,
    ///     remote_flags,
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
        remote_compatibility_flags: CompatibilityFlags,
        stream: NegotiatedStreamParts<R>,
    ) -> Self {
        SessionHandshakeParts::Binary(
            BinaryHandshake::from_stream_parts(
                remote_advertised,
                remote_protocol,
                local_advertised,
                negotiated_protocol,
                remote_compatibility_flags,
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
    /// use protocol::ProtocolVersion;
    /// use rsync_io::{negotiate_session_parts, SessionHandshakeParts};
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
}
