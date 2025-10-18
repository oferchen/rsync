use crate::handshake_util::{local_cap_reduced_protocol, remote_advertisement_was_clamped};
use crate::negotiation::{
    NegotiatedStream, NegotiatedStreamParts, TryMapInnerError, sniff_negotiation_stream,
    sniff_negotiation_stream_with_sniffer,
};
use core::fmt::{self, Write as FmtWrite};
use rsync_protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonGreetingOwned, NegotiationPrologue,
    NegotiationPrologueSniffer, ProtocolVersion, write_legacy_daemon_greeting,
};
use std::cmp;
use std::collections::TryReserveError;
use std::io::{self, Read, Write};

const LEGACY_GREETING_BUFFER_CAPACITY: usize = LEGACY_DAEMON_PREFIX_LEN + 7;

/// Stack-allocated buffer used to render the legacy daemon greeting without allocating.
///
/// The buffer keeps previously written bytes until [`Self::clear`]
/// is invoked, making it inexpensive to reuse across multiple negotiations when the
/// caller wants to avoid repeated stack initialisation.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LegacyGreetingBuffer {
    buf: [u8; LEGACY_GREETING_BUFFER_CAPACITY],
    len: usize,
}

impl LegacyGreetingBuffer {
    const fn new() -> Self {
        Self {
            buf: [0; LEGACY_GREETING_BUFFER_CAPACITY],
            len: 0,
        }
    }

    /// Reports whether the buffer is empty.
    #[must_use]
    const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Clears the buffer without zeroing the underlying storage.
    fn clear(&mut self) {
        self.len = 0;
    }

    #[must_use]
    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Renders a canonical legacy daemon greeting into the buffer.
    ///
    /// The helper clears any existing contents before delegating to
    /// [`write_legacy_daemon_greeting`], ensuring reused buffers never retain
    /// stale bytes from previous negotiations. On success the borrowed slice
    /// exposes the freshly written banner; on failure the buffer is cleared so
    /// callers can retry without observing partially formatted data.
    #[must_use = "callers typically forward the rendered greeting to the daemon"]
    fn render_greeting(&mut self, version: ProtocolVersion) -> Result<&[u8], fmt::Error> {
        self.clear();

        match write_legacy_daemon_greeting(self, version) {
            Ok(()) => {
                debug_assert!(
                    !self.is_empty(),
                    "successful greeting rendering must populate the buffer",
                );
                Ok(self.as_bytes())
            }
            Err(err) => {
                self.clear();
                Err(err)
            }
        }
    }
}

impl Default for LegacyGreetingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl AsRef<[u8]> for LegacyGreetingBuffer {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl FmtWrite for LegacyGreetingBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let Some(end) = self.len.checked_add(bytes.len()) else {
            return Err(fmt::Error);
        };

        if end > self.buf.len() {
            return Err(fmt::Error);
        }

        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    fn write_char(&mut self, ch: char) -> fmt::Result {
        let mut encoded = [0u8; 4];
        let encoded = ch.encode_utf8(&mut encoded);
        self.write_str(encoded)
    }
}

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

/// Decomposed components of a [`LegacyDaemonHandshake`].
///
/// The structure groups the parsed greeting, negotiated protocol, and replaying
/// stream parts so callers can temporarily take ownership of the components
/// while instrumenting the transport.
///
/// # Examples
///
/// ```
/// use rsync_protocol::ProtocolVersion;
/// use rsync_transport::{negotiate_legacy_daemon_session, LegacyDaemonHandshakeParts};
/// use std::io::Cursor;
///
/// let transport = Cursor::new(b"@RSYNCD: 31.0\n".to_vec());
/// let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST).unwrap();
///
/// let parts = handshake.into_parts();
/// assert_eq!(
///     parts.server_protocol(),
///     ProtocolVersion::from_supported(31).unwrap()
/// );
///
/// let rebuilt = parts.into_handshake();
/// assert_eq!(
///     rebuilt.server_protocol(),
///     ProtocolVersion::from_supported(31).unwrap()
/// );
/// ```
#[doc(alias = "@RSYNCD")]
#[derive(Clone, Debug)]
pub struct LegacyDaemonHandshakeParts<R> {
    server_greeting: LegacyDaemonGreetingOwned,
    negotiated_protocol: ProtocolVersion,
    stream: NegotiatedStreamParts<R>,
}

impl<R> LegacyDaemonHandshakeParts<R> {
    const fn new(
        server_greeting: LegacyDaemonGreetingOwned,
        negotiated_protocol: ProtocolVersion,
        stream: NegotiatedStreamParts<R>,
    ) -> Self {
        Self {
            server_greeting,
            negotiated_protocol,
            stream,
        }
    }

