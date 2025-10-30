use super::*;
use rsync_protocol::{NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion};
use std::io::{self, Cursor, Read, Write};

use crate::RemoteProtocolAdvertisement;

#[derive(Clone, Debug)]
struct MemoryTransport {
    reader: Cursor<Vec<u8>>,
    written: Vec<u8>,
}

impl MemoryTransport {
    fn new(input: &[u8]) -> Self {
        Self {
            reader: Cursor::new(input.to_vec()),
            written: Vec::new(),
        }
    }

    fn written(&self) -> &[u8] {
        &self.written
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

#[derive(Debug)]
struct CountingTransport {
    inner: MemoryTransport,
    flushes: usize,
}

impl CountingTransport {
    fn new(input: &[u8]) -> Self {
        Self {
            inner: MemoryTransport::new(input),
            flushes: 0,
        }
    }

    fn written(&self) -> &[u8] {
        self.inner.written()
    }

    fn flushes(&self) -> usize {
        self.flushes
    }
}

impl Read for CountingTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for CountingTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        self.inner.flush()
    }
}

#[test]
fn binary_handshake_round_trips_from_components() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&handshake_bytes(remote_version));

    let handshake =
        negotiate_binary_session(transport, ProtocolVersion::NEWEST).expect("handshake succeeds");

    let expected_buffer = handshake.stream().buffered().to_vec();
    let remote_advertised = handshake.remote_advertised_protocol();
    let remote_protocol = handshake.remote_protocol();
    let local_advertised = handshake.local_advertised_protocol();
    let negotiated_protocol = handshake.negotiated_protocol();
    let stream = handshake.into_stream();

    let rebuilt = BinaryHandshake::from_components(
        remote_advertised,
        remote_protocol,
        local_advertised,
        negotiated_protocol,
        stream,
    );

    assert_eq!(rebuilt.remote_advertised_protocol(), remote_advertised);
    assert_eq!(rebuilt.remote_protocol(), remote_protocol);
    assert_eq!(rebuilt.local_advertised_protocol(), local_advertised);
    assert_eq!(rebuilt.negotiated_protocol(), negotiated_protocol);
    assert_eq!(rebuilt.stream().decision(), NegotiationPrologue::Binary);
    assert_eq!(rebuilt.stream().buffered(), expected_buffer.as_slice());
    assert_eq!(
        rebuilt.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_protocol)
    );
}

fn handshake_bytes(version: ProtocolVersion) -> [u8; 4] {
    u32::from(version.as_u8()).to_be_bytes()
}

#[test]
fn negotiate_binary_session_exchanges_versions() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport = MemoryTransport::new(&handshake_bytes(remote_version));

    let handshake =
        negotiate_binary_session(transport, ProtocolVersion::NEWEST).expect("handshake succeeds");

    let parts = handshake.clone().into_parts();
    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_version)
    );
    assert_eq!(handshake.remote_protocol(), remote_version);
    assert_eq!(handshake.negotiated_protocol(), remote_version);
    assert_eq!(
        handshake.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_version)
    );
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());

    let transport = handshake.into_stream().into_inner();
    assert_eq!(
        transport.written(),
        &handshake_bytes(ProtocolVersion::NEWEST)
    );
}

#[test]
fn map_stream_inner_preserves_protocols_and_replays_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let handshake =
        negotiate_binary_session(transport, ProtocolVersion::NEWEST).expect("handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let mut handshake = handshake.map_stream_inner(InstrumentedTransport::new);
    handshake
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");
    handshake.stream_mut().flush().expect("flush propagates");

    assert_eq!(handshake.remote_protocol(), remote_version);
    assert_eq!(handshake.negotiated_protocol(), remote_version);
    assert_eq!(
        handshake.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"payload");
    assert_eq!(instrumented.flushes(), 1);

    let inner = instrumented.into_inner();
    let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(inner.written(), expected.as_slice());
}

