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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff_negotiation_stream;
    use std::io::Cursor;

    // Helper to create binary handshake parts
    fn create_binary_parts() -> SessionHandshakeParts<Cursor<Vec<u8>>> {
        // Binary negotiation: protocol 31 as BE u32
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
        let greeting = LegacyDaemonGreetingOwned::from_parts(31, Some(0), None)
            .expect("valid greeting");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        SessionHandshakeParts::from_legacy_components(greeting, proto31, stream.into_parts())
    }

    // ==== from_binary_components tests ====

    #[test]
    fn from_binary_components_creates_binary_variant() {
        let parts = create_binary_parts();
        assert!(matches!(parts, SessionHandshakeParts::Binary(_)));
    }

    #[test]
    fn from_binary_components_preserves_remote_advertised() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let parts = SessionHandshakeParts::from_binary_components(
            42, // Different raw value
            proto31,
            proto31,
            proto31,
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        );
        if let SessionHandshakeParts::Binary(binary_parts) = parts {
            assert_eq!(binary_parts.remote_advertised_protocol(), 42);
        } else {
            panic!("expected Binary variant");
        }
    }

    #[test]
    fn from_binary_components_preserves_remote_protocol() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto30 = ProtocolVersion::from_supported(30).unwrap();
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let parts = SessionHandshakeParts::from_binary_components(
            31,
            proto30, // remote_protocol
            proto31, // local_advertised
            proto30, // negotiated
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        );
        if let SessionHandshakeParts::Binary(binary_parts) = parts {
            assert_eq!(binary_parts.remote_protocol().as_u8(), 30);
        } else {
            panic!("expected Binary variant");
        }
    }

    #[test]
    fn from_binary_components_preserves_local_advertised() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto30 = ProtocolVersion::from_supported(30).unwrap();
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let parts = SessionHandshakeParts::from_binary_components(
            31,
            proto31,
            proto30, // local_advertised
            proto30, // negotiated
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        );
        if let SessionHandshakeParts::Binary(binary_parts) = parts {
            assert_eq!(binary_parts.local_advertised_protocol().as_u8(), 30);
        } else {
            panic!("expected Binary variant");
        }
    }

    #[test]
    fn from_binary_components_preserves_negotiated_protocol() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto30 = ProtocolVersion::from_supported(30).unwrap();
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let parts = SessionHandshakeParts::from_binary_components(
            31,
            proto31,
            proto31,
            proto30, // negotiated
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        );
        if let SessionHandshakeParts::Binary(binary_parts) = parts {
            assert_eq!(binary_parts.negotiated_protocol().as_u8(), 30);
        } else {
            panic!("expected Binary variant");
        }
    }

    #[test]
    fn from_binary_components_preserves_compatibility_flags() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let flags = CompatibilityFlags::SYMLINK_TIMES | CompatibilityFlags::SYMLINK_ICONV;
        let parts = SessionHandshakeParts::from_binary_components(
            31,
            proto31,
            proto31,
            proto31,
            flags,
            stream.into_parts(),
        );
        if let SessionHandshakeParts::Binary(binary_parts) = parts {
            assert!(binary_parts.remote_compatibility_flags().contains(CompatibilityFlags::SYMLINK_TIMES));
        } else {
            panic!("expected Binary variant");
        }
    }

    // ==== from_legacy_components tests ====

    #[test]
    fn from_legacy_components_creates_legacy_variant() {
        let parts = create_legacy_parts();
        assert!(matches!(parts, SessionHandshakeParts::Legacy(_)));
    }

    #[test]
    fn from_legacy_components_preserves_greeting() {
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let greeting = LegacyDaemonGreetingOwned::from_parts(31, Some(0), None)
            .expect("valid greeting");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let parts = SessionHandshakeParts::from_legacy_components(
            greeting,
            proto31,
            stream.into_parts(),
        );
        if let SessionHandshakeParts::Legacy(legacy_parts) = parts {
            assert_eq!(legacy_parts.server_greeting().advertised_protocol(), 31);
        } else {
            panic!("expected Legacy variant");
        }
    }

    #[test]
    fn from_legacy_components_preserves_negotiated_protocol() {
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let greeting = LegacyDaemonGreetingOwned::from_parts(31, Some(0), None)
            .expect("valid greeting");
        let proto30 = ProtocolVersion::from_supported(30).unwrap();
        let parts = SessionHandshakeParts::from_legacy_components(
            greeting,
            proto30, // clamped negotiated
            stream.into_parts(),
        );
        if let SessionHandshakeParts::Legacy(legacy_parts) = parts {
            assert_eq!(legacy_parts.negotiated_protocol().as_u8(), 30);
        } else {
            panic!("expected Legacy variant");
        }
    }

    #[test]
    fn from_legacy_components_with_digest_list() {
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let digests = "md5 sha1".to_string();
        let greeting = LegacyDaemonGreetingOwned::from_parts(31, Some(0), Some(digests))
            .expect("valid greeting with digests");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let parts = SessionHandshakeParts::from_legacy_components(
            greeting,
            proto31,
            stream.into_parts(),
        );
        if let SessionHandshakeParts::Legacy(legacy_parts) = parts {
            let greeting = legacy_parts.server_greeting();
            assert!(greeting.digest_list().is_some());
        } else {
            panic!("expected Legacy variant");
        }
    }

    // ==== Clone and Debug tests ====

    #[test]
    fn session_handshake_parts_clone_binary() {
        let parts = create_binary_parts();
        let cloned = parts.clone();
        assert!(matches!(cloned, SessionHandshakeParts::Binary(_)));
    }

    #[test]
    fn session_handshake_parts_clone_legacy() {
        let parts = create_legacy_parts();
        let cloned = parts.clone();
        assert!(matches!(cloned, SessionHandshakeParts::Legacy(_)));
    }

    #[test]
    fn session_handshake_parts_debug_binary() {
        let parts = create_binary_parts();
        let debug = format!("{parts:?}");
        assert!(debug.contains("Binary"));
    }

    #[test]
    fn session_handshake_parts_debug_legacy() {
        let parts = create_legacy_parts();
        let debug = format!("{parts:?}");
        assert!(debug.contains("Legacy"));
    }

    // ==== Type alias tests ====

    #[test]
    fn binary_handshake_components_tuple_accessible() {
        let parts = create_binary_parts();
        if let SessionHandshakeParts::Binary(binary_parts) = parts {
            let components: BinaryHandshakeComponents<_> = binary_parts.into_components();
            let (raw, remote, local, negotiated, flags, stream) = components;
            assert_eq!(raw, 31);
            assert_eq!(remote.as_u8(), 31);
            assert_eq!(local.as_u8(), 31);
            assert_eq!(negotiated.as_u8(), 31);
            assert_eq!(flags, CompatibilityFlags::EMPTY);
            // Stream contains the buffered bytes captured during sniffing
            assert!(!stream.buffered().is_empty());
        } else {
            panic!("expected Binary variant");
        }
    }

    #[test]
    fn legacy_handshake_components_tuple_accessible() {
        let parts = create_legacy_parts();
        if let SessionHandshakeParts::Legacy(legacy_parts) = parts {
            let components: LegacyHandshakeComponents<_> = legacy_parts.into_components();
            let (greeting, negotiated, stream) = components;
            assert_eq!(greeting.advertised_protocol(), 31);
            assert_eq!(negotiated.as_u8(), 31);
            assert!(stream.buffered().starts_with(b"@RSYNCD:"));
        } else {
            panic!("expected Legacy variant");
        }
    }
}
