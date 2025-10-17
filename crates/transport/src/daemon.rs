use crate::negotiation::{NegotiatedStream, sniff_negotiation_stream};
use rsync_protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonGreetingOwned, NegotiationPrologue, ProtocolVersion,
    format_legacy_daemon_greeting,
};
use std::cmp;
use std::io::{self, Read, Write};

/// Result of performing the legacy ASCII daemon negotiation.
///
/// The structure exposes the negotiated protocol version together with the
/// parsed greeting metadata while retaining the replaying stream so higher
/// layers can continue consuming control messages or file lists.
pub struct LegacyDaemonHandshake<R> {
    stream: NegotiatedStream<R>,
    server_greeting: LegacyDaemonGreetingOwned,
    negotiated_protocol: ProtocolVersion,
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
pub fn negotiate_legacy_daemon_session<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
) -> io::Result<LegacyDaemonHandshake<R>>
where
    R: Read + Write,
{
    let mut stream = sniff_negotiation_stream(reader)?;

    match stream.decision() {
        NegotiationPrologue::LegacyAscii => {}
        NegotiationPrologue::Binary => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "legacy daemon negotiation requires @RSYNCD: prefix",
            ));
        }
        NegotiationPrologue::NeedMoreData => {
            unreachable!("sniffer must fully classify the negotiation prologue")
        }
    }

    let mut line = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN + 32);
    let greeting = stream.read_and_parse_legacy_daemon_greeting_details(&mut line)?;
    let server_greeting = LegacyDaemonGreetingOwned::from(greeting);

    let negotiated_protocol = cmp::min(desired_protocol, server_greeting.protocol());

    let banner = format_legacy_daemon_greeting(negotiated_protocol);
    stream.write_all(banner.as_bytes())?;
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

    use rsync_protocol::ProtocolVersion;
    use std::io::{self, Cursor};

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

    #[test]
    fn negotiate_legacy_daemon_session_exchanges_banners() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake should succeed");

        assert_eq!(
            handshake.negotiated_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );
        assert_eq!(handshake.server_greeting().advertised_protocol(), 31);

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

        let transport = handshake.into_stream().into_inner();
        assert_eq!(transport.written(), b"@RSYNCD: 30.0\n");
    }

    #[test]
    fn negotiate_rejects_binary_prefix() {
        let transport = MemoryTransport::new(&[0x00, 0x20, 0x00, 0x00]);
        match negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST) {
            Ok(_) => panic!("binary negotiation is rejected"),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::InvalidData),
        }
    }
}
