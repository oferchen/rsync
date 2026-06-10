//! Tests for `NDX_DEL_STATS` emission from the receiver's goodbye handshake.
//!
//! The daemon-receive (push) direction does not run a separate generator
//! process. The receiver itself must emit `NDX_DEL_STATS` during goodbye so
//! the sender's `read_final_goodbye()` can decode the per-type deletion
//! counters when the upstream conditions are met.
//!
//! upstream:
//! - `generator.c:2376-2381` early `write_del_stats()` path
//! - `generator.c:2420-2425` late `write_del_stats()` path
//! - `main.c:225-238` `write_del_stats()` wire format
//! - `rsync.c:337-342` `read_ndx_and_attrs()` consumes NDX_DEL_STATS

use std::ffi::OsString;
use std::io::Cursor;

use protocol::codec::{NDX_DEL_STATS, NdxCodec, create_ndx_codec};
use protocol::stats::DeleteStats;
use protocol::{ProtocolVersion, read_varint};

use super::super::super::ReceiverContext;
use crate::config::{DeletionConfig, ServerConfig};
use crate::flags::ParsedServerFlags;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

const NDX_DONE_LE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

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

/// Builds a receiver configured to mirror a daemon-receive push:
/// `--delete --stats`, default early-delete timing, and the requested
/// protocol version.
fn receiver_with_delete_and_stats(protocol_version: u8) -> ReceiverContext {
    let handshake = handshake_for(protocol_version);
    let flags = ParsedServerFlags {
        delete: true,
        ..ParsedServerFlags::default()
    };
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        flags,
        deletion: DeletionConfig {
            max_delete: None,
            ignore_errors: false,
            late_delete: false,
        },
        do_stats: true,
        ..Default::default()
    };
    ReceiverContext::new_for_test(&handshake, config)
}

/// Verifies that a protocol 31 daemon-receiver with `--delete --stats` emits
/// `NDX_DEL_STATS` followed by 5 varints carrying the per-type counters,
/// then closes goodbye with `NDX_DONE`. Mirrors what upstream's
/// `read_final_goodbye()` expects to decode on the sender side.
#[test]
fn handle_goodbye_proto31_emits_ndx_del_stats_then_done() {
    let mut ctx = receiver_with_delete_and_stats(31);
    let stats = DeleteStats {
        files: 7,
        dirs: 3,
        symlinks: 2,
        devices: 1,
        specials: 4,
    };
    ctx.set_pending_del_stats(stats);

    // Sender's echo + final NDX_DONE
    let mut sender_input = Vec::new();
    sender_input.extend_from_slice(&NDX_DONE_LE);
    sender_input.extend_from_slice(&NDX_DONE_LE);

    let mut reader = Cursor::new(sender_input);
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(31);
    let mut ndx_read = create_ndx_codec(31);

    ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();

    let mut cursor = Cursor::new(&output);
    let mut codec = create_ndx_codec(31);
    let first = codec.read_ndx(&mut cursor).unwrap();
    assert_eq!(
        first, NDX_DEL_STATS,
        "first frame must be NDX_DEL_STATS, got {first}"
    );

    let decoded = DeleteStats::read_from(&mut cursor).unwrap();
    assert_eq!(decoded, stats, "del stats round-trip mismatch");

    let second = codec.read_ndx(&mut cursor).unwrap();
    assert_eq!(second, -1, "second frame must be NDX_DONE");

    let third = codec.read_ndx(&mut cursor).unwrap();
    assert_eq!(
        third, -1,
        "third frame must be the extended-goodbye NDX_DONE"
    );

    assert_eq!(
        cursor.position() as usize,
        output.len(),
        "trailing bytes after extended goodbye"
    );
}

/// Verifies that the receiver does not emit `NDX_DEL_STATS` when `--stats`
/// is absent, mirroring upstream's `INFO_GTE(STATS, 2)` gate.
#[test]
fn handle_goodbye_proto31_skips_ndx_del_stats_without_do_stats() {
    let handshake = handshake_for(31);
    let flags = ParsedServerFlags {
        delete: true,
        ..ParsedServerFlags::default()
    };
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(31).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        flags,
        do_stats: false,
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.set_pending_del_stats(DeleteStats {
        files: 9,
        ..DeleteStats::new()
    });

    let mut sender_input = Vec::new();
    sender_input.extend_from_slice(&NDX_DONE_LE);
    sender_input.extend_from_slice(&NDX_DONE_LE);

    let mut reader = Cursor::new(sender_input);
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(31);
    let mut ndx_read = create_ndx_codec(31);

    ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();

    // Two NDX_DONEs only - no NDX_DEL_STATS.
    assert_eq!(output.len(), 8);
    assert_eq!(&output[..4], &NDX_DONE_LE);
    assert_eq!(&output[4..], &NDX_DONE_LE);
}

