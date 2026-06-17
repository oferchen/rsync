//! Regression test for UTS batch-mode: `--only-write-batch` must capture
//! file payload so the matching `--read-batch` can reconstruct every entry.
//!
//! Prior to the fix, `--only-write-batch` ran the writer in
//! [`LocalCopyExecution::DryRun`] mode and the dry-run file-copy handler
//! never invoked `capture_batch_whole_file` / `finalize_batch_file_delta`.
//! The batch file therefore contained the flist segment, the post-flist ID
//! list terminators (or none under INC_RECURSE), the NDX_DONE phase markers
//! and the trailing stats - but no per-file token stream. The matching
//! `--read-batch` reconstructed an empty destination tree because the
//! NDX-replay loop walked straight through the NDX_DONE markers without
//! ever seeing a transferable file index.
//!
//! # Upstream Reference
//!
//! - `main.c:1839-1840` - `--only-write-batch` sets `dry_run = 1` so the
//!   destination tree is never touched. The batch-fd capture path still
//!   runs and writes the same byte stream upstream would tee to disk
//!   during a non-dry-run transfer.
//! - `receiver.c:recv_files()` - the `--read-batch` replay loop expects
//!   per-file NDX + iflags + sum_head + token stream + xfer checksum and
//!   reconstructs the destination from those tokens.

use std::fs;
use std::sync::{Arc, Mutex};

use batch::{BatchConfig, BatchFlags, BatchMode, BatchReader, BatchWriter};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use protocol::CompatibilityFlags;
use tempfile::tempdir;

/// Build a writer for `--only-write-batch` with `recurse` and
/// `preserve_links` set so the encoded flist matches a `-rl --only-write-batch`
/// invocation. The header is written eagerly so the executor can append
/// per-entry flist bytes immediately.
///
/// Mirrors the production compat_flags assembled by
/// `cli::frontend::execution::drive::workflow::run::local_batch_compat_flags`:
/// `SAFE_FILE_LIST | AVOID_XATTR_OPTIMIZATION | CHECKSUM_SEED_FIX |
/// INPLACE_PARTIAL_DIR | VARINT_FLIST_FLAGS`. Deliberately omits
/// `INC_RECURSE` so the flist segment encodes flat (matching upstream's
/// `--no-inc-recursive --write-batch` behaviour).
fn make_writer(path: &std::path::Path) -> Arc<Mutex<BatchWriter>> {
    let compat_flags = CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::INPLACE_PARTIAL_DIR
        | CompatibilityFlags::VARINT_FLIST_FLAGS;
    let config = BatchConfig::new(
        BatchMode::OnlyWrite,
        path.to_string_lossy().into_owned(),
        32,
    )
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

/// `--only-write-batch` must record per-file token data so the matching
/// `--read-batch` reconstructs every regular file with its source content.
///
/// Builds a source tree with three files of different sizes (including a
/// zero-byte file), drives the writer in `DryRun + batch_writer` mode (the
/// `--only-write-batch` regime), and then replays the batch into a fresh
/// destination. Every source file must materialise at the destination with
/// the exact byte contents from the source - any divergence indicates the
/// writer dropped the token stream.
#[test]
fn only_write_batch_replay_reconstructs_regular_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest_write = temp.path().join("dst_write");
    let dest_replay = temp.path().join("dst_replay");
    let batch_path = temp.path().join("batch.bin");

    fs::create_dir_all(&source).expect("create source dir");
    fs::create_dir_all(&dest_write).expect("create write-side dest");
    fs::create_dir_all(&dest_replay).expect("create replay-side dest");

    fs::write(source.join("empty"), b"").expect("write empty file");
    fs::write(source.join("small.txt"), b"hello, batch-mode").expect("write small file");
    // Large enough to exceed a single internal CHUNK_SIZE (32 KiB) so the
    // capture loop is exercised across multiple iterations.
    let big_payload: Vec<u8> = (0..96 * 1024).map(|i| (i % 251) as u8).collect();
    fs::write(source.join("big.bin"), &big_payload).expect("write big file");

    let writer = make_writer(&batch_path);
    let options = LocalCopyOptions::default()
        .recursive(true)
        .links(true)
        .batch_writer(Some(Arc::clone(&writer)));

    // Trailing slash on source so the contents land directly under dest
    // (mirrors `rsync -av --only-write-batch SRC/ DST`).
    let mut src_os = source.clone().into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest_write.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("only-write-batch dry-run succeeds");

    // Release the writer so the buffered output reaches disk before replay.
    Arc::try_unwrap(writer)
        .expect("writer uniquely owned")
        .into_inner()
        .expect("writer mutex not poisoned")
        .finalize()
        .expect("finalize batch writer");

    // Dry-run side must not have touched the destination.
    let dest_write_entries: Vec<_> = fs::read_dir(&dest_write)
        .expect("read dry-run dest")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert!(
        dest_write_entries.is_empty(),
        "--only-write-batch must not write to destination; found {dest_write_entries:?}"
    );

    let read_cfg = BatchConfig::new(
        BatchMode::Read,
        batch_path.to_string_lossy().into_owned(),
        32,
    );
    let mut reader = BatchReader::new(read_cfg.clone()).expect("open batch reader");
    reader.read_header().expect("read batch header");
    let entries = reader
        .read_protocol_flist()
        .expect("decode flist after --only-write-batch");
    let entry_names: Vec<String> = entries.iter().map(|e| e.name().to_owned()).collect();
    assert!(
        entry_names.iter().any(|n| n == "small.txt"),
        "flist must contain small.txt; got {entry_names:?}"
    );
    assert!(
        entry_names.iter().any(|n| n == "big.bin"),
        "flist must contain big.bin; got {entry_names:?}"
    );
    assert!(
        entry_names.iter().any(|n| n == "empty"),
        "flist must contain empty; got {entry_names:?}"
    );

    // Drop the reader so the file handle is closed before replay reopens it.
    drop(reader);

    let result = batch::replay::replay(&read_cfg, &dest_replay, 0).expect("replay succeeds");
    assert!(
        result.file_count >= 3,
        "replay must report at least 3 entries; got {}",
        result.file_count
    );

    let replay_empty = dest_replay.join("empty");
    assert!(
        replay_empty.exists(),
        "empty file must materialise at replay destination"
    );
    assert_eq!(
        fs::read(&replay_empty).expect("read empty file"),
        Vec::<u8>::new(),
        "empty file content must be empty"
    );

    let replay_small = dest_replay.join("small.txt");
    assert!(
        replay_small.exists(),
        "small.txt must materialise at replay destination"
    );
    assert_eq!(
        fs::read(&replay_small).expect("read small.txt"),
        b"hello, batch-mode",
        "small.txt content must round-trip through the batch file"
    );

    let replay_big = dest_replay.join("big.bin");
    assert!(
        replay_big.exists(),
        "big.bin must materialise at replay destination"
    );
    assert_eq!(
        fs::read(&replay_big).expect("read big.bin"),
        big_payload,
        "big.bin content must round-trip through the batch file"
    );
}

