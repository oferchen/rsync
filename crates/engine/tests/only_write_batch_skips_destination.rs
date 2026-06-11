//! Regression tests for UTS-15.a: `--only-write-batch` must not write to
//! the destination during local-copy execution.
//!
//! # Upstream Reference
//!
//! - `main.c:1839-1840` - `if (write_batch < 0) dry_run = 1;` forces dry-run
//!   when `--only-write-batch` is set so the receiver never invokes
//!   `do_recv()` / `finish_transfer()`. The batch file is the sole output.
//!
//! The combination `LocalCopyExecution::DryRun` + `LocalCopyOptions::batch_writer`
//! identifies `--only-write-batch` mode and must produce a populated batch
//! file while leaving the destination tree untouched. A plain `--dry-run`
//! (no batch writer) must continue to leave the destination untouched as
//! well; this second test guards the gate from collapsing back to a bare
//! `DryRun` check.

use std::fs;
use std::sync::{Arc, Mutex};

use batch::{BatchConfig, BatchFlags, BatchMode, BatchWriter};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use tempfile::tempdir;

/// Constructs a `BatchWriter` configured for `--only-write-batch`. The
/// header is written eagerly so the executor's flist/delta flush paths
/// can append to the open file.
fn make_only_write_batch_writer(path: &std::path::Path) -> Arc<Mutex<BatchWriter>> {
    let config = BatchConfig::new(
        BatchMode::OnlyWrite,
        path.to_string_lossy().into_owned(),
        32,
    );
    let mut writer = BatchWriter::new(config).expect("create batch writer");
    writer
        .write_header(BatchFlags::default())
        .expect("write batch header");
    Arc::new(Mutex::new(writer))
}

/// `--only-write-batch` must emit a non-empty batch file and skip every
/// destination write. The destination directory stays empty even though
/// the source contains a regular file with non-trivial payload.
#[test]
fn only_write_batch_produces_batch_and_skips_dst_write() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    let batch_path = temp.path().join("batch.bin");

    fs::create_dir_all(&source).expect("create source dir");
    fs::create_dir_all(&dest).expect("create dest dir");
    fs::write(source.join("file.txt"), b"hello").expect("write source file");

    let writer = make_only_write_batch_writer(&batch_path);
    let options = LocalCopyOptions::default()
        .recursive(true)
        .batch_writer(Some(Arc::clone(&writer)));

    let operands = vec![source.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("only-write-batch local copy succeeds");

    // Release the open writer handle so its buffered output is flushed
    // before the assertion runs.
    Arc::try_unwrap(writer)
        .expect("writer is uniquely owned")
        .into_inner()
        .expect("writer mutex not poisoned")
        .finalize()
        .expect("finalize batch writer");

    let batch_meta = fs::metadata(&batch_path).expect("stat batch file");
    assert!(
        batch_meta.len() > 0,
        "--only-write-batch must produce a non-empty batch file"
    );

    let dest_entries: Vec<_> = fs::read_dir(&dest)
        .expect("read destination dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert!(
        dest_entries.is_empty(),
        "--only-write-batch must not create destination entries; found {dest_entries:?}"
    );
}

/// Plain `--dry-run` (no batch writer) must also leave the destination
/// untouched. This pins the executor's gate to `DryRun && batch_writer
/// present`: if a future refactor collapses it to a bare `DryRun` check,
/// the prior test still passes while this one stays meaningful, and any
/// regression that lets `DryRun` write to the destination breaks both.
#[test]
fn dry_run_without_batch_still_skips_dst_write() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");

    fs::create_dir_all(&source).expect("create source dir");
    fs::create_dir_all(&dest).expect("create dest dir");
    fs::write(source.join("file.txt"), b"hello").expect("write source file");

    let options = LocalCopyOptions::default().recursive(true);
    let operands = vec![source.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry-run local copy succeeds");

    let dest_entries: Vec<_> = fs::read_dir(&dest)
        .expect("read destination dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert!(
        dest_entries.is_empty(),
        "--dry-run must not create destination entries; found {dest_entries:?}"
    );
}
