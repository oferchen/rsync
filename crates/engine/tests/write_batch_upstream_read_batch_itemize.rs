//! Cross-implementation oracle for `--write-batch` iflags fidelity: an
//! upstream rsync `--read-batch` replay of an oc-produced batch must
//! itemize a fresh file as created and count it in `--stats`.
//!
//! Needs the rsync 3.5.2 oracle, resolved by
//! `test_support::require_upstream_rsync` (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, or
//! the install `tools/ci/run_interop.sh build-only` makes). Without it the
//! test prints a skip line and returns; under `OC_RSYNC_UPSTREAM_COMPAT=1`,
//! as the interop job's oracle step runs it, a missing oracle fails.
//!
//! The always-run companion `write_batch_new_file_iflags.rs` pins the same
//! defect at the byte level without needing upstream; this test additionally
//! confirms the itemize string and `--stats` line upstream actually prints.
//!
//! Drives the real `oc-rsync` binary end to end (not `engine::local_copy`
//! directly): the batch trailer - final "goodbye" bytes and stats block -
//! is written by the `core` orchestration layer, not by the engine's local
//! copy executor alone, so a batch built by calling `engine::local_copy` in
//! isolation is missing it and an upstream `--read-batch` reader blocks
//! forever waiting for bytes that will never arrive.
//!
//! # Upstream Reference
//!
//! - `generator.c:583-584 itemize()`, `sender.c:469 write_ndx_and_attrs()`,
//!   `sender.c:588,626` (`stats.created_files`), `log.c:739-781` (itemize
//!   string rendering: `+` fill when `ITEM_IS_NEW` is set).

use std::fs;
use std::process::Command;
use std::time::Duration;

use tempfile::tempdir;
use test_support::{Deadlined, UpstreamVersion, run_deadlined};

/// Wall-clock budget for the upstream `--read-batch` subprocess. Generous
/// for a single small file, but still bounded - see `crates/test-support/
/// src/deadline.rs` for why an unbounded wait on a child (or a surviving
/// descendant of it) is never safe in a test harness.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn upstream_read_batch_itemizes_new_file_as_created() {
    let Some(oracle) = test_support::require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let upstream = oracle.binary();
    let oc_rsync = test_support::oc_rsync_bin();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest_write = temp.path().join("dst_write");
    let dest_read = temp.path().join("dst_read");
    let batch_path = temp.path().join("oc.batch");

    fs::create_dir_all(&source).expect("create source dir");
    fs::create_dir_all(&dest_write).expect("create write-side dest");
    fs::create_dir_all(&dest_read).expect("create read-side dest");
    fs::write(source.join("file.txt"), b"hello world\n").expect("write source file");

    // `oc-rsync --write-batch=B -a <src> <dst>` - the exact reproduction the
    // ledger describes.
    let mut src_arg = source.clone().into_os_string();
    src_arg.push("/");
    let mut write_cmd = Command::new(&oc_rsync);
    write_cmd
        .arg("-a")
        .arg(format!("--write-batch={}", batch_path.display()))
        .arg(&src_arg)
        .arg(&dest_write);
    let write_outcome =
        run_deadlined(&mut write_cmd, UPSTREAM_TIMEOUT).expect("spawn oc-rsync --write-batch");
    let Deadlined::Finished { status, stderr, .. } = write_outcome else {
        panic!("oc-rsync --write-batch timed out after {UPSTREAM_TIMEOUT:?}");
    };
    assert!(
        status.success(),
        "oc-rsync --write-batch failed: {}",
        String::from_utf8_lossy(&stderr)
    );

    // upstream `--read-batch` forks a local child that plays the sender
    // role reading from the batch file; `run_deadlined` sets stdin to null
    // for both and bounds the whole run, so neither can block on a stdin
    // read that never arrives nor hang the harness past the budget.
    let mut read_cmd = Command::new(upstream);
    read_cmd
        .arg("-avi")
        .arg(format!("--read-batch={}", batch_path.display()))
        .arg("--stats")
        .arg(format!("{}/", dest_read.display()));
    let read_outcome =
        run_deadlined(&mut read_cmd, UPSTREAM_TIMEOUT).expect("spawn upstream --read-batch");
    let Deadlined::Finished {
        status,
        stdout,
        stderr,
    } = read_outcome
    else {
        panic!("upstream --read-batch timed out after {UPSTREAM_TIMEOUT:?}");
    };
    assert!(
        status.success(),
        "upstream --read-batch failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    let stdout = String::from_utf8_lossy(&stdout);

    assert!(
        stdout.contains(">f+++++++++ file.txt"),
        "upstream must itemize the fresh file as created (`>f+++++++++`), not as \
         unchanged; got itemize output:\n{stdout}"
    );
    assert!(
        stdout.contains("Number of created files: 1"),
        "upstream --stats must count the fresh file as created; got:\n{stdout}"
    );

    assert_eq!(
        fs::read(dest_read.join("file.txt")).expect("read replayed file"),
        b"hello world\n",
        "replayed content must match the source"
    );
}
