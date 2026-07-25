//! `--write-batch` must honour `-z`.
//!
//! # Upstream Reference
//!
//! - `batch.c:59-76 flag_ptr[]` - `&do_compression` sits at stream-flag bit 8
//!   (protocol >= 29), right after `&xfer_dirs`.
//! - `batch.c:96-113 write_stream_flags()` - the bitmap of active flags is the
//!   first `write_int()` in the batch file, so `-a` alone yields 0x9f and
//!   `-az` yields 0x19f.
//! - `io.c:read_buf()` - `write_batch_monitor_in` tees the wire bytes to
//!   `batch_fd` before decompression, so a batch recorded under `-z` carries
//!   `token.c:send_deflated_token()` framing rather than
//!   `token.c:simple_send_token()`'s plain 4-byte length prefixes.
//! - `batch.c:120-161 check_batch_flags()` - the reader reconciles the recorded
//!   bitmap against the replay invocation, so both shapes must stay readable.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use batch::{BatchConfig, BatchFlags, BatchMode, BatchReader, BatchWriter};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use protocol::CompatibilityFlags;
use tempfile::tempdir;

/// Negotiated protocol for every batch in this module. 32 is the newest
/// version oc-rsync speaks and is well above the protocol-29 floor that gates
/// stream-flag bit 8.
const PROTOCOL: i32 = 32;

/// Highly compressible payload, large enough to span several 32 KiB capture
/// iterations so the deflate stream is exercised across internal flushes.
fn payload() -> Vec<u8> {
    b"the quick brown fox jumps over the lazy dog. "
        .iter()
        .copied()
        .cycle()
        .take(200_000)
        .collect()
}

/// Builds the batch writer for a `-rl [-z] --only-write-batch` capture.
///
/// `compress` drives stream-flag bit 8 exactly as production does: both the
/// header bit and the token framing derive from the single `--compress` state
/// (upstream `batch.c:68 &do_compression`).
fn make_writer(path: &Path, compress: bool) -> Arc<Mutex<BatchWriter>> {
    let compat_flags = CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::INPLACE_PARTIAL_DIR
        | CompatibilityFlags::VARINT_FLIST_FLAGS;
    let config = BatchConfig::new(
        BatchMode::OnlyWrite,
        path.to_string_lossy().into_owned(),
        PROTOCOL,
    )
    .with_compat_flags(compat_flags.bits() as i32)
    .with_checksum_seed(1);
    let mut writer = BatchWriter::new(config).expect("create batch writer");
    let flags = BatchFlags {
        recurse: true,
        preserve_links: true,
        do_compression: compress,
        ..Default::default()
    };
    writer.write_header(flags).expect("write batch header");
    Arc::new(Mutex::new(writer))
}

/// Captures `source` into a batch file under `root`, with or without `-z`.
///
/// Returns the path of the finalised batch file.
fn capture(root: &Path, source: &Path, compress: bool) -> PathBuf {
    let batch_path = root.join(if compress {
        "compressed.batch"
    } else {
        "plain.batch"
    });
    let dest = root.join("capture_dst");
    fs::create_dir_all(&dest).expect("create capture dest");

    let writer = make_writer(&batch_path, compress);
    let options = LocalCopyOptions::default()
        .recursive(true)
        .links(true)
        .compress(compress)
        .batch_writer(Some(Arc::clone(&writer)));

    let mut src_os = source.to_path_buf().into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("only-write-batch dry-run succeeds");

    Arc::try_unwrap(writer)
        .expect("writer uniquely owned")
        .into_inner()
        .expect("writer mutex not poisoned")
        .finalize()
        .expect("finalize batch writer");

    batch_path
}

/// Returns the leading stream-flags word of a batch file.
///
/// upstream: `batch.c:113 write_int(fd, flags)` is the very first thing in the
/// file, encoded little-endian.
fn stream_flags_word(batch_path: &Path) -> i32 {
    let bytes = fs::read(batch_path).expect("read batch file");
    let head: [u8; 4] = bytes[..4].try_into().expect("batch file has a flags word");
    i32::from_le_bytes(head)
}

/// Materialises a source tree holding one compressible file.
fn make_source(root: &Path) -> (PathBuf, Vec<u8>) {
    let source = root.join("src");
    fs::create_dir_all(&source).expect("create source dir");
    let body = payload();
    fs::write(source.join("body.txt"), &body).expect("write payload");
    (source, body)
}