#[test]
fn try_map_stream_inner_transforms_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let handshake =
        negotiate_binary_session(transport, ProtocolVersion::NEWEST).expect("handshake succeeds");

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
        .write_all(b"payload")
        .expect("write propagates");
    handshake.stream_mut().flush().expect("flush propagates");
    assert_eq!(handshake.remote_protocol(), remote_version);

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"payload");
    assert_eq!(instrumented.flushes(), 1);
}

#[test]
fn parts_map_stream_inner_transforms_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let parts = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds")
        .into_parts();
    assert_eq!(parts.remote_protocol(), remote_version);

    let mapped = parts.map_stream_inner(InstrumentedTransport::new);
    assert_eq!(mapped.remote_protocol(), remote_version);

    let mut handshake = mapped.into_handshake();
    handshake
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");
    handshake.stream_mut().flush().expect("flush propagates");

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"payload");
    assert_eq!(instrumented.flushes(), 1);

    let inner = instrumented.into_inner();
    let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(inner.written(), expected.as_slice());
}

#[test]
fn parts_try_map_stream_inner_transforms_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let parts = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds")
        .into_parts();

    let mapped = parts
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Ok(InstrumentedTransport::new(inner))
            },
        )
        .expect("mapping succeeds");
    assert_eq!(mapped.remote_protocol(), remote_version);

    let mut handshake = mapped.into_handshake();
    handshake
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"payload");
}

#[test]
fn parts_try_map_stream_inner_preserves_original_on_error() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let parts = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds")
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
    assert_eq!(restored.remote_protocol(), remote_version);

    let remapped = restored.map_stream_inner(InstrumentedTransport::new);
    assert_eq!(remapped.remote_protocol(), remote_version);
}

#[test]
fn try_map_stream_inner_preserves_original_on_error() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let handshake =
        negotiate_binary_session(transport, ProtocolVersion::NEWEST).expect("handshake succeeds");

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
    assert_eq!(original.remote_protocol(), remote_version);
    let transport = original.into_stream().into_inner();
    assert_eq!(
        transport.written(),
        &handshake_bytes(ProtocolVersion::NEWEST)
    );
}

#[test]
fn negotiate_binary_session_clamps_future_protocols() {
    let future_version = 40u32;
    let transport = MemoryTransport::new(&future_version.to_be_bytes());

    let desired = ProtocolVersion::from_supported(29).expect("29 supported");
    let handshake = negotiate_binary_session(transport, desired).expect("future versions clamp");

    assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(handshake.negotiated_protocol(), desired);
    assert_eq!(handshake.remote_advertised_protocol(), future_version);
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(future_version, ProtocolVersion::NEWEST)
    );

    let parts = handshake.into_parts();
    assert_eq!(parts.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(parts.negotiated_protocol(), desired);
    assert_eq!(parts.remote_advertised_protocol(), future_version);
    assert!(parts.remote_protocol_was_clamped());
    assert!(parts.local_protocol_was_capped());
    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(future_version, ProtocolVersion::NEWEST)
    );

    let transport = parts.into_handshake().into_stream().into_inner();
    assert_eq!(transport.written(), &handshake_bytes(desired));
}

#[test]
fn negotiate_binary_session_clamps_protocols_beyond_u8_range() {
    let future_version = 0x0001_0200u32;
    let transport = MemoryTransport::new(&future_version.to_be_bytes());

    let handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("future advertisements beyond u8 clamp to newest");

    assert_eq!(handshake.remote_advertised_protocol(), future_version);
    assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(handshake.negotiated_protocol(), ProtocolVersion::NEWEST);
    assert!(handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(future_version, ProtocolVersion::NEWEST)
    );
}

#[test]
fn negotiate_binary_session_clamps_u32_max_advertisement() {
    let transport = MemoryTransport::new(&u32::MAX.to_be_bytes());

    let handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("maximum u32 advertisement clamps to newest");

    assert_eq!(handshake.remote_advertised_protocol(), u32::MAX);
    assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(handshake.negotiated_protocol(), ProtocolVersion::NEWEST);
    assert!(handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(u32::MAX, ProtocolVersion::NEWEST)
    );
}

