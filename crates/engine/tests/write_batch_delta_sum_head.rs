//! Regression test: a `--write-batch` delta body must be preceded by the
//! `sum_head` that describes it.
//!
//! `begin_batch_file_delta()` used to emit a fixed whole-file-shaped header
//! (`count=0, blength=0, s2length=16, remainder=0`) before the executor knew
//! whether the body would be literals or block matches. A `--no-whole-file`
//! transfer then produced a batch whose tokens referenced basis blocks while
//! the header advertised none. oc-rsync replayed it anyway - its reader
//! substituted a guessed block length whenever the header carried zeros - but
//! upstream rsync 3.4.1 and 3.4.4 either abort with `Invalid block index 0
//! (count=0)` or walk off the end of a zero-length block table and crash.
//!
//! The header is now reserved at `begin_batch_file_delta()` and patched at
//! `finalize_batch_file_delta()` from the geometry the matcher actually used,
//! so the two cannot be composed independently.
//!
//! # Upstream Reference
//!
//! - `io.c:write_sum_head()` - four i32 LE fields; a whole-file transfer sends
//!   the all-zero `null_sum`, `s2length` included.
//! - `generator.c:sum_sizes_sqroot()` - a 100 KiB basis yields
//!   `blength = 700`, `count = 147`, `remainder = 200`.
//! - `receiver.c:587` - `Invalid block index %d (count=%ld)` aborts with
//!   `RERR_PROTOCOL` when a match token names a block the header omits.

use std::fs;
use std::sync::{Arc, Mutex};

use batch::{BatchConfig, BatchFlags, BatchMode, BatchWriter};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use protocol::CompatibilityFlags;
use tempfile::tempdir;

/// Appends what core's `finalize_batch` writes once the transfer ends, which
/// the `--read-batch` receiver reads: the stats (upstream: main.c:374-383)
/// and the goodbye `NDX_DONE` (main.c:933-935 read_final_goodbye()).
fn write_trailer(writer: &Mutex<BatchWriter>) {
    let mut w = writer.lock().expect("writer mutex not poisoned");
    w.write_stats(&batch::BatchStats {
        total_read: 0,
        total_written: 0,
        total_size: 0,
        flist_buildtime: Some(0),
        flist_xfertime: Some(0),
    })
    .expect("write batch stats");
    w.write_data(&[0x00]).expect("write goodbye NDX_DONE");
}

/// Replays `batch` into `dest` through the live `--read-batch` path (core
/// `ReceiverContext::for_batch_replay`), the code upstream-compatible replay
/// runs.
fn read_batch(batch: &std::path::Path, dest: &std::path::Path) {
    let out = std::process::Command::new(test_support::oc_rsync_bin())
        .arg(format!("--read-batch={}", batch.display()))
        .arg(format!("{}/", dest.display()))
        .output()
        .expect("spawn oc-rsync");
    assert!(out.status.success(), "--read-batch failed: {out:?}");
}

/// Basis size chosen so upstream's square-root block sizing is fully
/// determined: below `700 * 700` every basis uses the default 700-byte block.
const BASIS_LEN: usize = 100 * 1024;

/// Geometry upstream 3.4.4 advertises for a [`BASIS_LEN`]-byte basis,
/// confirmed by diffing its own `--write-batch` output.
const EXPECTED_COUNT: u32 = 147;
const EXPECTED_BLENGTH: u32 = 700;
const EXPECTED_REMAINDER: u32 = 200;
/// Phase-1 strong-sum width for the same basis (generator.c:745-754).
const EXPECTED_S2LENGTH: u32 = 2;

fn make_writer(path: &std::path::Path) -> Arc<Mutex<BatchWriter>> {
    let compat_flags = CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::INPLACE_PARTIAL_DIR
        | CompatibilityFlags::VARINT_FLIST_FLAGS;
    let config = BatchConfig::new(BatchMode::Write, path.to_string_lossy().into_owned(), 32)
        .with_compat_flags(compat_flags.bits() as i32)
        .with_checksum_seed(1);
    let mut writer = BatchWriter::new(config).expect("create batch writer");
    let flags = BatchFlags {
        recurse: true,
        preserve_links: true,
        ..Default::default()
    };
    writer.write_header(flags).expect("write batch header");
    Arc::new(Mutex::new(writer))
}