    /// Returns the parsed daemon greeting advertised by the server.
    #[must_use]
    pub const fn server_greeting(&self) -> &LegacyDaemonGreetingOwned {
        &self.server_greeting
    }

    /// Returns the server protocol after clamping future advertisements.
    #[must_use]
    pub const fn server_protocol(&self) -> ProtocolVersion {
        self.server_greeting.protocol()
    }

    /// Returns the raw protocol number advertised by the daemon.
    #[must_use]
    pub const fn remote_advertised_protocol(&self) -> u32 {
        self.server_greeting.advertised_protocol()
    }

    /// Returns the negotiated protocol after applying the caller's cap.
    #[must_use]
    pub const fn negotiated_protocol(&self) -> ProtocolVersion {
        self.negotiated_protocol
    }

    /// Reports whether the daemon advertised a protocol newer than the supported range.
    ///
    /// The helper mirrors [`LegacyDaemonHandshake::remote_protocol_was_clamped`] so callers that
    /// operate on the decomposed parts retain access to the same diagnostics without rebuilding the
    /// wrapper first.
    #[must_use]
    pub fn remote_protocol_was_clamped(&self) -> bool {
        remote_advertisement_was_clamped(self.remote_advertised_protocol(), self.server_protocol())
    }

    /// Reports whether the negotiated protocol was reduced by the caller-provided cap.
    ///
    /// This complements [`LegacyDaemonHandshake::local_protocol_was_capped`] when higher layers
    /// inspect the parts structure before reconstructing the handshake.
    #[must_use]
    pub fn local_protocol_was_capped(&self) -> bool {
        local_cap_reduced_protocol(self.server_protocol(), self.negotiated_protocol())
    }

    /// Returns the replaying stream parts captured during negotiation.
    #[must_use]
    pub const fn stream_parts(&self) -> &NegotiatedStreamParts<R> {
        &self.stream
    }

    /// Returns a mutable reference to the replaying stream parts.
    #[must_use]
    pub fn stream_parts_mut(&mut self) -> &mut NegotiatedStreamParts<R> {
        &mut self.stream
    }

    /// Releases the structure and returns the replaying stream parts.
    #[must_use]
    pub fn into_stream_parts(self) -> NegotiatedStreamParts<R> {
        self.stream
    }

    /// Maps the inner transport while preserving the negotiated metadata and greeting.
    ///
    /// The helper mirrors [`LegacyDaemonHandshake::map_stream_inner`] but operates on the
    /// decomposed parts, making it convenient to wrap the underlying transport before rebuilding
    /// the handshake. The replay buffer and parsed greeting remain intact so higher layers can
    /// continue processing daemon responses without rerunning the negotiation.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::ProtocolVersion;
    /// use rsync_transport::{negotiate_legacy_daemon_session, LegacyDaemonHandshakeParts};
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
    ///         Self {
    ///             reader: Cursor::new(input.to_vec()),
    ///             written: Vec::new(),
    ///             flushes: 0,
    ///         }
    ///     }
    ///
    ///     fn written(&self) -> &[u8] {
    ///         &self.written
    ///     }
    ///
    ///     fn flushes(&self) -> usize {
    ///         self.flushes
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
    /// #[derive(Debug)]
    /// struct Instrumented {
    ///     inner: MemoryTransport,
    ///     writes: Vec<u8>,
    ///     flushes: usize,
    /// }
    ///
    /// impl Instrumented {
    ///     fn new(inner: MemoryTransport) -> Self {
    ///         Self {
    ///             inner,
    ///             writes: Vec::new(),
    ///             flushes: 0,
    ///         }
    ///     }
    ///
    ///     fn writes(&self) -> &[u8] {
    ///         &self.writes
    ///     }
    ///
    ///     fn flushes(&self) -> usize {
    ///         self.flushes
    ///     }
    ///
    ///     fn into_inner(self) -> MemoryTransport {
    ///         self.inner
    ///     }
    /// }
    ///
    /// impl Read for Instrumented {
    ///     fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    ///         self.inner.read(buf)
    ///     }
    /// }
    ///
    /// impl Write for Instrumented {
    ///     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    ///         self.writes.extend_from_slice(buf);
    ///         self.inner.write(buf)
    ///     }
    ///
    ///     fn flush(&mut self) -> io::Result<()> {
    ///         self.flushes += 1;
    ///         self.inner.flush()
    ///     }
    /// }
    ///
    /// let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    /// let parts = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
    ///     .expect("handshake succeeds")
    ///     .into_parts();
    ///
    /// let instrumented = parts.map_stream_inner(Instrumented::new);
    /// assert_eq!(instrumented.server_protocol(), ProtocolVersion::from_supported(31).unwrap());
    ///
    /// let mut handshake = instrumented.into_handshake();
    /// handshake.stream_mut().write_all(b"OK\n").unwrap();
    /// handshake.stream_mut().flush().unwrap();
    ///
    /// let instrumented = handshake.into_stream().into_inner();
    /// assert_eq!(instrumented.writes(), b"OK\n");
    /// assert_eq!(instrumented.flushes(), 1);
    /// let inner = instrumented.into_inner();
    /// assert_eq!(inner.written(), b"@RSYNCD: 31.0\nOK\n");
    /// ```
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> LegacyDaemonHandshakeParts<T>
    where
        F: FnOnce(R) -> T,
    {
        let Self {
            server_greeting,
            negotiated_protocol,
            stream,
        } = self;

        LegacyDaemonHandshakeParts::from_components(
            server_greeting,
            negotiated_protocol,
            stream.map_inner(map),
        )
    }