/// Same regime but with a symlink in the source tree. `--only-write-batch`
/// emits a symlink flist entry and `--read-batch` materialises the link at
/// the destination. Regression coverage for the upstream batch-mode test
/// which builds `nolf-symlink -> nolf` in `hands_setup()`.
#[cfg(unix)]
#[test]
fn only_write_batch_replay_reconstructs_symlinks() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    let batch_path = temp.path().join("batch.bin");

    fs::create_dir_all(&source).expect("create source dir");
    fs::create_dir_all(&dest).expect("create dest dir");
    fs::write(source.join("target.txt"), b"target body").expect("write target");
    std::os::unix::fs::symlink("target.txt", source.join("link")).expect("create symlink");

    let writer = make_writer(&batch_path);
    let options = LocalCopyOptions::default()
        .recursive(true)
        .links(true)
        .batch_writer(Some(Arc::clone(&writer)));

    let mut src_os = source.into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("only-write-batch dry-run succeeds");

    Arc::try_unwrap(writer)
        .expect("writer uniquely owned")
        .into_inner()
        .expect("writer mutex not poisoned")
        .finalize()
        .expect("finalize batch writer");

    let read_cfg = BatchConfig::new(
        BatchMode::Read,
        batch_path.to_string_lossy().into_owned(),
        32,
    );
    let replay_dst = temp.path().join("replay");
    fs::create_dir_all(&replay_dst).expect("create replay dst");
    let result = batch::replay::replay(&read_cfg, &replay_dst, 0).expect("replay succeeds");
    assert!(
        result.symlinks_created >= 1,
        "replay must create the symlink"
    );

    let link_path = replay_dst.join("link");
    let link_meta = std::fs::symlink_metadata(&link_path).expect("symlink stat");
    assert!(
        link_meta.file_type().is_symlink(),
        "link must materialise as a symlink"
    );
    let target = std::fs::read_link(&link_path).expect("read_link");
    assert_eq!(
        target.to_string_lossy(),
        "target.txt",
        "symlink target must round-trip through the batch file"
    );
}

/// Subdirectory regression: nested files must be reconstructed under their
/// parent directories with the original content. Pins the cross-segment
/// behaviour of the batch reader's flist sort + NDX replay against a layout
/// that mirrors the upstream `hands_setup()` `dir/subdir/...` shape.
#[test]
fn only_write_batch_replay_reconstructs_subdirectory_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    let batch_path = temp.path().join("batch.bin");

    fs::create_dir_all(source.join("dir/subdir")).expect("create nested dirs");
    fs::write(source.join("dir/text"), b"dir-text contents").expect("write dir/text");
    fs::write(
        source.join("dir/subdir/nested.txt"),
        b"nested-text contents",
    )
    .expect("write nested file");

    let writer = make_writer(&batch_path);
    let options = LocalCopyOptions::default()
        .recursive(true)
        .batch_writer(Some(Arc::clone(&writer)));

    let mut src_os = source.into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("only-write-batch dry-run succeeds");

    Arc::try_unwrap(writer)
        .expect("writer uniquely owned")
        .into_inner()
        .expect("writer mutex not poisoned")
        .finalize()
        .expect("finalize batch writer");

    let read_cfg = BatchConfig::new(
        BatchMode::Read,
        batch_path.to_string_lossy().into_owned(),
        32,
    );
    let replay_dst = temp.path().join("replay");
    fs::create_dir_all(&replay_dst).expect("create replay dst");
    batch::replay::replay(&read_cfg, &replay_dst, 0).expect("replay succeeds");

    let dir_text = replay_dst.join("dir/text");
    assert!(dir_text.exists(), "dir/text must materialise");
    assert_eq!(
        fs::read(&dir_text).expect("read dir/text"),
        b"dir-text contents"
    );

    let nested = replay_dst.join("dir/subdir/nested.txt");
    assert!(nested.exists(), "dir/subdir/nested.txt must materialise");
    assert_eq!(
        fs::read(&nested).expect("read nested file"),
        b"nested-text contents"
    );
}