/// Replays `batch_path` into a fresh directory under `root` and returns it.
fn replay(root: &Path, batch_path: &Path, name: &str) -> PathBuf {
    let dest = root.join(name);
    fs::create_dir_all(&dest).expect("create replay dest");
    let read_cfg = BatchConfig::new(
        BatchMode::Read,
        batch_path.to_string_lossy().into_owned(),
        PROTOCOL,
    );
    batch::replay::replay(&read_cfg, &dest, 0).expect("replay succeeds");
    dest
}

/// upstream: `batch.c:59-76` puts `&do_compression` at bit 8 and
/// `batch.c:96-113 write_stream_flags()` sets a bit for every active flag, so a
/// batch captured under `-z` must advertise bit 8 while one captured without it
/// must leave the bit clear. Without the bit, `--read-batch` would decode
/// deflated tokens with `simple_recv_token()` and reconstruct garbage.
#[test]
fn compress_drives_stream_flag_bit_8() {
    let temp = tempdir().expect("tempdir");
    let (source, _) = make_source(temp.path());

    let compressed = capture(temp.path(), &source, true);
    assert_ne!(
        stream_flags_word(&compressed) & (1 << 8),
        0,
        "bit 8 must be set when compression is active"
    );

    let plain = capture(temp.path(), &source, false);
    assert_eq!(
        stream_flags_word(&plain) & (1 << 8),
        0,
        "bit 8 must stay clear without compression"
    );
}

/// upstream: `io.c:read_buf()` tees the wire bytes before decompression, so a
/// `-z` batch holds `token.c:send_deflated_token()` output. A batch that merely
/// flipped bit 8 while still recording plain literals would be the same size as
/// the payload and would fail this bound.
#[test]
fn compressed_batch_body_is_actually_deflated() {
    let temp = tempdir().expect("tempdir");
    let (source, body) = make_source(temp.path());

    let compressed = capture(temp.path(), &source, true);
    let compressed_len = fs::metadata(&compressed).expect("stat batch").len();
    assert!(
        compressed_len < body.len() as u64 / 4,
        "a deflated batch of {} repetitive bytes must be far smaller; got {compressed_len}",
        body.len()
    );

    let plain = capture(temp.path(), &source, false);
    let plain_len = fs::metadata(&plain).expect("stat batch").len();
    assert!(
        plain_len > body.len() as u64,
        "an uncompressed batch still carries every literal byte; got {plain_len}"
    );
}

/// upstream: `batch.c:120-161 check_batch_flags()` reconciles bit 8 on read and
/// `token.c:recv_deflated_token()` decodes the body, so a compressed batch must
/// reproduce the source tree byte-for-byte.
#[test]
fn compressed_batch_round_trips_through_the_reader() {
    let temp = tempdir().expect("tempdir");
    let (source, body) = make_source(temp.path());

    let batch_path = capture(temp.path(), &source, true);

    let mut reader = BatchReader::new(BatchConfig::new(
        BatchMode::Read,
        batch_path.to_string_lossy().into_owned(),
        PROTOCOL,
    ))
    .expect("open batch reader");
    let flags = reader.read_header().expect("read batch header");
    assert!(flags.do_compression, "header must record the -z state");
    drop(reader);

    let dest = replay(temp.path(), &batch_path, "replay_compressed");
    assert_eq!(
        fs::read(dest.join("body.txt")).expect("read replayed payload"),
        body,
        "compressed batch must reconstruct the source byte-for-byte"
    );
}

/// upstream: `batch.c:96-113` writes a zero bit for an inactive flag, and
/// `token.c:simple_recv_token()` stays the reader's plain path. This guards the
/// backward-compatible shape: batches written before bit 8 was ever set (and
/// every batch written without `-z`) must keep replaying unchanged.
#[test]
fn uncompressed_batch_still_round_trips() {
    let temp = tempdir().expect("tempdir");
    let (source, body) = make_source(temp.path());

    let batch_path = capture(temp.path(), &source, false);

    let mut reader = BatchReader::new(BatchConfig::new(
        BatchMode::Read,
        batch_path.to_string_lossy().into_owned(),
        PROTOCOL,
    ))
    .expect("open batch reader");
    let flags = reader.read_header().expect("read batch header");
    assert!(!flags.do_compression, "header must leave bit 8 clear");
    drop(reader);

    let dest = replay(temp.path(), &batch_path, "replay_plain");
    assert_eq!(
        fs::read(dest.join("body.txt")).expect("read replayed payload"),
        body,
        "uncompressed batch must keep reconstructing the source byte-for-byte"
    );
}
