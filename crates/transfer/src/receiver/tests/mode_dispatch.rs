//! Regression: the receiver's drive-mode dispatch is decided once and
//! implemented once, so the two pipelined drivers cannot diverge.
//!
//! # Why this file exists
//!
//! `run_pipelined` and `run_pipelined_incremental` each used to open-code an
//! `if list_only { } else if dry_run { } else { }` ladder. A mode added to one
//! was silently missing from the other, and the divergence was invisible
//! because the two are never exercised together: the shipped binary sets
//! `default-features = false` and takes `run_pipelined`, while
//! `--workspace --all-features` (and every library consumer, since
//! `incremental-flist` is a `transfer` default) takes
//! `run_pipelined_incremental`. Production ran one ladder and CI tested the
//! other. The same drift recurred four times.
//!
//! Every test here runs under BOTH feature settings, because
//! `super::super::transfer::mode` is compiled unconditionally: no `#[cfg]`
//! selects it. That is the point - the CI feature matrix runs this module with
//! `incremental-flist` on and off, and both cells assert the same decision and
//! the same wire shape.
//!
//! # Upstream Reference
//!
//! - `main.c:1839` - `if (write_batch < 0) dry_run = 1`, `do_xfers` stays 1.
//! - `sender.c:442-443` - `write_ndx_and_attrs(f_out)` then
//!   `write_sum_head(f_xfer)`: the sender reads a sum head per file whenever
//!   `do_xfers` is set, so `--only-write-batch` must send one and `--dry-run`
//!   must not.
//! - `generator.c:1249` - `--list-only` sends no per-file request at all.

use std::io::Cursor;
use std::num::NonZeroU8;

use protocol::codec::{NdxCodec, create_ndx_codec};
use protocol::flist::FileEntry;

use super::super::stats::TransferStats;
use super::super::transfer::mode::{NonTransferMode, ReceiverMode};
use super::super::{PipelineSetup, ReceiverContext};
use super::support::test_handshake;
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;

/// Builds a receiver whose flag set is the one property under test.
fn receiver(flags: ParsedServerFlags) -> ReceiverContext {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
        flags,
        ..Default::default()
    };
    ReceiverContext::new_for_test(&handshake, config)
}

/// `--only-write-batch` implies `--dry-run` upstream (`main.c:1839` sets
/// `dry_run = 1` while leaving `do_xfers = 1`), so the two flags are always
/// seen together and only the check ORDER tells them apart.
///
/// WHY this matters rather than merely "the enum has the right variant": the
/// dry-run body writes NDX + iflags and stops. Under `--only-write-batch` the
/// peer's `send_files()` goes on to read a sum head (`sender.c:442-443`), so
/// taking the dry-run body deadlocks the pair - the sender blocks on a read
/// that never arrives while the receiver blocks on the echo. Reordering these
/// two checks is therefore a hang, not a cosmetic difference.
#[test]
fn only_write_batch_outranks_the_dry_run_it_implies() {
    let ctx = receiver(ParsedServerFlags {
        only_write_batch: true,
        dry_run: true,
        ..Default::default()
    });
    assert_eq!(
        ctx.select_mode(),
        ReceiverMode::NonTransfer(NonTransferMode::OnlyWriteBatch),
        "only-write-batch sets dry_run too (main.c:1839); the dry-run body \
         omits the sum head sender.c:442-443 requires and hangs both ends"
    );
}

/// `--list-only` renders every entry via `list_file_entry()` and sends no
/// per-file NDX request at all (`generator.c:1249`), so it must outrank every
/// mode that does send one - including a `-n --list-only` combination.
#[test]
fn list_only_outranks_every_requesting_mode() {
    let ctx = receiver(ParsedServerFlags {
        list_only: true,
        dry_run: true,
        only_write_batch: true,
        ..Default::default()
    });
    assert_eq!(
        ctx.select_mode(),
        ReceiverMode::NonTransfer(NonTransferMode::ListOnly),
        "list-only sends no per-file request; any other mode would put NDX \
         requests on a wire the peer is not reading them from"
    );
}

/// A plain `-n` with no batch flag takes the dry-run body.
#[test]
fn plain_dry_run_selects_dry_run() {
    let ctx = receiver(ParsedServerFlags {
        dry_run: true,
        ..Default::default()
    });
    assert_eq!(
        ctx.select_mode(),
        ReceiverMode::NonTransfer(NonTransferMode::DryRun)
    );
}

/// The default: a real transfer. This is the one mode whose body is allowed to
/// differ per driver, so it must be the only value that is not `NonTransfer`.
#[test]
fn no_mode_flag_selects_the_full_transfer() {
    let ctx = receiver(ParsedServerFlags::default());
    assert_eq!(ctx.select_mode(), ReceiverMode::Transfer);
}

/// A server-side receiver (the remote end of a push) whose peer echoes one
/// `NDX + iflags` response per request.
///
/// `client_mode = false` keeps `run_only_write_batch_loop` off the
/// `discard_receive_data()` path (`receiver.c:813` gates it on `!am_server`),
/// so the scripted response is exactly the echo and nothing more.
fn echo_response(ndx: i32) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut codec = create_ndx_codec(32);
    codec.write_ndx(&mut buf, ndx).expect("write echo ndx");
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf
}