    /// Attempts to transform the inner transport while preserving the negotiated metadata.
    ///
    /// On success the new transport replaces the previous one and the replay buffer remains
    /// available. If the mapping fails, the original parts structure is returned alongside the
    /// error so callers can continue using the negotiated session without repeating the handshake.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::ProtocolVersion;
    /// use rsync_transport::{negotiate_legacy_daemon_session, LegacyDaemonHandshakeParts};
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
    ///         Self {
    ///             reader: Cursor::new(input.to_vec()),
    ///             written: Vec::new(),
    ///             flushes: 0,
    ///         }
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
    /// let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    /// let parts = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
    ///     .expect("handshake succeeds")
    ///     .into_parts();
    ///
    /// let err = parts
    ///     .try_map_stream_inner(|inner| -> Result<MemoryTransport, (io::Error, MemoryTransport)> {
    ///         Err((io::Error::new(io::ErrorKind::Other, "wrap failed"), inner))
    ///     })
    ///     .expect_err("mapping fails");
    /// assert_eq!(err.error().kind(), io::ErrorKind::Other);
    ///
    /// let restored = err.into_original().into_handshake();
    /// assert_eq!(restored.server_protocol(), ProtocolVersion::from_supported(31).unwrap());
    /// ```
    #[must_use = "handle the mapped handshake or propagate the error to preserve negotiation state"]
    pub fn try_map_stream_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<LegacyDaemonHandshakeParts<T>, TryMapInnerError<LegacyDaemonHandshakeParts<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        let Self {
            server_greeting,
            negotiated_protocol,
            stream,
        } = self;

        match (stream.try_map_inner(map), server_greeting) {
            (Ok(stream), greeting) => Ok(LegacyDaemonHandshakeParts::from_components(
                greeting,
                negotiated_protocol,
                stream,
            )),
            (Err(err), greeting) => Err(err.map_original(|stream| {
                LegacyDaemonHandshakeParts::from_components(greeting, negotiated_protocol, stream)
            })),
        }
    }

    /// Decomposes the parts structure into the parsed greeting, negotiated protocol, and
    /// replaying stream.
    ///
    /// The tuple mirrors [`LegacyDaemonHandshake::into_components`] but operates on the decomposed
    /// representation returned by [`LegacyDaemonHandshake::into_parts`]. Higher layers can therefore
    /// inspect the greeting metadata or wrap the replaying stream without first rebuilding the full
    /// handshake wrapper.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::{NegotiationPrologue, ProtocolVersion};
    /// use rsync_transport::negotiate_legacy_daemon_session;
    /// use std::io::Cursor;
    ///
    /// let transport = Cursor::new(b"@RSYNCD: 31.0\n".to_vec());
    /// let parts = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
    ///     .expect("legacy negotiation succeeds")
    ///     .into_parts();
    ///
    /// let expected_protocol = parts.server_protocol();
    /// let (greeting, negotiated_protocol, stream_parts) = parts.into_components();
    ///
    /// assert_eq!(greeting.protocol(), expected_protocol);
    /// assert_eq!(negotiated_protocol, expected_protocol);
    /// assert_eq!(stream_parts.decision(), NegotiationPrologue::LegacyAscii);
    /// assert_eq!(stream_parts.sniffed_prefix_len(), rsync_protocol::LEGACY_DAEMON_PREFIX_LEN);
    /// ```
    #[must_use]
    pub fn into_components(
        self,
    ) -> (
        LegacyDaemonGreetingOwned,
        ProtocolVersion,
        NegotiatedStreamParts<R>,
    ) {
        (self.server_greeting, self.negotiated_protocol, self.stream)
    }

    fn from_components(
        server_greeting: LegacyDaemonGreetingOwned,
        negotiated_protocol: ProtocolVersion,
        stream: NegotiatedStreamParts<R>,
    ) -> Self {
        Self::new(server_greeting, negotiated_protocol, stream)
    }

    /// Rebuilds a [`LegacyDaemonHandshake`] from the preserved components.
    #[must_use]
    pub fn into_handshake(self) -> LegacyDaemonHandshake<R> {
        LegacyDaemonHandshake::from_parts(self)
    }
}

