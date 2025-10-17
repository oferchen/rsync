use crate::binary::{BinaryHandshake, negotiate_binary_session_from_stream};
use crate::daemon::{LegacyDaemonHandshake, negotiate_legacy_daemon_session_from_stream};
use crate::negotiation::{
    NegotiatedStream, NegotiatedStreamParts, TryMapInnerError, sniff_negotiation_stream,
    sniff_negotiation_stream_with_sniffer,
};
use rsync_protocol::{
    LegacyDaemonGreetingOwned, NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion,
};
use std::convert::TryFrom;
use std::io::{self, Read, Write};

/// Result of negotiating an rsync session over an arbitrary transport.
///
/// The enum wraps either the binary remote-shell handshake or the legacy ASCII
/// daemon negotiation while exposing convenience accessors that mirror the
/// per-variant helpers. Higher layers can match on the [`decision`] to branch on
/// the negotiated style without re-sniffing the transport.
#[derive(Debug)]
pub enum SessionHandshake<R> {
    /// Binary remote-shell style negotiation (protocols ≥ 30).
    Binary(BinaryHandshake<R>),
    /// Legacy `@RSYNCD:` daemon negotiation.
    Legacy(LegacyDaemonHandshake<R>),
}

impl<R> SessionHandshake<R> {
    /// Returns the detected negotiation style.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        match self {
            Self::Binary(_) => NegotiationPrologue::Binary,
            Self::Legacy(_) => NegotiationPrologue::LegacyAscii,
        }
    }

    /// Returns the negotiated protocol version after applying the caller cap.
    #[must_use]
    pub fn negotiated_protocol(&self) -> ProtocolVersion {
        match self {
            Self::Binary(handshake) => handshake.negotiated_protocol(),
            Self::Legacy(handshake) => handshake.negotiated_protocol(),
        }
    }

    /// Returns the protocol version advertised by the peer before client caps are applied.
    #[must_use]
    pub fn remote_protocol(&self) -> ProtocolVersion {
        match self {
            Self::Binary(handshake) => handshake.remote_protocol(),
            Self::Legacy(handshake) => handshake.server_protocol(),
        }
    }

    /// Returns the raw protocol number advertised by the remote peer before clamping.
    #[must_use]
    pub fn remote_advertised_protocol(&self) -> u32 {
        match self {
            Self::Binary(handshake) => handshake.remote_advertised_protocol(),
            Self::Legacy(handshake) => handshake.remote_advertised_protocol(),
        }
    }

    /// Reports whether the remote advertisement had to be clamped to the supported range.
    #[must_use]
    pub fn remote_protocol_was_clamped(&self) -> bool {
        match self {
            Self::Binary(handshake) => handshake.remote_protocol_was_clamped(),
            Self::Legacy(handshake) => handshake.remote_protocol_was_clamped(),
        }
    }

    /// Reports whether the negotiated protocol was reduced due to the caller's desired cap.
    ///
    /// This mirrors the per-variant helpers and keeps the aggregated handshake API aligned with
    /// upstream rsync, where diagnostics note when the user-requested protocol forced a downgrade.
    #[must_use]
    pub fn local_protocol_was_capped(&self) -> bool {
        match self {
            Self::Binary(handshake) => handshake.local_protocol_was_capped(),
            Self::Legacy(handshake) => handshake.local_protocol_was_capped(),
        }
    }

    /// Returns the parsed legacy daemon greeting when the negotiation used the legacy ASCII handshake.
    ///
    /// Binary negotiations do not exchange a greeting, so the method returns [`None`] in that case.
    #[must_use]
    pub fn server_greeting(&self) -> Option<&LegacyDaemonGreetingOwned> {
        match self {
            Self::Binary(_) => None,
            Self::Legacy(handshake) => Some(handshake.server_greeting()),
        }
    }

    /// Returns a shared reference to the replaying stream regardless of variant.
    #[must_use]
    pub fn stream(&self) -> &NegotiatedStream<R> {
        match self {
            Self::Binary(handshake) => handshake.stream(),
            Self::Legacy(handshake) => handshake.stream(),
        }
    }

    /// Returns a mutable reference to the replaying stream regardless of variant.
    #[must_use]
    pub fn stream_mut(&mut self) -> &mut NegotiatedStream<R> {
        match self {
            Self::Binary(handshake) => handshake.stream_mut(),
            Self::Legacy(handshake) => handshake.stream_mut(),
        }
    }

    /// Releases the wrapper and returns the replaying stream.
    #[must_use]
    pub fn into_stream(self) -> NegotiatedStream<R> {
        match self {
            Self::Binary(handshake) => handshake.into_stream(),
            Self::Legacy(handshake) => handshake.into_stream(),
        }
    }

    /// Maps the inner transport while preserving the negotiated metadata.
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> SessionHandshake<T>
    where
        F: FnOnce(R) -> T,
    {
        match self {
            Self::Binary(handshake) => SessionHandshake::Binary(handshake.map_stream_inner(map)),
            Self::Legacy(handshake) => SessionHandshake::Legacy(handshake.map_stream_inner(map)),
        }
    }

    /// Attempts to transform the inner transport for both handshake variants.
    pub fn try_map_stream_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<SessionHandshake<T>, TryMapInnerError<SessionHandshake<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        match self {
            Self::Binary(handshake) => handshake
                .try_map_stream_inner(map)
                .map(SessionHandshake::Binary)
                .map_err(|err| err.map_original(SessionHandshake::Binary)),
            Self::Legacy(handshake) => handshake
                .try_map_stream_inner(map)
                .map(SessionHandshake::Legacy)
                .map_err(|err| err.map_original(SessionHandshake::Legacy)),
        }
    }

    /// Returns the underlying binary handshake if the negotiation used that style.
    #[must_use]
    pub fn as_binary(&self) -> Option<&BinaryHandshake<R>> {
        match self {
            Self::Binary(handshake) => Some(handshake),
            Self::Legacy(_) => None,
        }
    }

    /// Returns a mutable reference to the binary handshake when the negotiation used that style.
    #[must_use]
    pub fn as_binary_mut(&mut self) -> Option<&mut BinaryHandshake<R>> {
        match self {
            Self::Binary(handshake) => Some(handshake),
            Self::Legacy(_) => None,
        }
    }

    /// Returns the underlying legacy daemon handshake if the negotiation used that style.
    #[must_use]
    pub fn as_legacy(&self) -> Option<&LegacyDaemonHandshake<R>> {
        match self {
            Self::Binary(_) => None,
            Self::Legacy(handshake) => Some(handshake),
        }
    }

    /// Returns a mutable reference to the legacy daemon handshake when the negotiation used that style.
    #[must_use]
    pub fn as_legacy_mut(&mut self) -> Option<&mut LegacyDaemonHandshake<R>> {
        match self {
            Self::Binary(_) => None,
            Self::Legacy(handshake) => Some(handshake),
        }
    }

    /// Consumes the wrapper, returning the binary handshake when applicable.
    pub fn into_binary(self) -> Result<BinaryHandshake<R>, SessionHandshake<R>> {
        match self {
            Self::Binary(handshake) => Ok(handshake),
            Self::Legacy(_) => Err(self),
        }
    }

    /// Consumes the wrapper, returning the legacy daemon handshake when applicable.
    pub fn into_legacy(self) -> Result<LegacyDaemonHandshake<R>, SessionHandshake<R>> {
        match self {
            Self::Binary(_) => Err(self),
            Self::Legacy(handshake) => Ok(handshake),
        }
    }

    /// Decomposes the handshake into variant-specific metadata and replaying stream parts.
    ///
    /// The returned [`SessionHandshakeParts`] mirrors the helpers exposed by the variant-specific
    /// handshakes while allowing higher layers to stage the buffered negotiation bytes and
    /// negotiated metadata without matching on [`SessionHandshake`] immediately. This is useful
    /// when temporary ownership of the underlying transport is required (for example to wrap it
    /// with instrumentation) before resuming the rsync protocol exchange.
    #[must_use]
    pub fn into_stream_parts(self) -> SessionHandshakeParts<R> {
        match self {
            SessionHandshake::Binary(handshake) => {
                let (remote_advertised_protocol, remote_protocol, negotiated_protocol, parts) =
                    handshake.into_stream_parts();
                SessionHandshakeParts::Binary {
                    remote_advertised_protocol,
                    remote_protocol,
                    negotiated_protocol,
                    stream: parts,
                }
            }
            SessionHandshake::Legacy(handshake) => {
                let (server_greeting, negotiated_protocol, parts) = handshake.into_stream_parts();
                SessionHandshakeParts::Legacy {
                    server_greeting,
                    negotiated_protocol,
                    stream: parts,
                }
            }
        }
    }

    /// Reassembles a [`SessionHandshake`] from the variant-specific stream parts previously
    /// extracted via [`Self::into_stream_parts`].
    #[must_use]
    pub fn from_stream_parts(parts: SessionHandshakeParts<R>) -> Self {
        match parts {
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            } => SessionHandshake::Binary(BinaryHandshake::from_stream_parts(
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            )),
            SessionHandshakeParts::Legacy {
                server_greeting,
                negotiated_protocol,
                stream,
            } => SessionHandshake::Legacy(LegacyDaemonHandshake::from_stream_parts(
                server_greeting,
                negotiated_protocol,
                stream,
            )),
        }
    }
}

