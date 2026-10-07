//! Where the receiver's `NDX_DEL_STATS` frame sits among its phase `NDX_DONE`s.
//!
//! upstream: generator.c:2859-2916 - the generator sends the deletion counters
//! ahead of the `NDX_DONE` that ends the sender's phase loop, never in the
//! goodbye. A server sender reads them in `send_files()` and echoes them
//! before its stats (rsync.c:338-341, main.c:357-358), which is the only way a
//! pulling client's receiver learns how many files its generator deleted.
//! Sending them one `NDX_DONE` later lets the sender leave its phase loop
//! first, so the echo lands after the stats trailer instead.

use std::ffi::OsString;
use std::io::Cursor;

use protocol::codec::{NDX_DEL_STATS, NdxCodec, create_ndx_codec};
use protocol::{DeleteStats, ProtocolVersion};

use super::super::super::ReceiverContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

const PROTOCOL: u8 = 32;

/// Deletion counters with distinct, non-zero fields so a misplaced or
/// zeroed frame cannot match by accident.
const STATS: DeleteStats = DeleteStats {
    files: 2,
    dirs: 1,
    symlinks: 3,
    devices: 0,
    specials: 4,
};

/// Which wire slot a step of the expected stream occupies.
enum Wire {
    Done,
    DelStats,
}

fn receiver(delete: bool, late_delete: bool, delay_updates: bool) -> ReceiverContext {
    let protocol = ProtocolVersion::try_from(PROTOCOL).unwrap();
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
    let mut config = ServerConfig {
        role: ServerRole::Receiver,
        protocol,
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    config.flags.delete = delete;
    config.deletion.late_delete = late_delete;
    config.write.delay_updates = delay_updates;
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.pending_del_stats = STATS;
    ctx
}

/// Encodes `steps` with one connection-wide codec, as the receiver does.
fn encode(steps: &[Wire]) -> Vec<u8> {
    let mut codec = create_ndx_codec(PROTOCOL);
    let mut out = Vec::new();
    for step in steps {
        match step {
            Wire::Done => codec.write_ndx_done(&mut out).unwrap(),
            Wire::DelStats => {
                codec.write_ndx(&mut out, NDX_DEL_STATS).unwrap();
                STATS.write_to(&mut out).unwrap();
            }
        }
    }
    out
}

/// Runs the non-INC_RECURSE phase exchange against a sender that echoes each
/// phase `NDX_DONE` and sends its post-loop final one.
fn phase_exchange_output(ctx: &mut ReceiverContext) -> Vec<u8> {
    let mut reader = Cursor::new(encode(&[Wire::Done, Wire::Done, Wire::Done]));
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(PROTOCOL);
    let mut ndx_read = create_ndx_codec(PROTOCOL);
    ctx.exchange_phase_done(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();
    output
}

/// upstream: generator.c:2859-2871 - plain `--delete` (delete-during) without
/// `--delay-updates` sends `NDX_DONE`, `NDX_DONE`, the counters, then the
/// `NDX_DONE` that ends the sender's phase loop.
#[test]
fn early_delete_stats_precede_the_third_phase_done() {
    let mut ctx = receiver(true, false, false);
    assert_eq!(
        phase_exchange_output(&mut ctx),
        encode(&[Wire::Done, Wire::Done, Wire::DelStats, Wire::Done])
    );
}

/// upstream: generator.c:2911-2915 - `--delete-delay` / `--delete-after` send
/// the counters right before the late `NDX_DONE`, which is still the one that
/// ends the sender's phase loop.
#[test]
fn late_delete_stats_precede_the_third_phase_done() {
    let mut ctx = receiver(true, true, false);
    assert_eq!(
        phase_exchange_output(&mut ctx),
        encode(&[Wire::Done, Wire::Done, Wire::DelStats, Wire::Done])
    );
}

/// upstream: generator.c:2867-2871 + 2886-2889 - under `--delay-updates` the
/// early counters follow the first `NDX_DONE` directly, because the delay
/// phase's `NDX_DONE` is held back until the redo phase finishes.
#[test]
fn early_delete_stats_under_delay_updates_precede_the_second_phase_done() {
    let mut ctx = receiver(true, false, true);
    assert_eq!(
        phase_exchange_output(&mut ctx),
        encode(&[Wire::Done, Wire::DelStats, Wire::Done, Wire::Done])
    );
}

/// upstream: generator.c:2911-2915 - a late delete is unaffected by
/// `--delay-updates`; the counters still precede the third `NDX_DONE`.
#[test]
fn late_delete_stats_under_delay_updates_precede_the_third_phase_done() {
    let mut ctx = receiver(true, true, true);
    assert_eq!(
        phase_exchange_output(&mut ctx),
        encode(&[Wire::Done, Wire::Done, Wire::DelStats, Wire::Done])
    );
}

/// No deletion mode, no counters (generator.c:2868 `delete_mode ||
/// force_delete || read_batch`).
#[test]
fn no_delete_sends_no_stats() {
    let mut ctx = receiver(false, false, false);
    assert_eq!(
        phase_exchange_output(&mut ctx),
        encode(&[Wire::Done, Wire::Done, Wire::Done])
    );
}

/// The goodbye carries only `NDX_DONE`s: the counters went out at the phase
/// boundary, and sending them again would make the sender count them twice.
#[test]
fn goodbye_does_not_resend_delete_stats() {
    let ctx = receiver(true, false, false);
    let mut reader = Cursor::new(encode(&[Wire::Done]));
    let mut output = Vec::new();
    let mut ndx_write = create_ndx_codec(PROTOCOL);
    let mut ndx_read = create_ndx_codec(PROTOCOL);
    ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
        .unwrap();
    assert_eq!(output, encode(&[Wire::Done, Wire::Done]));
}
