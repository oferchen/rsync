//! Touched-block accounting on the disk commit thread.
//!
//! The receiver credits the distinct 4 KiB logical blocks each write lands in,
//! at the write's absolute file offset, and never credits an in-place matched
//! block that is seeked past or a sparse hole. These pin that per message kind
//! so a pull prints the count upstream's receiver would report.
//!
//! upstream: fileio.c:212-243 (reset/track), fileio.c:168-180 (sparse spans),
//! fileio.c:251-252 (dense writes), fileio.c:294-313 skip_matched().

use std::fs;
use std::path::Path;

use crate::pipeline::messages::{BeginMessage, CommitResult, FileMessage};

use super::config::DiskCommitConfig;
use super::thread::spawn_disk_thread;

fn begin(file_path: &Path, target_size: u64, is_inplace: bool, append_offset: u64) -> BeginMessage {
    BeginMessage {
        file_path: file_path.to_path_buf(),
        target_size,
        file_entry_index: 0,
        checksum_verifier: None,
        is_device_target: false,
        is_inplace,
        append_offset,
        xattr_list: None,
        xattr_basis: None,
        file_entry: None,
    }
}

/// Streams `messages` for one file through a fresh disk thread and returns its
/// commit result.
fn commit(
    config: DiskCommitConfig,
    begin: BeginMessage,
    messages: Vec<FileMessage>,
) -> CommitResult {
    let h = spawn_disk_thread(config).unwrap();
    h.file_tx.send(FileMessage::Begin(Box::new(begin))).unwrap();
    for message in messages {
        h.file_tx.send(message).unwrap();
    }
    h.file_tx
        .send(FileMessage::Commit {
            expected_checksum: Default::default(),
        })
        .unwrap();
    let result = h.result_rx.recv().unwrap().unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
    result
}

fn filled(byte: u8, len: usize) -> Vec<u8> {
    vec![byte; len]
}

/// WHY: an in-place update seeks past blocks already at their offset and
/// credits only what it writes - upstream's write-touched-blocks TEST 2 relies
/// on this to report 10 instead of the whole file. Here blocks 1 (literal) and
/// 3 (a matched block copied to a new offset) are written; 0 and 2 are skipped.
#[test]
fn inplace_skipped_blocks_are_not_counted() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let dir = test_support::create_tempdir();
    let path = dir.path().join("inplace.dat");
    fs::write(&path, filled(b'A', 16384)).unwrap();

    let result = commit(
        DiskCommitConfig::default(),
        begin(&path, 16384, true, 0),
        vec![
            FileMessage::SkipMatched(filled(b'A', 4096)),
            FileMessage::Chunk(filled(b'L', 4096)),
            FileMessage::SkipMatched(filled(b'A', 4096)),
            FileMessage::MatchedChunk(filled(b'M', 4096)),
        ],
    );
    assert_eq!(result.touched_blocks_4k, 2);
}

/// WHY: a temp-file rebuild writes literal and matched data alike, so every
/// block it covers is touched - 8000 contiguous bytes span 2 blocks.
#[test]
fn temp_file_rebuild_counts_literal_and_matched_writes() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let dir = test_support::create_tempdir();
    let path = dir.path().join("rebuild.dat");

    let result = commit(
        DiskCommitConfig::default(),
        begin(&path, 8000, false, 0),
        vec![
            FileMessage::Chunk(filled(b'L', 3000)),
            FileMessage::MatchedChunk(filled(b'M', 5000)),
        ],
    );
    assert_eq!(result.touched_blocks_4k, 2);
}

/// WHY: under --sparse a zero run is seeked over as a hole and never credited;
/// upstream's TEST 5 (`data + 4 MiB hole + data`) reports 2 for this reason.
#[test]
fn sparse_hole_is_not_counted() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let dir = test_support::create_tempdir();
    let path = dir.path().join("sparse.dat");
    let config = DiskCommitConfig {
        use_sparse: true,
        ..DiskCommitConfig::default()
    };

    let mut data = filled(0xAA, 4096);
    data.extend(filled(0, 64 * 1024));
    data.extend(filled(0xBB, 4096));
    let len = data.len() as u64;
    let result = commit(
        config,
        begin(&path, len, false, 0),
        vec![FileMessage::Chunk(data)],
    );
    assert_eq!(result.touched_blocks_4k, 2);
}

/// WHY: an in-place sparse update consumes a matched block through the sparse
/// processor to punch its holes, but upstream seeks over its data
/// (`use_seek`), so it is not credited - only the literal block counts.
#[test]
fn sparse_inplace_skip_matched_is_not_counted() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let dir = test_support::create_tempdir();
    let path = dir.path().join("sparse_inplace.dat");
    fs::write(&path, filled(b'A', 8192)).unwrap();
    let config = DiskCommitConfig {
        use_sparse: true,
        ..DiskCommitConfig::default()
    };

    let result = commit(
        config,
        begin(&path, 8192, true, 0),
        vec![
            FileMessage::SkipMatched(filled(b'A', 4096)),
            FileMessage::Chunk(filled(b'L', 4096)),
        ],
    );
    assert_eq!(result.touched_blocks_4k, 1);
}

/// WHY: upstream credits blocks by absolute file offset, so an append that
/// resumes at 4000 and writes 200 bytes straddles blocks 0 and 1.
#[test]
fn append_counts_blocks_at_the_resume_offset() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let dir = test_support::create_tempdir();
    let path = dir.path().join("append.dat");
    fs::write(&path, filled(b'P', 4000)).unwrap();

    let result = commit(
        DiskCommitConfig::default(),
        begin(&path, 4200, true, 4000),
        vec![FileMessage::Chunk(filled(b'T', 200))],
    );
    assert_eq!(result.touched_blocks_4k, 2);
}

/// WHY: the coalesced single-literal path must count the same as the chunked
/// one; a 20,000-byte file spans 5 blocks.
#[test]
fn coalesced_whole_file_counts_every_block() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let dir = test_support::create_tempdir();
    let path = dir.path().join("whole.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();
    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(begin(&path, 20000, false, 0)),
            data: filled(b'W', 20000),
            expected_checksum: Default::default(),
        })
        .unwrap();
    let result = h.result_rx.recv().unwrap().unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
    assert_eq!(result.touched_blocks_4k, 5);
}
