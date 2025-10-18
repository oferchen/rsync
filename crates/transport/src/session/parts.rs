use crate::binary::{BinaryHandshake, BinaryHandshakeParts};
use crate::daemon::{LegacyDaemonHandshake, LegacyDaemonHandshakeParts};
use crate::handshake_util::remote_advertisement_was_clamped;
use crate::negotiation::{NegotiatedStream, NegotiatedStreamParts, TryMapInnerError};
use rsync_protocol::{LegacyDaemonGreetingOwned, NegotiationPrologue, ProtocolVersion};
use std::convert::TryFrom;

use super::handshake::SessionHandshake;

/// Components extracted from a [`SessionHandshake`].
///
/// The structure mirrors the variant-specific handshake wrappers so callers can
/// temporarily take ownership of the buffered negotiation bytes while keeping
/// the negotiated protocol metadata. The parts can be converted back into a
/// [`SessionHandshake`] via [`SessionHandshake::from_stream_parts`] or the
/// [`From`] implementation. Variant-specific conversions are also exposed via
/// [`From`] and [`TryFrom`] so binary or legacy wrappers can be promoted or
/// recovered without manual pattern matching.
#[derive(Clone, Debug)]
pub enum SessionHandshakeParts<R> {
    /// Binary handshake metadata and replaying stream parts.
    Binary {
        /// Protocol number advertised by the remote peer before clamping.
        remote_advertised_protocol: u32,
        /// Protocol advertised by the remote peer.
        remote_protocol: ProtocolVersion,
        /// Protocol negotiated after applying the caller cap.
        negotiated_protocol: ProtocolVersion,
        /// Replaying stream parts containing the sniffed negotiation bytes.
        stream: NegotiatedStreamParts<R>,
    },
    /// Legacy daemon handshake metadata and replaying stream parts.
    Legacy {
        /// Parsed legacy daemon greeting announced by the server.
        server_greeting: LegacyDaemonGreetingOwned,
        /// Protocol negotiated after applying the caller cap.
        negotiated_protocol: ProtocolVersion,
        /// Replaying stream parts containing the sniffed negotiation bytes.
        stream: NegotiatedStreamParts<R>,
    },
}

