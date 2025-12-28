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
/// use rsync_io::{negotiate_binary_session, BinaryHandshakeParts};
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
    pub const fn stream_parts_mut(&mut self) -> &mut NegotiatedStreamParts<R> {
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
    /// use rsync_io::{negotiate_binary_session, BinaryHandshakeParts};
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
    /// use rsync_io::{negotiate_binary_session, BinaryHandshakeParts};
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
    /// use rsync_io::negotiate_binary_session;
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

    pub(crate) const fn from_components(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff_negotiation_stream;
    use protocol::NegotiationPrologue;
    use std::io::{self, Cursor};

    fn create_test_parts() -> BinaryHandshakeParts<Cursor<Vec<u8>>> {
        // Binary negotiation is triggered by first byte != '@'
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let remote_adv = RemoteProtocolAdvertisement::from_raw(31, proto31);
        BinaryHandshakeParts::from_components(
            remote_adv,
            proto31,
            proto31,
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        )
    }

    // ==== Protocol accessors ====

    #[test]
    fn remote_advertised_protocol_returns_raw() {
        let parts = create_test_parts();
        assert_eq!(parts.remote_advertised_protocol(), 31);
    }

    #[test]
    fn remote_protocol_returns_clamped_version() {
        let parts = create_test_parts();
        assert_eq!(parts.remote_protocol().as_u8(), 31);
    }

    #[test]
    fn negotiated_protocol_returns_version() {
        let parts = create_test_parts();
        assert_eq!(parts.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn local_advertised_protocol_returns_version() {
        let parts = create_test_parts();
        assert_eq!(parts.local_advertised_protocol().as_u8(), 31);
    }

    #[test]
    fn remote_compatibility_flags_empty() {
        let parts = create_test_parts();
        assert_eq!(
            parts.remote_compatibility_flags(),
            CompatibilityFlags::EMPTY
        );
    }

    // ==== Protocol clamping ====

    #[test]
    fn remote_protocol_was_clamped_false_when_supported() {
        let parts = create_test_parts();
        assert!(!parts.remote_protocol_was_clamped());
    }

    #[test]
    fn remote_advertisement_returns_classification() {
        let parts = create_test_parts();
        let adv = parts.remote_advertisement();
        assert!(!adv.was_clamped());
        assert_eq!(adv.negotiated().as_u8(), 31);
    }

    #[test]
    fn local_protocol_was_capped_true_when_reduced() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let proto29 = ProtocolVersion::from_supported(29).unwrap();
        let remote_adv = RemoteProtocolAdvertisement::from_raw(31, proto31);
        let parts = BinaryHandshakeParts::from_components(
            remote_adv,
            proto29,
            proto29,
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        );
        assert!(parts.local_protocol_was_capped());
    }

    #[test]
    fn local_protocol_was_capped_false_when_not_reduced() {
        let parts = create_test_parts();
        assert!(!parts.local_protocol_was_capped());
    }

    // ==== Stream parts accessors ====

    #[test]
    fn stream_parts_returns_reference() {
        let parts = create_test_parts();
        let stream = parts.stream_parts();
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    #[test]
    fn stream_parts_mut_returns_mutable_reference() {
        let mut parts = create_test_parts();
        let stream = parts.stream_parts_mut();
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    #[test]
    fn into_stream_parts_returns_owned() {
        let parts = create_test_parts();
        let stream = parts.into_stream_parts();
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    // ==== Decomposition ====

    #[test]
    fn into_components_returns_all_parts() {
        let parts = create_test_parts();
        let (remote_adv, remote_proto, local_adv, negotiated, flags, stream) =
            parts.into_components();
        assert_eq!(remote_adv, 31);
        assert_eq!(remote_proto.as_u8(), 31);
        assert_eq!(local_adv.as_u8(), 31);
        assert_eq!(negotiated.as_u8(), 31);
        assert_eq!(flags, CompatibilityFlags::EMPTY);
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    // ==== Reconstruction ====

    #[test]
    fn into_handshake_rebuilds_wrapper() {
        let parts = create_test_parts();
        let handshake = parts.into_handshake();
        assert_eq!(handshake.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn roundtrip_through_handshake_preserves_state() {
        let parts = create_test_parts();
        let expected_proto = parts.negotiated_protocol();
        let handshake = parts.into_handshake();
        let restored_parts = handshake.into_parts();
        assert_eq!(restored_parts.negotiated_protocol(), expected_proto);
    }

    // ==== Mapping ====

    #[test]
    fn map_stream_inner_transforms_transport() {
        let parts = create_test_parts();
        let mapped = parts.map_stream_inner(|cursor| {
            let pos = cursor.position();
            let mut new_cursor = Cursor::new(cursor.into_inner());
            new_cursor.set_position(pos);
            new_cursor
        });
        assert_eq!(mapped.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn try_map_stream_inner_succeeds() {
        let parts = create_test_parts();
        let result =
            parts.try_map_stream_inner(|cursor| -> Result<_, (io::Error, _)> { Ok(cursor) });
        assert!(result.is_ok());
        let mapped = result.unwrap();
        assert_eq!(mapped.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn try_map_stream_inner_fails_preserves_parts() {
        let parts = create_test_parts();
        let result = parts.try_map_stream_inner(|cursor| -> Result<Cursor<Vec<u8>>, _> {
            Err((io::Error::other("test error"), cursor))
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.error().kind(), io::ErrorKind::Other);
        let recovered = err.into_original();
        assert_eq!(recovered.negotiated_protocol().as_u8(), 31);
    }

    // ==== Clone and Debug ====

    #[test]
    fn clone_produces_independent_copy() {
        let parts = create_test_parts();
        let cloned = parts.clone();
        assert_eq!(parts.negotiated_protocol(), cloned.negotiated_protocol());
    }

    #[test]
    fn debug_format_contains_type_name() {
        let parts = create_test_parts();
        let debug = format!("{parts:?}");
        assert!(debug.contains("BinaryHandshakeParts"));
    }
}
