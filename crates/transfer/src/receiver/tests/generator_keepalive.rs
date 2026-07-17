//! Generator keepalive emission during local disk work.
//!
//! Upstream rsync's generator pokes `maybe_send_keepalive()` while it is busy
//! with local disk work so a remote sender's `--timeout` does not fire during a
//! long silent stretch. The three sites are `delete_in_dir()`
//! (generator.c:295-296), `touch_up_dirs()` (generator.c:2138-2144), and the
//! `generate_files()` per-file loop (generator.c:2348-2353).
//!
//! These tests drive each oc phase with a multiplexed writer and assert:
//!
//! - with a lull configured (`--timeout` set) and the lull elapsed, the phase
//!   emits at least one empty `MSG_DATA` keepalive frame, and
//! - with no lull (`--timeout` unset), the phase writes nothing - proving the
//!   change is a strict no-op that leaves the default transfer path
//!   byte-for-byte identical.
//!
//! Elapsed time is driven deterministically by configuring the lull to
//! `Duration::ZERO` (any elapsed interval already exceeds it), never by sleeping.

use std::ffi::OsString;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use metadata::MetadataOptions;
use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use super::support::{test_config, test_handshake};
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::receiver::stats::TransferStats;
use crate::role::ServerRole;
use crate::writer::{CountingWriter, MsgInfoSender, ServerWriter};

/// The exact wire bytes of an empty `MSG_DATA` keepalive frame: a 4-byte header
/// with tag `MPLEX_BASE (7) + MSG_DATA (0)` in the high byte and a zero payload
/// length, little-endian (`0x07000000` -> `[0, 0, 0, 7]`).
const KEEPALIVE_FRAME: [u8; 4] = [0, 0, 0, 7];

/// A `Write` sink that captures every byte into a shared buffer so a test can
/// inspect the multiplexed wire output after driving a receiver phase.
#[derive(Clone, Default)]
struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SharedSink {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

/// Builds a multiplexed [`ServerWriter`] over a capturing sink with the given
/// lull. `Some(Duration::ZERO)` makes every elapsed interval exceed the lull, so
/// each `maybe_send_keepalive()` deterministically emits; `None` mirrors an
/// unset `--timeout` and must stay a strict no-op.
fn mux_writer(lull: Option<Duration>) -> (ServerWriter<SharedSink>, SharedSink) {
    let sink = SharedSink::default();
    let mut writer = ServerWriter::new_plain(sink.clone())
        .activate_multiplex()
        .expect("activate multiplex");
    writer.set_allowed_lull(lull);
    (writer, sink)
}

/// Asserts the captured bytes are one or more empty `MSG_DATA` keepalive frames
/// and nothing else, so the phase's only wire output was keepalives.
fn assert_only_keepalives(bytes: &[u8]) {
    assert!(
        !bytes.is_empty(),
        "expected at least one keepalive frame, got none"
    );
    assert_eq!(
        bytes.len() % KEEPALIVE_FRAME.len(),
        0,
        "captured bytes are not a whole number of keepalive frames: {bytes:?}"
    );
    for chunk in bytes.chunks_exact(KEEPALIVE_FRAME.len()) {
        assert_eq!(
            chunk, KEEPALIVE_FRAME,
            "captured a non-keepalive frame: {bytes:?}"
        );
    }
}

/// A receiver config with `--times` so `touch_up_dirs` does real work (and thus
/// walks the file list, poking keepalives) rather than returning early.
fn config_with_times() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            times: true,
            ..ParsedServerFlags::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    }
}

/// The delete pass (upstream `delete_in_dir`) pokes a keepalive at the start of
/// the scan when a lull is configured, and nothing when it is not.
#[test]
fn delete_pass_keepalive_gated_on_timeout() {
    let hs = test_handshake();
    let dir = test_support::create_tempdir();

    // Lull configured: the entry poke (plus the post-parallel-scan poke) emits.
    let ctx = ReceiverContext::new_for_test(&hs, test_config());
    let (mut writer, sink) = mux_writer(Some(Duration::ZERO));
    ctx.delete_extraneous_files(
        dir.path(),
        #[cfg(unix)]
        None,
        &mut writer,
    )
    .expect("delete pass succeeds");
    assert_only_keepalives(&sink.bytes());

    // No lull: strict no-op, nothing crosses the wire.
    let (mut writer, sink) = mux_writer(None);
    ctx.delete_extraneous_files(
        dir.path(),
        #[cfg(unix)]
        None,
        &mut writer,
    )
    .expect("delete pass succeeds");
    assert!(
        sink.bytes().is_empty(),
        "delete pass must be a strict no-op without --timeout"
    );
}

