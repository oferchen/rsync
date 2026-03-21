//! Protocol version query accessors for [`SessionHandshake`].

use crate::handshake_util::RemoteProtocolAdvertisement;
use protocol::{LegacyDaemonGreetingOwned, NegotiationPrologue, ProtocolVersion};

use super::SessionHandshake;

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
    pub const fn negotiated_protocol(&self) -> ProtocolVersion {
        match self {
            Self::Binary(handshake) => handshake.negotiated_protocol(),
            Self::Legacy(handshake) => handshake.negotiated_protocol(),
        }
    }

    /// Returns the protocol version advertised by the peer before client caps are applied.
    #[must_use]
    pub const fn remote_protocol(&self) -> ProtocolVersion {
        match self {
            Self::Binary(handshake) => handshake.remote_protocol(),
            Self::Legacy(handshake) => handshake.server_protocol(),
        }
    }

    /// Returns the raw protocol number advertised by the remote peer before clamping.
    #[must_use]
    pub const fn remote_advertised_protocol(&self) -> u32 {
        match self {
            Self::Binary(handshake) => handshake.remote_advertised_protocol(),
            Self::Legacy(handshake) => handshake.remote_advertised_protocol(),
        }
    }

    /// Returns the protocol version advertised by the local peer before the negotiation settled.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_advertised_protocol(&self) -> ProtocolVersion {
        match self {
            Self::Binary(handshake) => handshake.local_advertised_protocol(),
            Self::Legacy(handshake) => handshake.local_advertised_protocol(),
        }
    }

    /// Returns the classification of the peer's protocol advertisement.
    #[must_use]
    pub const fn remote_advertisement(&self) -> RemoteProtocolAdvertisement {
        match self {
            Self::Binary(handshake) => handshake.remote_advertisement(),
            Self::Legacy(handshake) => handshake.remote_advertisement(),
        }
    }

    /// Reports whether the remote advertisement had to be clamped to the supported range.
    #[must_use]
    pub const fn remote_protocol_was_clamped(&self) -> bool {
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
    ///         let bytes = u32::from(advertised.as_u8()).to_le_bytes();
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
    pub const fn local_protocol_was_capped(&self) -> bool {
        match self {
            Self::Binary(handshake) => handshake.local_protocol_was_capped(),
            Self::Legacy(handshake) => handshake.local_protocol_was_capped(),
        }
    }

    /// Returns the parsed legacy daemon greeting when the negotiation used the legacy ASCII handshake.
    ///
    /// Binary negotiations do not exchange a greeting, so the method returns [`None`] in that case.
    pub const fn server_greeting(&self) -> Option<&LegacyDaemonGreetingOwned> {
        match self {
            Self::Binary(_) => None,
            Self::Legacy(handshake) => Some(handshake.server_greeting()),
        }
    }
}
