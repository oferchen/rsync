use crate::handshake_util::{RemoteProtocolAdvertisement, local_cap_reduced_protocol};
use crate::negotiation::{NegotiatedStreamParts, TryMapInnerError};
use protocol::{CompatibilityFlags, ProtocolVersion};

use super::BinaryHandshake;

/// Decomposed components of a [`BinaryHandshake`].
///
/// The structure groups the negotiated metadata with the replaying stream parts,
/// making it convenient to stage additional instrumentation around the transport
/// before reconstituting the handshake.
///
/// # Examples
///
/// ```
/// use protocol::ProtocolVersion;
/// use transport::{negotiate_binary_session, BinaryHandshakeParts};
/// use std::io::{Cursor, Read, Write};
///
/// #[derive(Debug)]
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
///     fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
///         self.reader.read(buf)
///     }
/// }
///
/// impl Write for Loopback {
///     fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
///         self.written.extend_from_slice(buf);
///         Ok(buf.len())
///     }
///
///     fn flush(&mut self) -> std::io::Result<()> {
///         Ok(())
///     }
/// }
///
/// let protocol = ProtocolVersion::from_supported(31).unwrap();
/// let transport = Loopback::new(u32::from(protocol.as_u8()).to_be_bytes());
/// let handshake = negotiate_binary_session(transport, protocol).unwrap();
///
/// let parts = handshake.into_parts();
/// assert_eq!(parts.remote_protocol(), protocol);
/// assert_eq!(parts.negotiated_protocol(), protocol);
///
/// let rebuilt = parts.into_handshake();
/// assert_eq!(rebuilt.remote_protocol(), protocol);
/// assert_eq!(rebuilt.negotiated_protocol(), protocol);
/// ```
#[derive(Clone, Debug)]
pub struct BinaryHandshakeParts<R> {
    remote_advertisement: RemoteProtocolAdvertisement,
    negotiated_protocol: ProtocolVersion,
    local_advertised: ProtocolVersion,
    remote_compatibility_flags: CompatibilityFlags,
    stream: NegotiatedStreamParts<R>,
}

impl<R> BinaryHandshakeParts<R> {
    const fn new(
        remote_advertisement: RemoteProtocolAdvertisement,
        negotiated_protocol: ProtocolVersion,
        local_advertised: ProtocolVersion,
        remote_compatibility_flags: CompatibilityFlags,
        stream: NegotiatedStreamParts<R>,
    ) -> Self {
        Self {
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
            remote_compatibility_flags,
            stream,
        }
    }

    /// Returns the protocol number advertised by the remote peer before clamping.
    #[must_use]
    pub const fn remote_advertised_protocol(&self) -> u32 {
        self.remote_advertisement.advertised()
    }

    /// Returns the remote protocol version after clamping future advertisements.
    #[must_use]
    pub const fn remote_protocol(&self) -> ProtocolVersion {
        self.remote_advertisement.negotiated()
    }

    /// Returns the negotiated protocol after applying the caller's cap.
    #[must_use]
    pub const fn negotiated_protocol(&self) -> ProtocolVersion {
        self.negotiated_protocol
    }

    /// Returns the protocol version advertised by the local peer before remote negotiation completed.
    ///
    /// Upstream rsync writes the caller's desired protocol (after applying any `--protocol` cap) to the
    /// transport before reading the remote advertisement. Capturing the value here allows higher layers
    /// to surface diagnostics such as "client requested protocol 32 but server replied with 30" without
    /// having to retain the original argument passed to [`crate::negotiate_binary_session`].
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_advertised_protocol(&self) -> ProtocolVersion {
        self.local_advertised
    }

    /// Returns the compatibility flags advertised by the remote peer.
    #[must_use]
    pub const fn remote_compatibility_flags(&self) -> CompatibilityFlags {
        self.remote_compatibility_flags
    }

    /// Reports whether the remote peer advertised a protocol newer than the supported range.
    ///
    /// The helper mirrors [`BinaryHandshake::remote_protocol_was_clamped`] so callers that
    /// temporarily decomposed the handshake via [`BinaryHandshake::into_parts`] retain access to
    /// the same diagnostics without rebuilding the wrapper first.
    #[must_use]
    pub const fn remote_protocol_was_clamped(&self) -> bool {
        self.remote_advertisement().was_clamped()
    }