impl<R> SessionHandshakeParts<R> {
    /// Returns the negotiation style associated with the extracted handshake.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        match self {
            SessionHandshakeParts::Binary { .. } => NegotiationPrologue::Binary,
            SessionHandshakeParts::Legacy { .. } => NegotiationPrologue::LegacyAscii,
        }
    }

    /// Returns the negotiated protocol version retained by the parts structure.
    #[must_use]
    pub fn negotiated_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary {
                negotiated_protocol,
                ..
            }
            | SessionHandshakeParts::Legacy {
                negotiated_protocol,
                ..
            } => *negotiated_protocol,
        }
    }

    /// Returns the protocol advertised by the remote peer.
    #[must_use]
    pub fn remote_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary {
                remote_protocol, ..
            } => *remote_protocol,
            SessionHandshakeParts::Legacy {
                server_greeting, ..
            } => server_greeting.protocol(),
        }
    }

    /// Returns the raw protocol number advertised by the remote peer.
    #[must_use]
    pub fn remote_advertised_protocol(&self) -> u32 {
        match self {
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                ..
            } => *remote_advertised_protocol,
            SessionHandshakeParts::Legacy {
                server_greeting, ..
            } => server_greeting.advertised_protocol(),
        }
    }

    /// Returns the legacy daemon greeting advertised by the server when available.
    #[must_use]
    pub fn server_greeting(&self) -> Option<&LegacyDaemonGreetingOwned> {
        match self {
            SessionHandshakeParts::Binary { .. } => None,
            SessionHandshakeParts::Legacy {
                server_greeting, ..
            } => Some(server_greeting),
        }
    }

    /// Returns a shared reference to the replaying stream parts.
    #[must_use]
    pub fn stream(&self) -> &NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary { stream, .. }
            | SessionHandshakeParts::Legacy { stream, .. } => stream,
        }
    }

    /// Returns a mutable reference to the replaying stream parts.
    #[must_use]
    pub fn stream_mut(&mut self) -> &mut NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary { stream, .. }
            | SessionHandshakeParts::Legacy { stream, .. } => stream,
        }
    }

    /// Releases the parts structure and reconstructs the replaying stream.
    #[must_use]
    pub fn into_stream(self) -> NegotiatedStream<R> {
        match self {
            SessionHandshakeParts::Binary { stream, .. }
            | SessionHandshakeParts::Legacy { stream, .. } => stream.into_stream(),
        }
    }

    /// Maps the inner transport for both variants while preserving the negotiated metadata.
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> SessionHandshakeParts<T>
    where
        F: FnOnce(R) -> T,
    {
        match self {
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            } => SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream: stream.map_inner(map),
            },
            SessionHandshakeParts::Legacy {
                server_greeting,
                negotiated_protocol,
                stream,
            } => SessionHandshakeParts::Legacy {
                server_greeting,
                negotiated_protocol,
                stream: stream.map_inner(map),
            },
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
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            } => stream
                .try_map_inner(map)
                .map(|stream| SessionHandshakeParts::Binary {
                    remote_advertised_protocol,
                    remote_protocol,
                    negotiated_protocol,
                    stream,
                })
                .map_err(|err| {
                    err.map_original(|stream| SessionHandshakeParts::Binary {
                        remote_advertised_protocol,
                        remote_protocol,
                        negotiated_protocol,
                        stream,
                    })
                }),
            SessionHandshakeParts::Legacy {
                server_greeting,
                negotiated_protocol,
                stream,
            } => match stream.try_map_inner(map) {
                Ok(stream) => Ok(SessionHandshakeParts::Legacy {
                    server_greeting,
                    negotiated_protocol,
                    stream,
                }),
                Err(err) => Err(err.map_original(|stream| SessionHandshakeParts::Legacy {
                    server_greeting,
                    negotiated_protocol,
                    stream,
                })),
            },
        }
    }

    /// Consumes the parts structure, returning the binary handshake components when available.
    pub fn into_binary(
        self,
    ) -> Result<
        (
            u32,
            ProtocolVersion,
            ProtocolVersion,
            NegotiatedStreamParts<R>,
        ),
        SessionHandshakeParts<R>,
    > {
        match self {
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            } => Ok((
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            )),
            SessionHandshakeParts::Legacy { .. } => Err(self),
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
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            } => Ok(BinaryHandshake::from_stream_parts(
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            )
            .into_parts()),
            SessionHandshakeParts::Legacy { .. } => Err(self),
        }
    }

    /// Consumes the parts structure, returning the legacy handshake components when available.
    pub fn into_legacy(
        self,
    ) -> Result<
        (
            LegacyDaemonGreetingOwned,
            ProtocolVersion,
            NegotiatedStreamParts<R>,
        ),
        SessionHandshakeParts<R>,
    > {
        match self {
            SessionHandshakeParts::Binary { .. } => Err(self),
            SessionHandshakeParts::Legacy {
                server_greeting,
                negotiated_protocol,
                stream,
            } => Ok((server_greeting, negotiated_protocol, stream)),
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
            SessionHandshakeParts::Binary { .. } => Err(self),
            SessionHandshakeParts::Legacy {
                server_greeting,
                negotiated_protocol,
                stream,
            } => Ok(LegacyDaemonHandshake::from_stream_parts(
                server_greeting,
                negotiated_protocol,
                stream,
            )
            .into_parts()),
        }
    }

    /// Reports whether the remote advertisement had to be clamped to the supported range.
    #[must_use]
    pub fn remote_protocol_was_clamped(&self) -> bool {
        remote_advertisement_was_clamped(self.remote_advertised_protocol(), self.remote_protocol())
    }

    /// Reports whether the negotiated protocol was reduced due to the caller's desired cap.
    #[must_use]
    pub fn local_protocol_was_capped(&self) -> bool {
        self.negotiated_protocol() < self.remote_protocol()
    }

    /// Reassembles a [`SessionHandshake`] from the stored components.
    #[must_use]
    pub fn into_handshake(self) -> SessionHandshake<R> {
        SessionHandshake::from_stream_parts(self)
    }
}

impl<R> From<BinaryHandshake<R>> for SessionHandshakeParts<R> {
    fn from(handshake: BinaryHandshake<R>) -> Self {
        let (remote_advertised_protocol, remote_protocol, negotiated_protocol, stream) =
            handshake.into_stream_parts();
        SessionHandshakeParts::Binary {
            remote_advertised_protocol,
            remote_protocol,
            negotiated_protocol,
            stream,
        }
    }
}

impl<R> From<LegacyDaemonHandshake<R>> for SessionHandshakeParts<R> {
    fn from(handshake: LegacyDaemonHandshake<R>) -> Self {
        let (server_greeting, negotiated_protocol, stream) = handshake.into_stream_parts();
        SessionHandshakeParts::Legacy {
            server_greeting,
            negotiated_protocol,
            stream,
        }
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for BinaryHandshake<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        parts
            .into_binary()
            .map(|(remote_advertised, remote, negotiated, stream)| {
                BinaryHandshake::from_stream_parts(remote_advertised, remote, negotiated, stream)
            })
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for LegacyDaemonHandshake<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        parts
            .into_legacy()
            .map(|(server_greeting, negotiated, stream)| {
                LegacyDaemonHandshake::from_stream_parts(server_greeting, negotiated, stream)
            })
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for BinaryHandshakeParts<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        parts.into_binary_parts()
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for LegacyDaemonHandshakeParts<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        parts.into_legacy_parts()
    }
}