/// The final directory-metadata retouch (upstream `touch_up_dirs`) pokes a
/// keepalive per directory entry when a lull is configured, and nothing when it
/// is not.
#[test]
fn touch_up_dirs_keepalive_gated_on_timeout() {
    let hs = test_handshake();
    let dir = test_support::create_tempdir();

    let mut ctx = ReceiverContext::new_for_test(&hs, config_with_times());
    ctx.file_list = vec![
        FileEntry::new_directory("a".into(), 0o755),
        FileEntry::new_directory("b".into(), 0o755),
    ];

    let (mut writer, sink) = mux_writer(Some(Duration::ZERO));
    ctx.touch_up_dirs(dir.path(), &mut writer);
    assert_only_keepalives(&sink.bytes());

    let (mut writer, sink) = mux_writer(None);
    ctx.touch_up_dirs(dir.path(), &mut writer);
    assert!(
        sink.bytes().is_empty(),
        "touch_up_dirs must be a strict no-op without --timeout"
    );
}

/// The per-file generate loop (upstream `generate_files`) pokes a keepalive per
/// candidate when a lull is configured, and nothing when it is not.
#[test]
fn build_files_to_transfer_keepalive_gated_on_timeout() {
    let hs = test_handshake();
    let dir = test_support::create_tempdir();
    let opts = MetadataOptions::default();

    let mut ctx = ReceiverContext::new_for_test(&hs, test_config());
    // New regular files with no destination present take the no-output new-file
    // path, so the loop's only wire output is the per-file keepalive.
    ctx.file_list = vec![
        FileEntry::new_file("f1".into(), 4, 0o644),
        FileEntry::new_file("f2".into(), 4, 0o644),
    ];

    let (mut writer, sink) = mux_writer(Some(Duration::ZERO));
    let mut errors = Vec::new();
    let mut stats = TransferStats::default();
    let _ = ctx.build_files_to_transfer(
        &mut writer,
        dir.path(),
        &opts,
        None,
        &mut errors,
        &mut stats,
        None,
        None,
    );
    assert_only_keepalives(&sink.bytes());

    let (mut writer, sink) = mux_writer(None);
    let mut errors = Vec::new();
    let mut stats = TransferStats::default();
    let _ = ctx.build_files_to_transfer(
        &mut writer,
        dir.path(),
        &opts,
        None,
        &mut errors,
        &mut stats,
        None,
        None,
    );
    assert!(
        sink.bytes().is_empty(),
        "build_files_to_transfer must be a strict no-op without --timeout"
    );
}

/// The receiver's production writer is a `CountingWriter<&mut ServerWriter>`.
/// This pins that the keepalive method forwards through that wrapper (and stays
/// gated), so the phases above reach the real lull-gated emitter at runtime.
#[test]
fn counting_writer_forwards_keepalive() {
    let (mut inner, sink) = mux_writer(Some(Duration::ZERO));
    {
        let mut cw = CountingWriter::new(&mut inner);
        assert!(
            cw.maybe_send_keepalive().expect("keepalive"),
            "wrapped writer must emit when the lull has elapsed"
        );
    }
    assert_eq!(sink.bytes(), KEEPALIVE_FRAME);

    let (mut inner, sink) = mux_writer(None);
    {
        let mut cw = CountingWriter::new(&mut inner);
        assert!(
            !cw.maybe_send_keepalive().expect("keepalive"),
            "wrapped writer must stay silent without --timeout"
        );
    }
    assert!(sink.bytes().is_empty());
}

/// A plain (non-multiplexed) writer never emits a keepalive even with a lull
/// configured, matching upstream's `am_server`-gated emission: keepalives ride
/// the multiplexed server stream only.
#[test]
fn plain_writer_never_emits_keepalive() {
    let mut writer = ServerWriter::new_plain(Vec::new());
    writer.set_allowed_lull(Some(Duration::ZERO));
    assert!(
        !MsgInfoSender::maybe_send_keepalive(&mut writer).expect("keepalive"),
        "plain-mode writer must never emit a keepalive"
    );
    match writer {
        ServerWriter::Plain(buf) => assert!(buf.is_empty()),
        _ => panic!("writer should still be plain"),
    }
}
