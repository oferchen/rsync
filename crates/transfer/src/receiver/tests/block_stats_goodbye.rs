//! `MSG_BLOCK_STATS` placement in the receiver's goodbye handshake.
//!
//! A server receiver reports its touched-block count to the client in one
//! `MSG_BLOCK_STATS` frame, only at protocol 33+, immediately before the final
//! goodbye `NDX_DONE`. These tests pin the exact server-to-client bytes of the
//! goodbye at protocol 32 (byte-identical to before the frame existed) and at
//! protocol 33 (the same bytes with the frame spliced in).
//!
//! # Upstream Reference
//!
//! - `main.c:1112-1117` - `do_recv()` sends `MSG_BLOCK_STATS` after `NDX_DONE`
//!   and `MSG_STATS` when `protocol_version >= 33`.
//! - `io.c:1728-1729` - the server generator forwards it to the client, ahead
//!   of the final goodbye `NDX_DONE` written at `main.c:1172` (measured against
//!   rsync 3.5.1: `..., MSG_DATA "\0", MSG_BLOCK_STATS, MSG_DATA "\0"`).

use std::ffi::OsString;
use std::io::Cursor;

use protocol::ProtocolVersion;
use protocol::codec::create_ndx_codec;

use super::super::ReceiverContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;
use crate::writer::ServerWriter;

/// The modern (protocol >= 30) `NDX_DONE` as one `MSG_DATA` frame: length 1,
/// tag `MPLEX_BASE + MSG_DATA = 7`, payload `0x00`.
const NDX_DONE_FRAME: [u8; 5] = [0x01, 0x00, 0x00, 0x07, 0x00];

fn server_receiver(protocol: ProtocolVersion, touched_blocks_4k: u64) -> ReceiverContext {
    let handshake = HandshakeResult {
        protocol,
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    };
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol,
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.touched_blocks_4k = touched_blocks_4k;
    ctx
}

/// Runs the receiver's goodbye against a sender that echoes one `NDX_DONE`
/// and returns every byte the receiver wrote to the multiplexed stream.
fn goodbye_bytes(ctx: &ReceiverContext) -> Vec<u8> {
    let proto = ctx.protocol.as_u8();
    let mut reader = Cursor::new(vec![0x00]);
    let mut out = Vec::new();
    {
        let mut writer = ServerWriter::new_plain(&mut out)
            .activate_multiplex()
            .unwrap();
        let mut ndx_write = create_ndx_codec(proto);
        let mut ndx_read = create_ndx_codec(proto);
        ctx.handle_goodbye(&mut reader, &mut writer, &mut ndx_write, &mut ndx_read)
            .unwrap();
    }
    out
}

fn block_stats_frame(count: i64) -> Vec<u8> {
    let mut frame = vec![0x08, 0x00, 0x00, 0x12];
    frame.extend_from_slice(&count.to_le_bytes());
    frame
}

/// WHY: protocol 32 must stay byte-identical to the pre-block-stats wire: the
/// receiver's count is tracked unconditionally, but no frame may leak onto a
/// protocol-32 stream (a 3.5.0 peer treats tag 11 as a fatal unknown tag).
#[test]
fn proto32_goodbye_is_byte_identical_and_carries_no_block_stats() {
    let mut golden = NDX_DONE_FRAME.to_vec();
    golden.extend_from_slice(&NDX_DONE_FRAME);

    for touched in [0, 5, 1_024] {
        let ctx = server_receiver(ProtocolVersion::V32, touched);
        assert_eq!(
            goodbye_bytes(&ctx),
            golden,
            "protocol 32 goodbye changed with {touched} touched blocks"
        );
    }
}

/// WHY: at protocol 33 the server receiver owes the client its count, framed
/// exactly as upstream forwards it: between the goodbye `NDX_DONE` and the
/// final one, which is where a 3.5.1 client sender reads it before printing
/// `Number of 4 KiB logical blocks touched`.
#[test]
fn proto33_goodbye_sends_block_stats_before_final_ndx_done() {
    let ctx = server_receiver(ProtocolVersion::V33, 1_024);
    let mut expected = NDX_DONE_FRAME.to_vec();
    expected.extend_from_slice(&block_stats_frame(1_024));
    expected.extend_from_slice(&NDX_DONE_FRAME);
    assert_eq!(goodbye_bytes(&ctx), expected);
}

/// WHY: upstream sends the frame even for a zero count (main.c:1112 gates only
/// on the protocol), and the client prints 0 either way.
#[test]
fn proto33_goodbye_sends_zero_count() {
    let ctx = server_receiver(ProtocolVersion::V33, 0);
    let mut expected = NDX_DONE_FRAME.to_vec();
    expected.extend_from_slice(&block_stats_frame(0));
    expected.extend_from_slice(&NDX_DONE_FRAME);
    assert_eq!(goodbye_bytes(&ctx), expected);
}

/// WHY: a client receiver (pull) keeps its count local - upstream's receiver
/// hands it to its own generator over the error pipe and it never reaches the
/// wire - so a protocol-33 pull goodbye carries no frame toward the sender.
#[test]
fn proto33_client_receiver_goodbye_sends_no_block_stats() {
    let mut ctx = server_receiver(ProtocolVersion::V33, 9);
    ctx.config.connection.client_mode = true;
    let mut expected = NDX_DONE_FRAME.to_vec();
    expected.extend_from_slice(&NDX_DONE_FRAME);
    assert_eq!(goodbye_bytes(&ctx), expected);
}