/// Negotiates an rsync session, automatically detecting the handshake style.
///
/// The helper mirrors upstream rsync's behaviour when dialing a transport whose
/// negotiation style is unknown. It sniffs the prologue, dispatches to either
/// the binary or legacy negotiation helper, and returns a [`SessionHandshake`]
/// carrying the negotiated metadata together with the replaying stream.
///
/// # Errors
///
/// Propagates any I/O error reported by the underlying sniffing or variant
/// specific negotiation helper.
pub fn negotiate_session<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream(reader)?;
    negotiate_session_from_stream(stream, desired_protocol)
}

/// Negotiates an rsync session and returns the decomposed handshake parts.
///
/// This is a convenience wrapper around [`negotiate_session`] that immediately
/// converts the negotiated handshake into [`SessionHandshakeParts`]. Callers
/// that intend to wrap the underlying transport typically need this split
/// representation to stash the replayed negotiation bytes while instrumenting
/// the stream before resuming the protocol exchange.
///
/// # Errors
///
/// Propagates any I/O error reported by [`negotiate_session`].
pub fn negotiate_session_parts<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshakeParts<R>>
where
    R: Read + Write,
{
    negotiate_session(reader, desired_protocol).map(SessionHandshake::into_stream_parts)
}

/// Components extracted from a [`SessionHandshake`].
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

    /// Reports whether the remote advertisement had to be clamped to the supported range.
    #[must_use]
    pub fn remote_protocol_was_clamped(&self) -> bool {
        advertisement_was_clamped(self.remote_advertised_protocol(), self.remote_protocol())
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

/// Negotiates an rsync session while reusing a caller supplied sniffer.
///
/// This mirrors [`negotiate_session`] but allows higher layers to reuse a
/// [`NegotiationPrologueSniffer`] across multiple negotiations, matching the
/// existing binary and legacy helper variants.
pub fn negotiate_session_with_sniffer<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<SessionHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream_with_sniffer(reader, sniffer)?;
    negotiate_session_from_stream(stream, desired_protocol)
}