impl<R> LegacyDaemonHandshake<R> {
    /// Returns the negotiated protocol version after applying the caller's cap.
    #[must_use]
    pub const fn negotiated_protocol(&self) -> ProtocolVersion {
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
    pub fn remote_protocol_was_clamped(&self) -> bool {
        remote_advertisement_was_clamped(self.remote_advertised_protocol(), self.server_protocol())
    }

    /// Reports whether the caller's desired cap reduced the negotiated protocol version.
    ///
    /// The negotiated protocol equals the minimum of the daemon's advertised protocol and the
    /// caller's requested cap. When the caller limits the session to an older version, certain
    /// protocol features become unavailable. This helper mirrors upstream rsync by exposing that
    /// condition so higher layers can render matching diagnostics.
    #[must_use]
    pub fn local_protocol_was_capped(&self) -> bool {
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

/// Performs the legacy ASCII rsync daemon negotiation.
///
/// The helper mirrors upstream rsync's client behaviour when connecting to an
/// `rsync://` daemon: it sniffs the negotiation prologue, parses the `@RSYNCD:`
/// greeting, clamps the negotiated protocol to the caller-provided cap, and
/// sends the client's greeting line before returning the replaying stream.
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidData`] if the negotiation prologue indicates a
///   binary handshake, which is handled by different transports.
/// - Any I/O error reported while sniffing the prologue, reading the greeting,
///   writing the client's banner, or flushing the stream.
#[doc(alias = "@RSYNCD")]
pub fn negotiate_legacy_daemon_session<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
) -> io::Result<LegacyDaemonHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream(reader)?;
    negotiate_legacy_daemon_session_from_stream(stream, desired_protocol)
}

/// Performs the legacy ASCII negotiation with a caller-supplied sniffer.
///
/// Reusing a [`NegotiationPrologueSniffer`] allows higher layers to amortize
/// allocations when establishing many daemon sessions. The sniffer is reset
/// before any bytes are observed so state from previous negotiations is fully
/// cleared. Behaviour otherwise matches [`negotiate_legacy_daemon_session`].
#[doc(alias = "@RSYNCD")]
pub fn negotiate_legacy_daemon_session_with_sniffer<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<LegacyDaemonHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream_with_sniffer(reader, sniffer)?;
    negotiate_legacy_daemon_session_from_stream(stream, desired_protocol)
}

/// Performs the legacy ASCII negotiation using a pre-sniffed [`NegotiatedStream`].
///
/// This helper accepts the [`NegotiatedStream`] produced by
/// [`sniff_negotiation_stream`] (or its sniffer-backed counterpart) and drives
/// the remainder of the daemon handshake without repeating the prologue
/// detection. The stream is verified to contain the `@RSYNCD:` prefix before the
/// greeting is parsed and echoed back to the server.
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidData`] if the supplied stream does not represent a
///   legacy daemon negotiation or if formatting the client banner fails.
/// - Any I/O error reported while exchanging the greeting with the daemon.
#[doc(alias = "@RSYNCD")]
pub fn negotiate_legacy_daemon_session_from_stream<R>(
    mut stream: NegotiatedStream<R>,
    desired_protocol: ProtocolVersion,
) -> io::Result<LegacyDaemonHandshake<R>>
where
    R: Read + Write,
{
    stream.ensure_decision(
        NegotiationPrologue::LegacyAscii,
        "legacy daemon negotiation requires @RSYNCD: prefix",
    )?;

    let mut line = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN + 32);
    let greeting = stream.read_and_parse_legacy_daemon_greeting_details(&mut line)?;
    let server_greeting = LegacyDaemonGreetingOwned::from(greeting);

    let negotiated_protocol = cmp::min(desired_protocol, server_greeting.protocol());

    let mut banner = LegacyGreetingBuffer::new();
    let bytes = banner.render_greeting(negotiated_protocol).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "failed to format legacy daemon greeting",
        )
    })?;

    if bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "legacy daemon greeting formatter produced empty banner",
        ));
    }

    stream.write_all(bytes)?;
    stream.flush()?;

    Ok(LegacyDaemonHandshake {
        stream,
        server_greeting,
        negotiated_protocol,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use rsync_protocol::{
        NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion,
        format_legacy_daemon_greeting,
    };
    use std::io::{self, Cursor, Read, Write};

    #[derive(Debug)]
    struct MemoryTransport {
        reader: Cursor<Vec<u8>>,
        written: Vec<u8>,
        flushes: usize,
    }

    impl MemoryTransport {
        fn new(input: &[u8]) -> Self {
            Self {
                reader: Cursor::new(input.to_vec()),
                written: Vec::new(),
                flushes: 0,
            }
        }

        fn written(&self) -> &[u8] {
            &self.written
        }

        fn flushes(&self) -> usize {
            self.flushes
        }
    }

    impl Read for MemoryTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reader.read(buf)
        }
    }

    impl Write for MemoryTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct InstrumentedTransport {
        inner: MemoryTransport,
        observed_writes: Vec<u8>,
        flushes: usize,
    }

    impl InstrumentedTransport {
        fn new(inner: MemoryTransport) -> Self {
            Self {
                inner,
                observed_writes: Vec::new(),
                flushes: 0,
            }
        }

        fn writes(&self) -> &[u8] {
            &self.observed_writes
        }

        fn flushes(&self) -> usize {
            self.flushes
        }

        fn into_inner(self) -> MemoryTransport {
            self.inner
        }
    }

    impl Read for InstrumentedTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Write for InstrumentedTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.observed_writes.extend_from_slice(buf);
            self.inner.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            self.inner.flush()
        }
    }

    #[test]
    fn negotiate_legacy_daemon_session_exchanges_banners() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        assert_eq!(
            handshake.negotiated_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );
        assert_eq!(
            handshake.server_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );
        assert_eq!(handshake.server_greeting().advertised_protocol(), 31);
        assert_eq!(handshake.remote_advertised_protocol(), 31);
        assert!(!handshake.remote_protocol_was_clamped());
        assert!(!handshake.local_protocol_was_capped());

        let transport = handshake.into_stream().into_inner();
        assert_eq!(transport.written(), b"@RSYNCD: 31.0\n");
        assert_eq!(transport.flushes(), 1);
    }

    #[test]
    fn negotiate_respects_requested_protocol_cap() {
        let transport = MemoryTransport::new(b"@RSYNCD: 32.0\n");
        let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
        let handshake =
            negotiate_legacy_daemon_session(transport, desired).expect("handshake should succeed");

        assert_eq!(handshake.negotiated_protocol(), desired);
        assert_eq!(
            handshake.server_protocol(),
            ProtocolVersion::from_supported(32).expect("protocol 32 supported"),
        );
        assert_eq!(handshake.remote_advertised_protocol(), 32);
        assert!(!handshake.remote_protocol_was_clamped());
        assert!(handshake.local_protocol_was_capped());

        let parts = handshake.into_parts();
        assert!(!parts.remote_protocol_was_clamped());
        assert!(parts.local_protocol_was_capped());

        let transport = parts.into_handshake().into_stream().into_inner();
        assert_eq!(transport.written(), b"@RSYNCD: 30.0\n");
    }

    #[test]
    fn negotiate_clamps_future_advertisement() {
        let transport = MemoryTransport::new(b"@RSYNCD: 40.0\n");
        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        assert_eq!(handshake.server_greeting().advertised_protocol(), 40);
        assert_eq!(handshake.remote_advertised_protocol(), 40);
        assert_eq!(handshake.server_protocol(), ProtocolVersion::NEWEST);
        assert_eq!(handshake.negotiated_protocol(), ProtocolVersion::NEWEST);
        assert!(handshake.remote_protocol_was_clamped());
        assert!(!handshake.local_protocol_was_capped());

        let parts = handshake.into_parts();
        assert!(parts.remote_protocol_was_clamped());
        assert!(!parts.local_protocol_was_capped());

        let transport = parts.into_handshake().into_stream().into_inner();
        assert_eq!(transport.written(), b"@RSYNCD: 32.0\n");
    }

    #[test]
    fn negotiate_clamps_large_future_advertisement() {
        let transport = MemoryTransport::new(b"@RSYNCD: 999.0\n");
        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        assert_eq!(handshake.server_greeting().advertised_protocol(), 999);
        assert_eq!(handshake.remote_advertised_protocol(), 999);
        assert_eq!(handshake.server_protocol(), ProtocolVersion::NEWEST);
        assert_eq!(handshake.negotiated_protocol(), ProtocolVersion::NEWEST);
        assert!(handshake.remote_protocol_was_clamped());
        assert!(!handshake.local_protocol_was_capped());

        let parts = handshake.into_parts();
        assert!(parts.remote_protocol_was_clamped());
        assert!(!parts.local_protocol_was_capped());

        let transport = parts.into_handshake().into_stream().into_inner();
        assert_eq!(transport.written(), b"@RSYNCD: 32.0\n");
    }

    #[test]
    fn negotiate_clamps_max_u32_advertisement() {
        let transport = MemoryTransport::new(b"@RSYNCD: 4294967295.0\n");
        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        assert_eq!(handshake.server_greeting().advertised_protocol(), u32::MAX);
        assert_eq!(handshake.remote_advertised_protocol(), u32::MAX);
        assert_eq!(handshake.server_protocol(), ProtocolVersion::NEWEST);
        assert_eq!(handshake.negotiated_protocol(), ProtocolVersion::NEWEST);
        assert!(handshake.remote_protocol_was_clamped());
        assert!(!handshake.local_protocol_was_capped());

        let parts = handshake.into_parts();
        assert!(parts.remote_protocol_was_clamped());
        assert!(!parts.local_protocol_was_capped());

        let transport = parts.into_handshake().into_stream().into_inner();
        assert_eq!(transport.written(), b"@RSYNCD: 32.0\n");
    }

    #[test]
    fn negotiate_rejects_binary_prefix() {
        let transport = MemoryTransport::new(&[0x00, 0x20, 0x00, 0x00]);
        match negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST) {
            Ok(_) => panic!("binary negotiation is rejected"),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::InvalidData),
        }
    }

    #[test]
    fn map_stream_inner_preserves_state_and_transforms_transport() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        assert!(!handshake.local_protocol_was_capped());
        let mut handshake = handshake.map_stream_inner(InstrumentedTransport::new);
        handshake
            .stream_mut()
            .write_all(b"@RSYNCD: OK\n")
            .expect("write propagates");
        handshake.stream_mut().flush().expect("flush propagates");

        assert_eq!(
            handshake.negotiated_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );
        assert_eq!(
            handshake.server_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );
        assert!(!handshake.local_protocol_was_capped());

        let instrumented = handshake.into_stream().into_inner();
        assert_eq!(instrumented.writes(), b"@RSYNCD: OK\n");
        assert_eq!(instrumented.flushes(), 1);

        let inner = instrumented.into_inner();
        assert_eq!(inner.flushes(), 2);
        assert_eq!(inner.written(), b"@RSYNCD: 31.0\n@RSYNCD: OK\n");
    }

    #[test]
    fn into_parts_round_trips_legacy_handshake() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        let parts = handshake.into_parts();
        let expected_protocol = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
        assert_eq!(parts.server_protocol(), expected_protocol);
        assert_eq!(parts.negotiated_protocol(), expected_protocol);
        assert_eq!(parts.remote_advertised_protocol(), 31);
        assert!(!parts.remote_protocol_was_clamped());
        assert!(!parts.local_protocol_was_capped());
        assert_eq!(
            parts.stream_parts().decision(),
            NegotiationPrologue::LegacyAscii
        );

        let mut rebuilt = parts.into_handshake();
        assert_eq!(rebuilt.server_protocol(), expected_protocol);
        assert_eq!(rebuilt.negotiated_protocol(), expected_protocol);

        rebuilt
            .stream_mut()
            .write_all(b"@RSYNCD: OK\n")
            .expect("write propagates");
        rebuilt.stream_mut().flush().expect("flush propagates");

        let transport = rebuilt.into_stream().into_inner();
        assert_eq!(transport.flushes(), 2);
        assert_eq!(transport.written(), b"@RSYNCD: 31.0\n@RSYNCD: OK\n");
    }

    #[test]
    fn legacy_handshake_rehydrates_sniffer_state() {
        let mut bytes = b"@RSYNCD: 31.0\n".to_vec();
        bytes.extend_from_slice(b"@RSYNCD: OK\n");
        let transport = MemoryTransport::new(&bytes);

        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        let mut sniffer = NegotiationPrologueSniffer::new();
        handshake
            .rehydrate_sniffer(&mut sniffer)
            .expect("rehydration succeeds");

        assert!(sniffer.is_legacy());
        assert_eq!(sniffer.buffered(), handshake.stream().buffered());
        assert_eq!(
            sniffer.sniffed_prefix_len(),
            handshake.stream().sniffed_prefix_len()
        );
    }

    #[test]
    fn try_map_stream_inner_transforms_transport() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        assert!(!handshake.local_protocol_was_capped());
        let mut handshake = handshake
            .try_map_stream_inner(
                |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                    Ok(InstrumentedTransport::new(inner))
                },
            )
            .expect("mapping succeeds");

        assert!(!handshake.local_protocol_was_capped());
        handshake
            .stream_mut()
            .write_all(b"@RSYNCD: OK\n")
            .expect("write propagates");
        handshake.stream_mut().flush().expect("flush propagates");

        let instrumented = handshake.into_stream().into_inner();
        assert_eq!(instrumented.writes(), b"@RSYNCD: OK\n");
        assert_eq!(instrumented.flushes(), 1);
    }

    #[test]
    fn parts_map_stream_inner_transforms_transport() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let parts = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed")
            .into_parts();

        let mapped = parts.map_stream_inner(InstrumentedTransport::new);
        assert_eq!(
            mapped.server_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );

        let mut handshake = mapped.into_handshake();
        handshake
            .stream_mut()
            .write_all(b"OK\n")
            .expect("write propagates");
        handshake.stream_mut().flush().expect("flush propagates");

        let instrumented = handshake.into_stream().into_inner();
        assert_eq!(instrumented.writes(), b"OK\n");
        assert_eq!(instrumented.flushes(), 1);

        let inner = instrumented.into_inner();
        assert_eq!(inner.written(), b"@RSYNCD: 31.0\nOK\n");
    }

    #[test]
    fn parts_try_map_stream_inner_transforms_transport() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let parts = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed")
            .into_parts();

        let mapped = parts
            .try_map_stream_inner(
                |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                    Ok(InstrumentedTransport::new(inner))
                },
            )
            .expect("mapping succeeds");
        assert_eq!(
            mapped.server_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );

        let mut handshake = mapped.into_handshake();
        handshake
            .stream_mut()
            .write_all(b"OK\n")
            .expect("write propagates");

        let instrumented = handshake.into_stream().into_inner();
        assert_eq!(instrumented.writes(), b"OK\n");
    }

    #[test]
    fn parts_try_map_stream_inner_preserves_original_on_error() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let parts = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed")
            .into_parts();

        let err = parts
            .try_map_stream_inner(
                |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                    Err((io::Error::other("wrap failed"), inner))
                },
            )
            .expect_err("mapping fails");
        assert_eq!(err.error().kind(), io::ErrorKind::Other);

        let restored = err.into_original();
        assert_eq!(
            restored.server_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );

        let remapped = restored.map_stream_inner(InstrumentedTransport::new);
        assert_eq!(
            remapped.server_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );
    }

    #[test]
    fn try_map_stream_inner_preserves_original_on_error() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        assert!(!handshake.local_protocol_was_capped());
        let err = handshake
            .try_map_stream_inner(
                |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                    Err((io::Error::other("boom"), inner))
                },
            )
            .expect_err("mapping fails");

        assert_eq!(err.error().kind(), io::ErrorKind::Other);
        let original = err.into_original();
        let transport = original.into_stream().into_inner();
        assert_eq!(transport.written(), b"@RSYNCD: 31.0\n");
    }

    #[test]
    fn negotiate_legacy_daemon_session_with_sniffer_can_be_reused() {
        let transport1 = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let transport2 = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let mut sniffer = NegotiationPrologueSniffer::new();

        let handshake1 = negotiate_legacy_daemon_session_with_sniffer(
            transport1,
            ProtocolVersion::NEWEST,
            &mut sniffer,
        )
        .expect("handshake should succeed with supplied sniffer");
        assert_eq!(
            handshake1.negotiated_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );
        assert_eq!(
            handshake1.server_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );

        drop(handshake1);

        let handshake2 = negotiate_legacy_daemon_session_with_sniffer(
            transport2,
            ProtocolVersion::NEWEST,
            &mut sniffer,
        )
        .expect("sniffer can be reused across sessions");
        assert_eq!(
            handshake2.negotiated_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );
        assert_eq!(
            handshake2.server_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );
    }

    #[test]
    fn into_stream_parts_exposes_legacy_state() {
        let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

        let handshake =
            negotiate_legacy_daemon_session(transport, desired).expect("handshake should succeed");

        assert!(handshake.local_protocol_was_capped());
        let (greeting, negotiated, parts) = handshake.into_stream_parts();
        assert_eq!(greeting.advertised_protocol(), 31);
        assert_eq!(negotiated, desired);
        assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
        assert_eq!(parts.sniffed_prefix(), b"@RSYNCD:");
        assert_eq!(parts.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);
        assert_eq!(parts.buffered_remaining(), 0);

        let mut stream = parts.into_stream();
        let mut tail = Vec::new();
        stream
            .read_to_end(&mut tail)
            .expect("legacy handshake drains buffered prefix");
        assert!(tail.is_empty());

        let transport = stream.into_inner();
        assert_eq!(transport.flushes(), 1);
        assert_eq!(
            transport.written(),
            format_legacy_daemon_greeting(negotiated).as_bytes()
        );
    }

    #[test]
    fn legacy_handshake_parts_into_components_matches_accessors() {
        let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

        let parts = negotiate_legacy_daemon_session(transport, desired)
            .expect("handshake should succeed")
            .into_parts();

        let expected_greeting = parts.server_greeting().clone();
        let expected_negotiated = parts.negotiated_protocol();
        let expected_consumed = parts.stream_parts().buffered_consumed();
        let expected_buffer = parts.stream_parts().buffered().to_vec();

        let (greeting, negotiated, stream_parts) = parts.into_components();

        assert_eq!(greeting, expected_greeting);
        assert_eq!(negotiated, expected_negotiated);
        assert_eq!(stream_parts.decision(), NegotiationPrologue::LegacyAscii);
        assert_eq!(stream_parts.sniffed_prefix(), b"@RSYNCD:");
        assert_eq!(stream_parts.buffered_consumed(), expected_consumed);
        assert_eq!(stream_parts.buffered(), expected_buffer.as_slice());
    }

    #[test]
    fn from_stream_parts_rehydrates_legacy_handshake() {
        let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

        let handshake =
            negotiate_legacy_daemon_session(transport, desired).expect("handshake should succeed");

        assert!(handshake.local_protocol_was_capped());
        let (greeting, negotiated, parts) = handshake.into_stream_parts();
        let greeting_clone = greeting.clone();
        assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);

        let mut rehydrated = LegacyDaemonHandshake::from_stream_parts(greeting, negotiated, parts);

        assert!(rehydrated.local_protocol_was_capped());
        assert_eq!(rehydrated.negotiated_protocol(), negotiated);
        assert_eq!(rehydrated.server_greeting(), &greeting_clone);
        assert_eq!(rehydrated.server_protocol(), greeting_clone.protocol());
        assert_eq!(
            rehydrated.stream().decision(),
            NegotiationPrologue::LegacyAscii
        );

        rehydrated
            .stream_mut()
            .write_all(b"@RSYNCD: OK\n")
            .expect("write propagates");
        rehydrated.stream_mut().flush().expect("flush propagates");

        let transport = rehydrated.into_stream().into_inner();
        assert_eq!(transport.flushes(), 2);

        let mut expected = format_legacy_daemon_greeting(negotiated);
        expected.push_str("@RSYNCD: OK\n");
        assert_eq!(transport.written(), expected.as_bytes());
    }

    #[test]
    fn legacy_handshake_round_trips_from_components() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        let expected_buffer = handshake.stream().buffered().to_vec();
        let greeting = handshake.server_greeting().clone();
        let negotiated_protocol = handshake.negotiated_protocol();
        let stream = handshake.into_stream();

        let rebuilt =
            LegacyDaemonHandshake::from_components(greeting.clone(), negotiated_protocol, stream);

        assert_eq!(rebuilt.server_greeting(), &greeting);
        assert_eq!(rebuilt.negotiated_protocol(), negotiated_protocol);
        assert_eq!(
            rebuilt.stream().decision(),
            NegotiationPrologue::LegacyAscii
        );
        assert_eq!(rebuilt.stream().buffered(), expected_buffer.as_slice());
    }

    #[test]
    fn legacy_greeting_buffer_matches_formatter() {
        let mut buffer = LegacyGreetingBuffer::new();
        let rendered = buffer
            .render_greeting(ProtocolVersion::NEWEST)
            .expect("writing to stack buffer succeeds");

        assert_eq!(
            rendered,
            format_legacy_daemon_greeting(ProtocolVersion::NEWEST).as_bytes()
        );
    }

    #[test]
    fn legacy_greeting_buffer_supports_reuse() {
        let mut buffer = LegacyGreetingBuffer::default();
        let first = buffer
            .render_greeting(ProtocolVersion::NEWEST)
            .expect("writing to stack buffer succeeds")
            .to_vec();

        assert_eq!(buffer.as_bytes().len(), first.len());

        let second = buffer
            .render_greeting(ProtocolVersion::NEWEST)
            .expect("writing after reuse succeeds");
        assert_eq!(second, first.as_slice());
    }

    #[test]
    fn legacy_greeting_buffer_rejects_overflow() {
        let mut buffer = LegacyGreetingBuffer::new();
        let long = "@RSYNCD: 32.0 additional";

        assert!(buffer.write_str(long).is_err());
        assert!(buffer.as_bytes().is_empty());
        assert!(buffer.is_empty());
        assert_eq!(buffer.as_bytes().len(), 0);
    }

    #[test]
    fn legacy_greeting_buffer_detects_internal_len_overflow() {
        let mut buffer = LegacyGreetingBuffer::new();
        buffer.len = usize::MAX;

        let result = buffer.write_str("@RSYNCD: 31.0\n");
        assert!(result.is_err());
        assert_eq!(buffer.len, usize::MAX);
        assert!(buffer.buf.iter().all(|&byte| byte == 0));
    }
}
