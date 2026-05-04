//! Shared test helpers for constructing handshake fixtures.

use crate::binary::BinaryHandshake;
use crate::daemon::LegacyDaemonHandshake;
use crate::sniff_negotiation_stream;
use protocol::{CompatibilityFlags, ProtocolVersion, parse_legacy_daemon_greeting_owned};
use std::io::Cursor;

/// Creates a binary handshake fixture at protocol 31. The first byte differs
/// from `'@'` so the sniffer chooses the binary prologue, and the remaining
/// bytes encode protocol 31 as a little-endian u32.
pub(super) fn create_binary_handshake() -> BinaryHandshake<Cursor<Vec<u8>>> {
    let stream = sniff_negotiation_stream(Cursor::new(vec![0x1f, 0x00, 0x00, 0x00]))
        .expect("sniff succeeds");
    let proto31 = ProtocolVersion::from_supported(31).unwrap();
    BinaryHandshake::from_components(
        31,
        proto31,
        proto31,
        proto31,
        CompatibilityFlags::EMPTY,
        stream,
    )
}

/// Creates a legacy daemon handshake fixture at protocol 31.
pub(super) fn create_legacy_handshake() -> LegacyDaemonHandshake<Cursor<Vec<u8>>> {
    let stream =
        sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec())).expect("sniff succeeds");
    let greeting = parse_legacy_daemon_greeting_owned("@RSYNCD: 31.0").expect("valid greeting");
    let proto31 = ProtocolVersion::from_supported(31).unwrap();
    LegacyDaemonHandshake::from_components(greeting, proto31, stream)
}