/// Records a delta `--write-batch` and returns the raw batch bytes.
///
/// The destination starts as an exact copy of the pre-modification source, so
/// `--no-whole-file --ignore-times` forces a genuine block-matching pass
/// instead of a whole-file resend.
fn record_delta_batch() -> Vec<u8> {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    let batch_path = temp.path().join("batch.bin");
    fs::create_dir_all(&source).expect("create source dir");
    fs::create_dir_all(&dest).expect("create dest dir");

    let basis = vec![b'A'; BASIS_LEN];
    fs::write(source.join("large.dat"), &basis).expect("write source");
    fs::write(dest.join("large.dat"), &basis).expect("write basis");

    let mut modified = basis;
    modified[50_000..50_008].copy_from_slice(b"MODIFIED");
    fs::write(source.join("large.dat"), &modified).expect("modify source");

    let writer = make_writer(&batch_path);
    let options = LocalCopyOptions::default()
        .recursive(true)
        .links(true)
        .whole_file(false)
        .ignore_times(true)
        .batch_writer(Some(Arc::clone(&writer)));

    let mut src_os = source.into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("delta write-batch succeeds");

    write_trailer(&writer);
    Arc::try_unwrap(writer)
        .expect("writer uniquely owned")
        .into_inner()
        .expect("writer mutex not poisoned")
        .finalize()
        .expect("finalize batch writer");

    fs::read(&batch_path).expect("read batch bytes")
}

/// The recorded header must carry the basis geometry, byte for byte, so a
/// peer replaying the batch resolves the body's match tokens to the same
/// basis ranges the matcher used.
#[test]
fn delta_write_batch_records_the_basis_geometry() {
    let bytes = record_delta_batch();

    let expected = protocol::wire::SumHead::with_blocks(
        EXPECTED_COUNT,
        EXPECTED_BLENGTH,
        // The batch carries no block sums, but `s2length` is still the
        // phase-1 width `sum_sizes_sqroot()` chose; upstream 3.5.1's own
        // `--write-batch` of this fixture records 2.
        EXPECTED_S2LENGTH,
        EXPECTED_REMAINDER,
    )
    .expect("valid geometry")
    .encode();

    assert!(
        bytes.windows(expected.len()).any(|w| w == expected),
        "batch must embed the basis sum_head {expected:02x?}"
    );
}

/// The header that shipped before this fix - `count=0, blength=0,
/// s2length=16` - is what upstream crashes on. It must never reappear in a
/// delta batch.
#[test]
fn delta_write_batch_never_records_a_whole_file_header() {
    let bytes = record_delta_batch();

    let mut regression = [0u8; protocol::wire::SumHead::WIRE_LEN];
    regression[8] = 16;
    assert!(
        !bytes.windows(regression.len()).any(|w| w == regression),
        "a delta batch must not advertise a whole-file sum_head"
    );
}

/// Every match token in the body must resolve against the recorded header.
///
/// This is the invariant upstream enforces at `receiver.c:587`; oc-rsync's own
/// reader now enforces it too, so a writer that drifts from its header fails
/// here rather than shipping a batch that only upstream rejects.
#[test]
fn delta_write_batch_replays_through_the_strict_reader() {
    let temp = tempdir().expect("tempdir");
    let batch_path = temp.path().join("batch.bin");
    let replay_root = temp.path().join("replay");
    fs::create_dir_all(&replay_root).expect("create replay dir");
    fs::write(&batch_path, record_delta_batch()).expect("write batch");

    // The replay side starts from the same basis the batch was recorded
    // against, which is what makes the match tokens meaningful.
    let basis = vec![b'A'; BASIS_LEN];
    fs::write(replay_root.join("large.dat"), &basis).expect("seed basis");

    read_batch(&batch_path, &replay_root);

    let mut expected = basis;
    expected[50_000..50_008].copy_from_slice(b"MODIFIED");
    assert_eq!(
        fs::read(replay_root.join("large.dat")).expect("read replayed file"),
        expected,
        "replay must reconstruct the modified source byte for byte"
    );
}