/// Late-delete timing (`--delete-delay` / `--delete-after`) must emit
/// `NDX_DEL_STATS` whenever `do_stats` is set, even without the `delete`
/// flag (upstream gates on `INFO_GTE(STATS, 2)` alone in the late path).
#[test]
fn handle_goodbye_proto31_late_delete_emits_on_do_stats_only() {
    let handshake = handshake_for(31);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(31).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        deletion: DeletionConfig {
            max_delete: None,
            ignore_errors: false,
            late_delete: true,
        },
        do_stats: true,
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.set_pending_del_stats(DeleteStats {
        files: 1,
        dirs: 2,
        symlinks: 3,
        devices: 4,
        specials: 5,
    });

    let mut sender_input = Vec::new();
    sender_input.extend_from_slice(&NDX_DONE_LE);
    sender_input.extend_from_slice(&NDX_DONE_LE);

    let mut reader = Cursor::new(sender_input);
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(31);
    let mut ndx_read = create_ndx_codec(31);

    ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();

    let mut cursor = Cursor::new(&output);
    let mut codec = create_ndx_codec(31);
    assert_eq!(codec.read_ndx(&mut cursor).unwrap(), NDX_DEL_STATS);
    let decoded = DeleteStats::read_from(&mut cursor).unwrap();
    assert_eq!(decoded.files, 1);
    assert_eq!(decoded.dirs, 2);
    assert_eq!(decoded.symlinks, 3);
    assert_eq!(decoded.devices, 4);
    assert_eq!(decoded.specials, 5);
    assert_eq!(codec.read_ndx(&mut cursor).unwrap(), -1);
    assert_eq!(codec.read_ndx(&mut cursor).unwrap(), -1);
}

/// Pre-protocol-31 transports must never emit `NDX_DEL_STATS` even with
/// `--delete --stats` set, since the message did not exist before
/// protocol 31. Mirrors upstream's `protocol_version >= 31` guard.
#[test]
fn handle_goodbye_proto30_never_emits_ndx_del_stats() {
    let handshake = handshake_for(30);
    let flags = ParsedServerFlags {
        delete: true,
        ..ParsedServerFlags::default()
    };
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(30).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        flags,
        do_stats: true,
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.set_pending_del_stats(DeleteStats {
        files: 9,
        ..DeleteStats::new()
    });

    let mut reader = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(30);
    let mut ndx_read = create_ndx_codec(30);

    ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();

    assert_eq!(output.len(), 4);
    assert_eq!(&output[..], &NDX_DONE_LE);
}

/// Wire-format spot check: the five fields after `NDX_DEL_STATS` are
/// independent varints in (files, dirs, symlinks, devices, specials)
/// order. Asserts the order matches upstream `main.c:225-238`.
#[test]
fn handle_goodbye_proto31_varint_field_order_matches_upstream() {
    let mut ctx = receiver_with_delete_and_stats(31);
    let stats = DeleteStats {
        files: 11,
        dirs: 22,
        symlinks: 33,
        devices: 44,
        specials: 55,
    };
    ctx.set_pending_del_stats(stats);

    let mut sender_input = Vec::new();
    sender_input.extend_from_slice(&NDX_DONE_LE);
    sender_input.extend_from_slice(&NDX_DONE_LE);

    let mut reader = Cursor::new(sender_input);
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(31);
    let mut ndx_read = create_ndx_codec(31);

    ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();

    let mut cursor = Cursor::new(&output);
    let mut codec = create_ndx_codec(31);
    assert_eq!(codec.read_ndx(&mut cursor).unwrap(), NDX_DEL_STATS);

    // Verify each varint independently in upstream order.
    assert_eq!(read_varint(&mut cursor).unwrap(), 11);
    assert_eq!(read_varint(&mut cursor).unwrap(), 22);
    assert_eq!(read_varint(&mut cursor).unwrap(), 33);
    assert_eq!(read_varint(&mut cursor).unwrap(), 44);
    assert_eq!(read_varint(&mut cursor).unwrap(), 55);
}
