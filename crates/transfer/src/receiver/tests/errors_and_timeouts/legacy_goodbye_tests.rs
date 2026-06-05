//! Tests for legacy goodbye handshake (protocol 28/29).
//!
//! Protocol 28/29 uses a simpler goodbye sequence than protocol 30+:
//! just NDX_DONE as 4-byte little-endian i32, without NDX_FLIST_EOF
//! or NDX_DEL_STATS messages.
//!
//! upstream: main.c:875-906 `read_final_goodbye()`

use std::ffi::OsString;
use std::io::Cursor;

use protocol::ProtocolVersion;
use protocol::codec::{NdxCodec, create_ndx_codec};

use super::super::super::ReceiverContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

/// NDX_DONE as 4-byte little-endian (-1 = 0xFFFFFFFF).
const NDX_DONE_LE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

/// Creates a `HandshakeResult` for a specific protocol version.
fn handshake_for(protocol_version: u8) -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

/// Creates a `ReceiverContext` for a given protocol version.
fn receiver_for(protocol_version: u8) -> ReceiverContext {
    let handshake = handshake_for(protocol_version);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    ReceiverContext::new_for_test(&handshake, config)
}

#[test]
fn proto28_supports_goodbye_but_not_extended() {
    let ctx = receiver_for(28);
    assert!(ctx.protocol.supports_goodbye_exchange());
    assert!(!ctx.protocol.supports_extended_goodbye());
    assert!(!ctx.protocol.supports_multi_phase());
}

#[test]
fn proto29_supports_goodbye_but_not_extended() {
    let ctx = receiver_for(29);
    assert!(ctx.protocol.supports_goodbye_exchange());
    assert!(!ctx.protocol.supports_extended_goodbye());
    assert!(ctx.protocol.supports_multi_phase());
}

#[test]
fn exchange_phase_done_proto28_single_phase() {
    let mut ctx = receiver_for(28);

    let mut sender_input = Vec::new();
    sender_input.extend_from_slice(&NDX_DONE_LE); // echo for phase 1
    sender_input.extend_from_slice(&NDX_DONE_LE); // sender's post-loop final

    let mut reader = Cursor::new(sender_input);
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(28);
    let mut ndx_read = create_ndx_codec(28);

    ctx.exchange_phase_done(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();

    // 2 NDX_DONEs = 8 bytes, all 0xFF
    assert_eq!(output.len(), 8);
    assert_eq!(&output[0..4], &NDX_DONE_LE);
    assert_eq!(&output[4..8], &NDX_DONE_LE);
}

#[test]
fn exchange_phase_done_proto29_two_phases() {
    let mut ctx = receiver_for(29);

    let mut sender_input = Vec::new();
    for _ in 0..3 {
        sender_input.extend_from_slice(&NDX_DONE_LE);
    }

    let mut reader = Cursor::new(sender_input);
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(29);
    let mut ndx_read = create_ndx_codec(29);

    ctx.exchange_phase_done(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();

    // 3 NDX_DONEs = 12 bytes
    assert_eq!(output.len(), 12);
    for i in 0..3 {
        assert_eq!(&output[i * 4..(i + 1) * 4], &NDX_DONE_LE);
    }
}

#[test]
fn handle_goodbye_proto28_sends_single_ndx_done() {
    let ctx = receiver_for(28);

    let mut reader = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(28);
    let mut ndx_read = create_ndx_codec(28);

    ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();

    assert_eq!(output.len(), 4);
    assert_eq!(&output[..], &NDX_DONE_LE);
}

#[test]
fn handle_goodbye_proto29_sends_single_ndx_done() {
    let ctx = receiver_for(29);

    let mut reader = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(29);
    let mut ndx_read = create_ndx_codec(29);

    ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();

    assert_eq!(output.len(), 4);
    assert_eq!(&output[..], &NDX_DONE_LE);
}

#[test]
fn legacy_ndx_done_wire_format_matches_read_int() {
    let mut codec = create_ndx_codec(28);
    let mut buf = Vec::new();
    codec.write_ndx_done(&mut buf).unwrap();

    assert_eq!(buf, (-1i32).to_le_bytes());
}

#[test]
fn exchange_phase_done_proto28_reads_all_sender_bytes() {
    let mut ctx = receiver_for(28);

    let mut sender_input = Vec::new();
    sender_input.extend_from_slice(&NDX_DONE_LE);
    sender_input.extend_from_slice(&NDX_DONE_LE);

    let mut reader = Cursor::new(sender_input);
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(28);
    let mut ndx_read = create_ndx_codec(28);

    ctx.exchange_phase_done(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();

    // All sender bytes consumed
    assert_eq!(reader.position(), 8);
}