#[test]
fn negotiate_binary_session_applies_cap() {
    let remote_version = ProtocolVersion::NEWEST;
    let desired = ProtocolVersion::from_supported(30).expect("30 supported");
    let transport = MemoryTransport::new(&handshake_bytes(remote_version));

    let handshake = negotiate_binary_session(transport, desired).expect("handshake succeeds");

    let parts = handshake.clone().into_parts();
    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_version)
    );
    assert_eq!(handshake.remote_protocol(), remote_version);
    assert_eq!(handshake.negotiated_protocol(), desired);
    assert_eq!(
        handshake.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_version)
    );
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(handshake.local_protocol_was_capped());

    let parts = handshake.into_parts();
    assert!(!parts.remote_protocol_was_clamped());
    assert!(parts.local_protocol_was_capped());
}

#[test]
fn negotiate_binary_session_rejects_legacy_prefix() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let err = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect_err("legacy prefix must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn negotiate_binary_session_rejects_out_of_range_version() {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&27u32.to_be_bytes());
    let transport = MemoryTransport::new(&bytes);
    let err = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect_err("unsupported protocol must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn into_stream_parts_exposes_negotiation_state() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport = MemoryTransport::new(&handshake_bytes(remote_version));

    let handshake =
        negotiate_binary_session(transport, ProtocolVersion::NEWEST).expect("handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let (remote_adv, remote, local_advertised, negotiated, parts) = handshake.into_stream_parts();
    assert_eq!(remote_adv, u32::from(remote_version.as_u8()));
    assert_eq!(remote, remote_version);
    assert_eq!(local_advertised, ProtocolVersion::NEWEST);
    assert_eq!(negotiated, remote_version);
    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(
        parts.sniffed_prefix(),
        &handshake_bytes(remote_version)[..1]
    );
    assert_eq!(parts.buffered_remaining(), 0);
    assert_eq!(parts.sniffed_prefix_len(), 1);

    let mut stream = parts.into_stream();
    let mut remainder = Vec::new();
    stream
        .read_to_end(&mut remainder)
        .expect("no additional bytes remain after handshake");
    assert!(remainder.is_empty());

    let transport = stream.into_inner();
    assert_eq!(
        transport.written(),
        &handshake_bytes(ProtocolVersion::NEWEST)
    );
}

#[test]
fn binary_handshake_parts_into_components_matches_accessors() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let parts = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds")
        .into_parts();

    let expected_advertised = parts.remote_advertised_protocol();
    let expected_remote = parts.remote_protocol();
    let expected_local = parts.local_advertised_protocol();
    let expected_negotiated = parts.negotiated_protocol();
    let expected_consumed = parts.stream_parts().buffered_consumed();
    let expected_buffer = parts.stream_parts().buffered().to_vec();

    let (advertised, remote, local, negotiated, stream_parts) = parts.into_components();

    assert_eq!(advertised, expected_advertised);
    assert_eq!(remote, expected_remote);
    assert_eq!(local, expected_local);
    assert_eq!(negotiated, expected_negotiated);
    assert_eq!(stream_parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(
        stream_parts.sniffed_prefix(),
        &handshake_bytes(expected_remote)[..1]
    );
    assert_eq!(stream_parts.buffered_consumed(), expected_consumed);
    assert_eq!(stream_parts.buffered(), expected_buffer.as_slice());
}

#[test]
fn from_stream_parts_rehydrates_binary_handshake() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport = CountingTransport::new(&handshake_bytes(remote_version));

    let handshake =
        negotiate_binary_session(transport, ProtocolVersion::NEWEST).expect("handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let (remote_adv, remote, local_advertised, negotiated, parts) = handshake.into_stream_parts();
    assert_eq!(remote_adv, u32::from(remote_version.as_u8()));
    assert_eq!(remote, remote_version);
    assert_eq!(local_advertised, ProtocolVersion::NEWEST);
    assert_eq!(negotiated, remote_version);
    assert_eq!(parts.decision(), NegotiationPrologue::Binary);

    let mut rehydrated =
        BinaryHandshake::from_stream_parts(remote_adv, remote, local_advertised, negotiated, parts);

    assert!(!rehydrated.local_protocol_was_capped());
    assert_eq!(rehydrated.remote_protocol(), remote_version);
    assert_eq!(rehydrated.negotiated_protocol(), remote_version);
    assert_eq!(rehydrated.stream().decision(), NegotiationPrologue::Binary);

    rehydrated
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");
    rehydrated.stream_mut().flush().expect("flush propagates");

    let transport = rehydrated.into_stream().into_inner();
    assert_eq!(transport.flushes(), 2);

    let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(transport.written(), expected.as_slice());
}

#[test]
fn into_parts_round_trips_binary_handshake() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport = CountingTransport::new(&handshake_bytes(remote_version));

    let handshake =
        negotiate_binary_session(transport, ProtocolVersion::NEWEST).expect("handshake succeeds");

    let parts = handshake.into_parts();
    assert_eq!(
        parts.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert_eq!(parts.remote_protocol(), remote_version);
    assert_eq!(parts.negotiated_protocol(), remote_version);
    assert_eq!(parts.local_advertised_protocol(), ProtocolVersion::NEWEST);
    assert!(!parts.remote_protocol_was_clamped());
    assert!(!parts.local_protocol_was_capped());
    assert_eq!(parts.stream_parts().decision(), NegotiationPrologue::Binary);

    let mut rebuilt = parts.into_handshake();
    assert_eq!(rebuilt.remote_protocol(), remote_version);
    assert_eq!(rebuilt.negotiated_protocol(), remote_version);

    rebuilt
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");
    rebuilt.stream_mut().flush().expect("flush propagates");

    let transport = rebuilt.into_stream().into_inner();
    assert_eq!(transport.flushes(), 2);

    let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(transport.written(), expected.as_slice());
}

#[test]
fn binary_handshake_rehydrates_sniffer_state() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let mut bytes = handshake_bytes(remote_version).to_vec();
    bytes.extend_from_slice(b"payload");
    let transport = MemoryTransport::new(&bytes);

    let handshake =
        negotiate_binary_session(transport, ProtocolVersion::NEWEST).expect("handshake succeeds");

    let mut sniffer = NegotiationPrologueSniffer::new();
    handshake
        .rehydrate_sniffer(&mut sniffer)
        .expect("rehydration succeeds");

    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
    assert_eq!(sniffer.buffered(), handshake.stream().buffered());
    assert_eq!(
        sniffer.sniffed_prefix_len(),
        handshake.stream().sniffed_prefix_len()
    );
}

#[test]
fn negotiate_binary_session_flushes_advertisement() {
    let transport = CountingTransport::new(&handshake_bytes(ProtocolVersion::NEWEST));

    let handshake =
        negotiate_binary_session(transport, ProtocolVersion::NEWEST).expect("handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let transport = handshake.into_stream().into_inner();
    assert_eq!(transport.flushes(), 1);
    assert_eq!(
        transport.written(),
        &handshake_bytes(ProtocolVersion::NEWEST)
    );
}

#[test]
fn negotiate_binary_session_with_sniffer_reuses_instance() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport1 = MemoryTransport::new(&handshake_bytes(remote_version));
    let transport2 = MemoryTransport::new(&handshake_bytes(remote_version));

    let mut sniffer = NegotiationPrologueSniffer::new();

    let handshake1 =
        negotiate_binary_session_with_sniffer(transport1, ProtocolVersion::NEWEST, &mut sniffer)
            .expect("handshake succeeds with supplied sniffer");
    assert_eq!(handshake1.remote_protocol(), remote_version);
    assert_eq!(handshake1.negotiated_protocol(), remote_version);
    assert!(!handshake1.local_protocol_was_capped());

    drop(handshake1);

    let handshake2 =
        negotiate_binary_session_with_sniffer(transport2, ProtocolVersion::NEWEST, &mut sniffer)
            .expect("sniffer can be reused for subsequent sessions");
    assert_eq!(handshake2.remote_protocol(), remote_version);
    assert_eq!(handshake2.negotiated_protocol(), remote_version);
    assert!(!handshake2.local_protocol_was_capped());
}
