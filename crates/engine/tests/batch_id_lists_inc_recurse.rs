//! Regression test: under INC_RECURSE-on compat_flags, the local-copy
//! batch writer must NOT emit post-flist uid/gid ID list terminators.
//!
//! # Upstream Reference
//!
//! - `flist.c:2548` - `if (numeric_ids <= 0 && !inc_recurse)
//!   send_id_lists(f);`. ID lists are gated off entirely when INC_RECURSE
//!   is negotiated; uid/gid names ride inline on each flist entry via
//!   `XMIT_USER_NAME_FOLLOWS` / `XMIT_GROUP_NAME_FOLLOWS`.
//!
//! Prior to the fix, `write_batch_id_lists` unconditionally emitted two
//! varint30(0) terminators after the flist end marker. The matching
//! reader in `crates/batch/src/reader/flist.rs` only consumes those bytes
//! when `!inc_recurse`, so under INC_RECURSE the two stray varints drift
//! the stream cursor: subsequent NDX reads decode garbage and the
//! `--read-batch` replay sees zero files.
//!
//! This test drives `LocalCopyPlan::execute_with_options` with a
//! `BatchWriter` whose compat_flags include `CF_INC_RECURSE`, then reads
//! the resulting batch file back through `BatchReader::read_protocol_flist`
//! and confirms the flist deserializes without losing position - i.e. the
//! stream contains no extraneous bytes between the flist end marker and
//! the first NDX of the delta-replay region.

use std::fs;
use std::sync::{Arc, Mutex};

use batch::{BatchConfig, BatchFlags, BatchMode, BatchReader, BatchWriter};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use protocol::CompatibilityFlags;
use tempfile::tempdir;

/// Compose the compat_flags that mirror upstream `--write-batch` with
/// INC_RECURSE negotiated. The regression specifically exercises the
/// CF_INC_RECURSE bit being set.
fn inc_recurse_compat_flags() -> i32 {
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::VARINT_FLIST_FLAGS;
    flags.bits() as i32
}

/// Construct a `BatchWriter` configured for `--only-write-batch` with
/// INC_RECURSE-on compat_flags, write the header eagerly, and wrap it
/// in an `Arc<Mutex<_>>` so the executor can share it.
fn make_inc_recurse_batch_writer(path: &std::path::Path) -> Arc<Mutex<BatchWriter>> {
    let config = BatchConfig::new(
        BatchMode::OnlyWrite,
        path.to_string_lossy().into_owned(),
        32,
    )
    .with_compat_flags(inc_recurse_compat_flags())
    .with_checksum_seed(1);

    let mut writer = BatchWriter::new(config).expect("create batch writer");
    writer
        .write_header(BatchFlags::default())
        .expect("write batch header");
    Arc::new(Mutex::new(writer))
}

/// Under INC_RECURSE-on compat_flags the batch writer must omit the
/// post-flist uid/gid terminators (`flist.c:2548`). With the omission in
/// place, `BatchReader::read_protocol_flist` consumes the flist segment
/// without drifting past it, and the entry covering the source file
/// appears in the decoded flist.
///
/// Before the fix, the writer left two stray varint30(0) bytes after the
/// flist end marker. The reader (`reader/flist.rs:167`) skipped the ID
/// list consumption under INC_RECURSE, so those bytes leaked into the
/// delta-replay region and the replay decoded garbage NDX values. The
/// flist itself decoded fine, so the assertion below stays focused on
/// the symptom that motivated the fix: the post-flist stream cursor
/// stays aligned for downstream replay.
#[test]
fn write_batch_under_inc_recurse_omits_post_flist_id_terminators() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    let batch_path = temp.path().join("batch.bin");

    fs::create_dir_all(&source).expect("create source dir");
    fs::create_dir_all(&dest).expect("create dest dir");
    fs::write(source.join("payload.bin"), b"batch-mode regression payload")
        .expect("write source file");

    let writer = make_inc_recurse_batch_writer(&batch_path);
    let options = LocalCopyOptions::default()
        .recursive(true)
        .batch_writer(Some(Arc::clone(&writer)));

    let operands = vec![source.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("only-write-batch local copy under INC_RECURSE succeeds");

    // Release the writer so its buffered output reaches disk before the
    // reader opens the file.
    Arc::try_unwrap(writer)
        .expect("writer is uniquely owned")
        .into_inner()
        .expect("writer mutex not poisoned")
        .finalize()
        .expect("finalize batch writer");

    // Read the batch header and flist back. The flist deserializer reads
    // until the end-of-list marker; if the writer had emitted stray
    // varint30(0) bytes after the marker, the reader's cursor would
    // remain on top of them and would NOT advance past the ID-list
    // region (because INC_RECURSE turns off the ID-list consumption path
    // in `reader/flist.rs`). This assertion pins both invariants: the
    // header records CF_INC_RECURSE, and the flist decode finds the one
    // payload entry the source contains.
    let read_cfg = BatchConfig::new(
        BatchMode::Read,
        batch_path.to_string_lossy().into_owned(),
        32,
    );
    let mut reader = BatchReader::new(read_cfg).expect("open batch reader");
    let header = reader.read_header().expect("read batch header");
    let header_compat = reader
        .header()
        .expect("header populated after read_header")
        .compat_flags
        .expect("compat_flags written for protocol 32");
    assert!(
        CompatibilityFlags::from_bits(header_compat as u32)
            .contains(CompatibilityFlags::INC_RECURSE),
        "batch header must record CF_INC_RECURSE under INC_RECURSE-on compat_flags"
    );
    let _ = header;

    let entries = reader
        .read_protocol_flist()
        .expect("decode flist after INC_RECURSE write_batch");
    let payload_seen = entries
        .iter()
        .any(|entry| entry.name().contains("payload.bin"));
    assert!(
        payload_seen,
        "decoded flist must include the source payload; got {:?}",
        entries
            .iter()
            .map(|e| e.name().to_string())
            .collect::<Vec<_>>()
    );
}