/// Negotiates an rsync session using a caller supplied sniffer and returns the
/// decomposed handshake parts.
///
/// This mirrors [`negotiate_session_parts`] but reuses the provided
/// [`NegotiationPrologueSniffer`], matching the semantics of
/// [`negotiate_session_with_sniffer`].
///
/// # Errors
///
/// Propagates any I/O error reported by
/// [`negotiate_session_with_sniffer`].
pub fn negotiate_session_parts_with_sniffer<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<SessionHandshakeParts<R>>
where
    R: Read + Write,
{
    negotiate_session_with_sniffer(reader, desired_protocol, sniffer)
        .map(SessionHandshake::into_stream_parts)
}

fn negotiate_session_from_stream<R>(
    stream: NegotiatedStream<R>,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshake<R>>
where
    R: Read + Write,
{
    match stream.decision() {
        NegotiationPrologue::Binary => {
            negotiate_binary_session_from_stream(stream, desired_protocol)
                .map(SessionHandshake::Binary)
        }
        NegotiationPrologue::LegacyAscii => {
            negotiate_legacy_daemon_session_from_stream(stream, desired_protocol)
                .map(SessionHandshake::Legacy)
        }
        NegotiationPrologue::NeedMoreData => {
            unreachable!("negotiation sniffer fully classifies the prologue")
        }
    }
}

fn advertisement_was_clamped(advertised: u32, protocol: ProtocolVersion) -> bool {
    let advertised_byte = u8::try_from(advertised).unwrap_or(u8::MAX);
    advertised_byte > protocol.as_u8()
}

#[cfg(test)]
mod tests;