/// Drives one non-transfer mode over a scripted echo and returns the bytes the
/// receiver put on the wire.
fn drive(mode: NonTransferMode, dest: &std::path::Path) -> Vec<u8> {
    let entry = FileEntry::new_file("f".into(), 4, 0o644);
    let mut ctx = receiver(ParsedServerFlags {
        // Set both, as upstream does, and let `mode` pick the body: the request
        // shape must follow the mode, not the raw flags.
        dry_run: true,
        only_write_batch: matches!(mode, NonTransferMode::OnlyWriteBatch),
        ..Default::default()
    });
    ctx.config.connection.client_mode = false;
    ctx.file_list = std::sync::Arc::new(vec![entry]);

    let setup = PipelineSetup {
        dest_dir: dest.to_path_buf(),
        metadata_opts: metadata::MetadataOptions::default(),
        checksum_length: NonZeroU8::new(2).expect("nonzero"),
        // Any algorithm does: the basis is absent, so no strong sum is ever
        // computed - only the sum head's presence is under test.
        checksum_algorithm: signature::SignatureAlgorithm::Md4,
        acl_cache: None,
        acl_id_map: None,
        #[cfg(unix)]
        sandbox: None,
    };

    let files: Vec<(usize, &FileEntry, std::path::PathBuf, u32)> =
        vec![(0, &ctx.file_list[0], dest.join("f"), 0)];
    let sent = SharedBuf::default();
    let mut reader = crate::reader::ServerReader::new_plain(Cursor::new(echo_response(0)));
    let mut writer = crate::writer::ServerWriter::new_plain(sent.clone());
    let mut stats = TransferStats::default();

    ctx.run_non_transfer_mode(mode, &mut reader, &mut writer, &setup, &files, &mut stats)
        .expect("non-transfer mode drives to completion");
    drop(writer);
    sent.take()
}

/// Capture sink for the bytes a driver puts on the wire. `ServerWriter` owns
/// its sink and exposes no accessor, so the buffer is shared rather than
/// reclaimed.
#[derive(Clone, Default)]
struct SharedBuf(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl SharedBuf {
    fn take(self) -> Vec<u8> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Reads the `NDX + iflags` request header off `bytes`, returning what remains.
fn skip_request_header(bytes: &[u8]) -> &[u8] {
    let mut cursor = Cursor::new(bytes);
    let mut codec = create_ndx_codec(32);
    codec.read_ndx(&mut cursor).expect("request carries an NDX");
    let consumed = cursor.position() as usize;
    &bytes[consumed + 2..]
}

/// The bug this whole change exists to make impossible: `--only-write-batch`
/// must put a sum head on the wire and `--dry-run` must not.
///
/// WHY assert the wire and not just the enum: the enum is only a label. What
/// the sender actually blocks on is `write_sum_head(f_xfer)`
/// (`sender.c:442-443`), which it reads whenever `do_xfers` is set - and
/// `--only-write-batch` leaves `do_xfers = 1` (`main.c:1839`). A receiver that
/// took the dry-run body under `--only-write-batch` produced a byte-correct
/// prefix and then hung, which no assertion over flags or exit codes catches.
///
/// Both halves are asserted together on purpose: pinning only the sum head
/// would let the dry run start emitting one (desyncing the plain `-n` path in
/// the opposite direction), and pinning only its absence would let the bug back.
#[test]
fn only_write_batch_sends_a_sum_head_and_dry_run_does_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Give the file a basis on disk so the sum head describes a REAL signature.
    // Without one the head is all zeros and the assertion below could not tell
    // a genuine `--only-write-batch` request from a zero-filled placeholder.
    std::fs::write(dir.path().join("f"), b"data").expect("seed basis");

    let batch = drive(NonTransferMode::OnlyWriteBatch, dir.path());
    let after_batch_header = skip_request_header(&batch);
    let mut batch_tail = Cursor::new(after_batch_header);
    let sum_head = super::super::wire::SumHead::read(&mut batch_tail)
        .expect("only-write-batch sends a sum head, sender.c:443");
    assert_eq!(
        sum_head.count, 1,
        "the sum head must describe the basis block the generator computed \
         (do_xfers stays 1 under --only-write-batch, main.c:1839)"
    );
    assert_eq!(
        sum_head.s2length as u8, 2,
        "the sum head must carry the phase-1 short checksum length the \
         generator negotiated, not a placeholder"
    );

    let plain = drive(NonTransferMode::DryRun, dir.path());
    assert!(
        skip_request_header(&plain).is_empty(),
        "a plain --dry-run request is NDX + iflags and nothing else \
         (generator.c:1858-1959 skips write_sum_head); trailing bytes {:?} \
         would be parsed by the sender as its next frame header",
        skip_request_header(&plain)
    );
}

/// `--list-only` puts no per-file request on the wire at all, and reports its
/// entries through `stats.list_only_entries` instead (`generator.c:1249`).
#[test]
fn list_only_sends_no_per_file_request() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(
        drive(NonTransferMode::ListOnly, dir.path()).is_empty(),
        "list-only must send no NDX; the peer's send_files() is not reading \
         per-file requests in this mode"
    );
}
