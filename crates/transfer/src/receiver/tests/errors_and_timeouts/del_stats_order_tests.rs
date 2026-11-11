//! When the receiver's phase-ending `NDX_DONE`s and its `NDX_DEL_STATS` frame
//! go out, relative to the sender's `NDX_DONE`s it reads.
//!
//! upstream: generator.c:2853-2872 - once the receiver has finished phase 0,
//! the generator writes the redo phase's `NDX_DONE`, the delay phase's unless
//! `--delay-updates` holds it back, and at protocol 31 an early delete's
//! counters and the `NDX_DONE` after them, without waiting for the sender.
//! generator.c:2883-2890 writes the held-back ones after the redo phase, and
//! generator.c:2910-2915 sends a late delete's counters and `NDX_DONE` only
//! after the receiver is done. rsync 3.5.1 pulling from a 3.5.1 daemon writes
//! exactly these sequences.

use std::cell::RefCell;
use std::ffi::OsString;
use std::io::{self, Cursor, Read, Write};
use std::rc::Rc;

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

/// Marks a read of one sender `NDX_DONE` in the transcript; no NDX encoding
/// on a protocol 32 wire starts with this byte.
const READ: u8 = 0xee;

/// One step of the expected transcript.
#[derive(Clone, Copy)]
enum Wire {
    /// The receiver writes `NDX_DONE`.
    Done,
    /// The receiver writes `NDX_DEL_STATS` and the counters.
    DelStats,
    /// The receiver reads the sender's `NDX_DONE`.
    Read,
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
            Wire::Read => out.push(READ),
        }
    }
    out
}

/// The receiver's writes and its reads, in the order they happen.
#[derive(Clone, Default)]
struct Transcript(Rc<RefCell<Vec<u8>>>);

impl Write for Transcript {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl crate::writer::MsgInfoSender for Transcript {}

/// A sender that answers with one `NDX_DONE` per read and logs each read.
struct Sender {
    dones: Cursor<Vec<u8>>,
    transcript: Transcript,
}

impl Read for Sender {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.transcript.0.borrow_mut().push(READ);
        self.dones.read(&mut buf[..1])
    }
}

/// Runs the non-INC_RECURSE phase exchange and the goodbye against a sender
/// that echoes every phase `NDX_DONE`, sends its post-loop one and echoes
/// the goodbye.
fn session(ctx: &mut ReceiverContext) -> Vec<u8> {
    let transcript = Transcript::default();
    let mut sender = Sender {
        dones: Cursor::new(encode(&[Wire::Done; 4])),
        transcript: transcript.clone(),
    };
    let mut writer = transcript.clone();
    let mut ndx_write = create_ndx_codec(PROTOCOL);
    let mut ndx_read = create_ndx_codec(PROTOCOL);
    ctx.exchange_phase_done(&mut sender, &mut writer, &mut ndx_write, &mut ndx_read)
        .unwrap();
    ctx.handle_goodbye(&mut sender, &mut writer, &mut ndx_write, &mut ndx_read)
        .unwrap();
    transcript.0.take()
}

use Wire::{DelStats as S, Done as D, Read as R};

/// upstream: generator.c:2862-2871 - plain `--delete` (delete-during) without
/// `--delay-updates`: the redo and delay phases' `NDX_DONE`s, the counters and
/// the `NDX_DONE` after them all follow the sender's first `NDX_DONE`.
#[test]
fn early_delete_writes_every_phase_end_after_the_first_sender_done() {
    let mut ctx = receiver(true, false, false);
    assert_eq!(session(&mut ctx), encode(&[D, R, D, D, S, D, R, R, R, D]));
}

/// upstream: generator.c:2862-2871 + 2886-2889 - under `--delay-updates` the
/// counters follow the redo phase's `NDX_DONE`; the delay phase's and the one
/// after it wait for the sender's next `NDX_DONE`.
#[test]
fn early_delete_under_delay_updates_holds_the_delay_phase_end() {
    let mut ctx = receiver(true, false, true);
    assert_eq!(session(&mut ctx), encode(&[D, R, D, S, R, D, D, R, R, D]));
}

/// upstream: generator.c:2910-2915 - `--delete-delay` / `--delete-after` send
/// the counters and their `NDX_DONE` once the sender's phase loop is over.
#[test]
fn late_delete_stats_wait_for_the_sender_phase_loop() {
    let mut ctx = receiver(true, true, false);
    assert_eq!(session(&mut ctx), encode(&[D, R, D, D, R, R, S, D, R, D]));
}

/// upstream: generator.c:2883-2890 + 2910-2915 - a late delete under
/// `--delay-updates` holds the delay phase's `NDX_DONE` for one sender
/// `NDX_DONE` and the counters until the phase loop is over.
#[test]
fn late_delete_under_delay_updates_holds_both() {
    let mut ctx = receiver(true, true, true);
    assert_eq!(session(&mut ctx), encode(&[D, R, D, R, D, R, S, D, R, D]));
}

/// No deletion mode, no counters (main.c:229 `delete_mode || force_delete ||
/// read_batch`); the protocol 31 `NDX_DONE` still goes out early.
#[test]
fn no_delete_sends_no_stats() {
    let mut ctx = receiver(false, false, false);
    assert_eq!(session(&mut ctx), encode(&[D, R, D, D, D, R, R, R, D]));
}