    /// Returns the classification of the peer's protocol advertisement.
    ///
    /// When the peer announces a protocol within rsync's supported range the
    /// classification contains the negotiated [`ProtocolVersion`]. Future
    /// protocols are reported via the [`RemoteProtocolAdvertisement::Future`]
    /// variant so higher layers can reference the raw number in diagnostics
    /// while still observing that the negotiated session uses
    /// [`ProtocolVersion::NEWEST`].
    #[must_use]
    pub const fn remote_advertisement(&self) -> RemoteProtocolAdvertisement {
        self.remote_advertisement
    }

    /// Reports whether the negotiated protocol was reduced by the caller-specified cap.
    ///
    /// This complements [`BinaryHandshake::local_protocol_was_capped`] for scenarios where the
    /// parts structure is inspected before reconstructing the full handshake. The check mirrors the
    /// behaviour of `rsync --protocol=<version>`: when the user requests an older protocol, the
    /// negotiated session is forced to run at that level even if the peer advertised something newer.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_protocol_was_capped(&self) -> bool {
        local_cap_reduced_protocol(self.remote_protocol(), self.negotiated_protocol())
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

    /// Maps the inner transport while keeping the negotiated metadata intact.
    ///
    /// This mirrors [`BinaryHandshake::map_stream_inner`] but operates on the decomposed
    /// parts, allowing callers to wrap the underlying transport before rebuilding the
    /// handshake. The replay buffer and negotiated protocols are preserved so higher
    /// layers can continue consuming the stream without rerunning the negotiation.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use protocol::ProtocolVersion;
    /// use transport::{negotiate_binary_session, BinaryHandshakeParts};
    /// use std::io::{self, Cursor, Read, Write};
    ///
    /// #[derive(Debug)]
    /// struct Instrumented<R> {
    ///     inner: R,
    ///     writes: Vec<u8>,
    /// }
    ///
    /// impl<R: Read + Write> Read for Instrumented<R> {
    ///     fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    ///         self.inner.read(buf)
    ///     }
    /// }
    ///
    /// impl<R: Read + Write> Write for Instrumented<R> {
    ///     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    ///         self.writes.extend_from_slice(buf);
    ///         self.inner.write(buf)
    ///     }
    ///
    ///     fn flush(&mut self) -> io::Result<()> {
    ///         self.inner.flush()
    ///     }
    /// }
    ///
    /// fn main() -> io::Result<()> {
    ///     let advertisement = u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes();
    ///     let transport = Cursor::new(advertisement.to_vec());
    ///     let parts = negotiate_binary_session(transport, ProtocolVersion::NEWEST)?
    ///         .into_parts();
    ///
    ///     let wrapped = parts.map_stream_inner(|inner| Instrumented {
    ///         inner,
    ///         writes: Vec::new(),
    ///     });
    ///     assert_eq!(wrapped.negotiated_protocol(), ProtocolVersion::NEWEST);
    ///     Ok(())
    /// }
    /// ```
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> BinaryHandshakeParts<T>
    where
        F: FnOnce(R) -> T,
    {
        let Self {
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
            remote_compatibility_flags,
            stream,
        } = self;

        BinaryHandshakeParts::from_components(
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
            remote_compatibility_flags,
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
    /// ```rust,no_run
    /// use protocol::ProtocolVersion;
    /// use transport::{negotiate_binary_session, BinaryHandshakeParts};
    /// use std::io::{self, Read, Write};
    ///
    /// #[derive(Debug)]
    /// struct Loopback {
    ///     reader: std::io::Cursor<Vec<u8>>,
    ///     written: Vec<u8>,
    /// }
    ///
    /// impl Loopback {
    ///     fn new(advertisement: [u8; 4]) -> Self {
    ///         Self {
    ///             reader: std::io::Cursor::new(advertisement.to_vec()),
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
    /// let parts = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
    ///     .expect("handshake succeeds")
    ///     .into_parts();
    ///
    /// let err = parts
    ///     .try_map_stream_inner(|inner| -> Result<Loopback, (io::Error, Loopback)> {
    ///         Err((io::Error::new(io::ErrorKind::Other, "wrap failed"), inner))
    ///     })
    ///     .expect_err("mapping fails");
    /// assert_eq!(err.error().kind(), io::ErrorKind::Other);
    ///
    /// let restored = err.into_original().into_handshake();
    /// assert_eq!(restored.remote_protocol(), remote);
    /// ```
    #[must_use = "handle the mapped handshake or propagate the error to preserve negotiation state"]
    pub fn try_map_stream_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<BinaryHandshakeParts<T>, TryMapInnerError<BinaryHandshakeParts<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        let Self {
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
            remote_compatibility_flags,
            stream,
        } = self;

        stream
            .try_map_inner(map)
            .map(|stream| {
                BinaryHandshakeParts::from_components(
                    remote_advertisement,
                    negotiated_protocol,
                    local_advertised,
                    remote_compatibility_flags,
                    stream,
                )
            })
            .map_err(|err| {
                err.map_original(|stream| {
                    BinaryHandshakeParts::from_components(
                        remote_advertisement,
                        negotiated_protocol,
                        local_advertised,
                        remote_compatibility_flags,
                        stream,
                    )
                })
            })
    }

    /// Decomposes the parts structure into the advertised versions and replaying stream.
    ///
    /// The tuple mirrors [`BinaryHandshake::into_components`] but operates on the already
    /// decomposed representation returned by [`BinaryHandshake::into_parts`]. Callers that need to
    /// inspect the raw protocol numbers before rebuilding the handshake can therefore recover the
    /// metadata without an intermediate reconstruction step.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use protocol::ProtocolVersion;
    /// use transport::negotiate_binary_session;
    /// use std::io::{self, Cursor};
    ///
    /// fn main() -> io::Result<()> {
    ///     let advertisement = u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes();
    ///     let transport = Cursor::new(advertisement.to_vec());
    ///     let parts = negotiate_binary_session(transport, ProtocolVersion::NEWEST)?
    ///         .into_parts();
    ///
    ///     let expected_remote = parts.remote_protocol();
    ///     let expected_local = parts.local_advertised_protocol();
    ///     let expected_negotiated = parts.negotiated_protocol();
    ///     let expected_flags = parts.remote_compatibility_flags();
    ///     let (
    ///         remote_advertised,
    ///         remote_protocol,
    ///         local_advertised,
    ///         negotiated_protocol,
    ///         remote_flags,
    ///         stream_parts,
    ///     ) =
    ///         parts.into_components();
    ///
    ///     assert_eq!(remote_advertised, u32::from(expected_remote.as_u8()));
    ///     assert_eq!(remote_protocol, expected_remote);
    ///     assert_eq!(local_advertised, expected_local);
    ///     assert_eq!(negotiated_protocol, expected_negotiated);
    ///     assert_eq!(remote_flags, expected_flags);
    ///     assert_eq!(
    ///         stream_parts.decision(),
    ///         protocol::NegotiationPrologue::Binary
    ///     );
    ///     assert!(stream_parts.buffered().is_empty());
    ///     Ok(())
    /// }
    /// ```
    #[must_use]
    pub fn into_components(
        self,
    ) -> (
        u32,
        ProtocolVersion,
        ProtocolVersion,
        ProtocolVersion,
        CompatibilityFlags,
        NegotiatedStreamParts<R>,
    ) {
        (
            self.remote_advertisement.advertised(),
            self.remote_advertisement.negotiated(),
            self.local_advertised,
            self.negotiated_protocol,
            self.remote_compatibility_flags,
            self.stream,
        )
    }

    pub(crate) fn from_components(
        remote_advertisement: RemoteProtocolAdvertisement,
        negotiated_protocol: ProtocolVersion,
        local_advertised: ProtocolVersion,
        remote_compatibility_flags: CompatibilityFlags,
        stream: NegotiatedStreamParts<R>,
    ) -> Self {
        Self::new(
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
            remote_compatibility_flags,
            stream,
        )
    }

    /// Rebuilds a [`BinaryHandshake`] from the preserved components.
    #[must_use]
    pub fn into_handshake(self) -> BinaryHandshake<R> {
        BinaryHandshake::from_parts(self)
    }
}
