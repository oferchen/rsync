use super::handshake::LegacyDaemonHandshake;
use crate::handshake_util::{RemoteProtocolAdvertisement, local_cap_reduced_protocol};
use crate::negotiation::{NegotiatedStreamParts, TryMapInnerError};
use protocol::{LegacyDaemonGreetingOwned, ProtocolVersion};

/// Decomposed components of a [`LegacyDaemonHandshake`].
///
/// The structure groups the parsed greeting, negotiated protocol, and replaying
/// stream parts so callers can temporarily take ownership of the components
/// while instrumenting the transport.
///
/// # Examples
///
/// ```
/// use protocol::ProtocolVersion;
/// use transport::{negotiate_legacy_daemon_session, LegacyDaemonHandshakeParts};
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

    /// Returns the protocol version the client advertised to the daemon.
    ///
    /// For the legacy handshake the client echoes the final negotiated protocol back to the server, so
    /// the value mirrors [`Self::negotiated_protocol`] but is exposed explicitly to keep the API shape
    /// aligned with the binary negotiation helpers.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_advertised_protocol(&self) -> ProtocolVersion {
        self.negotiated_protocol
    }

    /// Reports whether the daemon advertised a protocol newer than the supported range.
    ///
    /// The helper mirrors [`LegacyDaemonHandshake::remote_protocol_was_clamped`] so callers that
    /// operate on the decomposed parts retain access to the same diagnostics without rebuilding the
    /// wrapper first.
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

    /// Reports whether the negotiated protocol was reduced by the caller-provided cap.
    ///
    /// This complements [`LegacyDaemonHandshake::local_protocol_was_capped`] when higher layers
    /// inspect the parts structure before reconstructing the handshake. The check mirrors
    /// `rsync --protocol=<version>`: if the caller requests an older protocol than the daemon
    /// advertised, the negotiated session is forced to run at the downgraded version.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_protocol_was_capped(&self) -> bool {
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
    /// use protocol::ProtocolVersion;
    /// use transport::{negotiate_legacy_daemon_session, LegacyDaemonHandshakeParts};
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
    /// use protocol::ProtocolVersion;
    /// use transport::{negotiate_legacy_daemon_session, LegacyDaemonHandshakeParts};
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
    /// use protocol::{NegotiationPrologue, ProtocolVersion};
    /// use transport::negotiate_legacy_daemon_session;
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
    /// assert_eq!(stream_parts.sniffed_prefix_len(), protocol::LEGACY_DAEMON_PREFIX_LEN);
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

    pub(super) fn from_components(
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
