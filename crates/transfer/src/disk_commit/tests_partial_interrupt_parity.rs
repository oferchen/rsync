// Parity validation: partial interrupt behavior must match upstream rsync.
//
// Upstream rsync's cleanup.c and receiver.c define the authoritative behavior
// for what happens to temp files when a transfer is interrupted:
//
//   1. PartialMode::None (default): temp file is deleted on any interrupt.
//   2. PartialMode::Partial (--partial): temp file is renamed to the final
//      destination, and modtime is stamped to epoch (0). The mtime=0 stamp
//      prevents --update from skipping the partial file on subsequent runs.
//      upstream: cleanup.c:130-135, cleanup.c:174-178
//   3. PartialMode::PartialDir (--partial-dir=DIR): temp file is moved to
//      DIR/filename. No mtime=0 stamp. upstream: cleanup.c:105-115
//
// This test module validates the full behavioral matrix: 3 modes x 3 interrupt
// types (Shutdown, Abort, channel Disconnect), plus edge cases for
// CleanupManager integration and zero-byte partial suppression.
//
// upstream references:
//   - cleanup.c:105-135  partial retention decision
//   - cleanup.c:174-178  mtime=0 stamp for plain --partial
//   - receiver.c:340-345 do_rename(partialptr, fname) on interrupt

use std::fs;

use crate::pipeline::messages::{BeginMessage, FileMessage};

use super::config::{DiskCommitConfig, PartialMode};
use super::thread::spawn_disk_thread;

/// Creates a BeginMessage for a test file at the given path.
fn begin_msg(file_path: std::path::PathBuf, target_size: u64) -> FileMessage {
    FileMessage::Begin(Box::new(BeginMessage {
        file_path,
        target_size,
        file_entry_index: 0,
        checksum_verifier: None,
        is_device_target: false,
        is_inplace: false,
        append_offset: 0,
        xattr_list: None,
    }))
}

/// Data pattern for partial content verification. Uses a repeating byte
/// sequence so partial content can be validated as a prefix.
fn test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

// ---------------------------------------------------------------------------
// Upstream parity: PartialMode::None deletes temp on all interrupt types
// ---------------------------------------------------------------------------

/// upstream: cleanup.c:55-80 - do_unlink() removes temp on interrupt
/// when keep_partial is false.
#[test]
fn parity_none_shutdown_deletes_temp() {
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("none_shutdown.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default());
    h.file_tx.send(begin_msg(dest.clone(), 1024)).unwrap();
    h.file_tx
        .send(FileMessage::Chunk(test_data(100)))
        .unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());
    assert!(
        !dest.exists(),
        "PartialMode::None must delete temp file on Shutdown"
    );

    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

/// upstream: cleanup.c:55-80 - do_unlink() removes temp on abort.
#[test]
fn parity_none_abort_deletes_temp() {
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("none_abort.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default());
    h.file_tx.send(begin_msg(dest.clone(), 1024)).unwrap();
    h.file_tx
        .send(FileMessage::Chunk(test_data(100)))
        .unwrap();
    h.file_tx
        .send(FileMessage::Abort {
            reason: "test".into(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());
    assert!(
        !dest.exists(),
        "PartialMode::None must delete temp file on Abort"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// upstream: cleanup.c:55-80 - do_unlink() removes temp on disconnect.
#[test]
fn parity_none_disconnect_deletes_temp() {
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("none_disconnect.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default());
    h.file_tx.send(begin_msg(dest.clone(), 1024)).unwrap();
    h.file_tx
        .send(FileMessage::Chunk(test_data(100)))
        .unwrap();

    drop(h.file_tx);
    h.join_handle.join().unwrap();

    assert!(
        !dest.exists(),
        "PartialMode::None must delete temp file on channel disconnect"
    );
}

// ---------------------------------------------------------------------------
// Upstream parity: PartialMode::Partial retains + stamps mtime=0
// ---------------------------------------------------------------------------

/// Helper: run a partial-mode transfer with the given interrupt type and
/// return the file_path and mtime of the retained file.
enum InterruptType {
    Shutdown,
    Abort,
    Disconnect,
}

fn run_partial_interrupt(
    dir: &tempfile::TempDir,
    name: &str,
    interrupt: InterruptType,
    data_size: usize,
) -> std::path::PathBuf {
    let dest = dir.path().join(name);
    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx.send(begin_msg(dest.clone(), 1024)).unwrap();
    let data = test_data(data_size);
    h.file_tx.send(FileMessage::Chunk(data)).unwrap();

    match interrupt {
        InterruptType::Shutdown => {
            h.file_tx.send(FileMessage::Shutdown).unwrap();
            let result = h.result_rx.recv().unwrap();
            assert!(result.is_err());
            drop(h.file_tx);
            h.join_handle.join().unwrap();
        }
        InterruptType::Abort => {
            h.file_tx
                .send(FileMessage::Abort {
                    reason: "test abort".into(),
                })
                .unwrap();
            let result = h.result_rx.recv().unwrap();
            assert!(result.is_err());
            h.file_tx.send(FileMessage::Shutdown).unwrap();
            h.join_handle.join().unwrap();
        }
        InterruptType::Disconnect => {
            drop(h.file_tx);
            h.join_handle.join().unwrap();
        }
    }
    dest
}

/// upstream: cleanup.c:130-135 - keep_partial && got_literal retains file.
/// cleanup.c:174-178 - stamps mtime=0 on retained partial.
#[test]
fn parity_partial_shutdown_retains_with_mtime_zero() {
    let dir = test_support::create_tempdir();
    let dest = run_partial_interrupt(&dir, "p_shutdown.dat", InterruptType::Shutdown, 200);

    assert!(dest.exists(), "--partial must retain file on Shutdown");
    assert_eq!(fs::read(&dest).unwrap(), test_data(200));

    let mtime = filetime::FileTime::from_last_modification_time(&fs::metadata(&dest).unwrap());
    assert_eq!(
        mtime,
        filetime::FileTime::from_unix_time(0, 0),
        "--partial must stamp mtime=0 on retained file (upstream cleanup.c:174-178)"
    );
}

/// upstream: cleanup.c:130-135/174-178 - same behavior on abort.
#[test]
fn parity_partial_abort_retains_with_mtime_zero() {
    let dir = test_support::create_tempdir();
    let dest = run_partial_interrupt(&dir, "p_abort.dat", InterruptType::Abort, 200);

    assert!(dest.exists(), "--partial must retain file on Abort");
    assert_eq!(fs::read(&dest).unwrap(), test_data(200));

    let mtime = filetime::FileTime::from_last_modification_time(&fs::metadata(&dest).unwrap());
    assert_eq!(
        mtime,
        filetime::FileTime::from_unix_time(0, 0),
        "--partial must stamp mtime=0 on retained file (abort path)"
    );
}

/// upstream: cleanup.c:130-135/174-178 - same behavior on disconnect.
#[test]
fn parity_partial_disconnect_retains_with_mtime_zero() {
    let dir = test_support::create_tempdir();
    let dest = run_partial_interrupt(&dir, "p_disconnect.dat", InterruptType::Disconnect, 200);

    assert!(dest.exists(), "--partial must retain file on Disconnect");
    assert_eq!(fs::read(&dest).unwrap(), test_data(200));

    let mtime = filetime::FileTime::from_last_modification_time(&fs::metadata(&dest).unwrap());
    assert_eq!(
        mtime,
        filetime::FileTime::from_unix_time(0, 0),
        "--partial must stamp mtime=0 on retained file (disconnect path)"
    );
}

/// Partial file content must be a valid prefix of the intended data.
/// This rules out corruption during the partial write path.
#[test]
fn parity_partial_content_is_valid_prefix() {
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("prefix_check.dat");
    let full_data = test_data(500);

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx.send(begin_msg(dest.clone(), 1024)).unwrap();
    // Send 200 of 500 bytes, then interrupt.
    h.file_tx
        .send(FileMessage::Chunk(full_data[..200].to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    let partial_content = fs::read(&dest).unwrap();
    assert_eq!(
        partial_content.len(),
        200,
        "partial file must contain exactly the written data"
    );
    assert_eq!(
        &partial_content[..],
        &full_data[..200],
        "partial content must be a valid prefix of the intended data"
    );

    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

// ---------------------------------------------------------------------------
// Upstream parity: PartialMode::PartialDir retains WITHOUT mtime=0
// ---------------------------------------------------------------------------

fn run_partial_dir_interrupt(
    dir: &tempfile::TempDir,
    name: &str,
    interrupt: InterruptType,
    data_size: usize,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let dest = dir.path().join(name);
    let partial_dir = dir.path().join(".rsync-partial-parity");
    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx.send(begin_msg(dest.clone(), 1024)).unwrap();
    let data = test_data(data_size);
    h.file_tx.send(FileMessage::Chunk(data)).unwrap();

    match interrupt {
        InterruptType::Shutdown => {
            h.file_tx.send(FileMessage::Shutdown).unwrap();
            let result = h.result_rx.recv().unwrap();
            assert!(result.is_err());
            drop(h.file_tx);
            h.join_handle.join().unwrap();
        }
        InterruptType::Abort => {
            h.file_tx
                .send(FileMessage::Abort {
                    reason: "test abort".into(),
                })
                .unwrap();
            let result = h.result_rx.recv().unwrap();
            assert!(result.is_err());
            h.file_tx.send(FileMessage::Shutdown).unwrap();
            h.join_handle.join().unwrap();
        }
        InterruptType::Disconnect => {
            drop(h.file_tx);
            h.join_handle.join().unwrap();
        }
    }
    let partial_path = partial_dir.join(name);
    (dest, partial_path)
}

/// upstream: cleanup.c:105-115 - handle_partial_dir() moves temp to
/// partial-dir. No mtime=0 stamp (only plain --partial does that).
#[test]
fn parity_partial_dir_shutdown_retains_without_mtime_zero() {
    let dir = test_support::create_tempdir();
    let (dest, partial_path) =
        run_partial_dir_interrupt(&dir, "pd_shutdown.dat", InterruptType::Shutdown, 200);

    assert!(
        !dest.exists(),
        "--partial-dir must NOT place file at final destination"
    );
    assert!(
        partial_path.exists(),
        "--partial-dir must retain file in partial directory on Shutdown"
    );
    assert_eq!(fs::read(&partial_path).unwrap(), test_data(200));

    // upstream: cleanup.c:174-178 only applies to plain --partial, not --partial-dir
    let mtime =
        filetime::FileTime::from_last_modification_time(&fs::metadata(&partial_path).unwrap());
    assert_ne!(
        mtime,
        filetime::FileTime::from_unix_time(0, 0),
        "--partial-dir must NOT stamp mtime=0 (upstream only does this for plain --partial)"
    );
}

/// upstream: cleanup.c:105-115 - same behavior on abort.
#[test]
fn parity_partial_dir_abort_retains_without_mtime_zero() {
    let dir = test_support::create_tempdir();
    let (dest, partial_path) =
        run_partial_dir_interrupt(&dir, "pd_abort.dat", InterruptType::Abort, 200);

    assert!(!dest.exists());
    assert!(
        partial_path.exists(),
        "--partial-dir must retain file in partial directory on Abort"
    );
    assert_eq!(fs::read(&partial_path).unwrap(), test_data(200));

    let mtime =
        filetime::FileTime::from_last_modification_time(&fs::metadata(&partial_path).unwrap());
    assert_ne!(
        mtime,
        filetime::FileTime::from_unix_time(0, 0),
        "--partial-dir must NOT stamp mtime=0 on abort"
    );
}

/// upstream: cleanup.c:105-115 - same behavior on disconnect.
#[test]
fn parity_partial_dir_disconnect_retains_without_mtime_zero() {
    let dir = test_support::create_tempdir();
    let (dest, partial_path) =
        run_partial_dir_interrupt(&dir, "pd_disconnect.dat", InterruptType::Disconnect, 200);

    assert!(!dest.exists());
    assert!(
        partial_path.exists(),
        "--partial-dir must retain file in partial directory on Disconnect"
    );
    assert_eq!(fs::read(&partial_path).unwrap(), test_data(200));

    let mtime =
        filetime::FileTime::from_last_modification_time(&fs::metadata(&partial_path).unwrap());
    assert_ne!(
        mtime,
        filetime::FileTime::from_unix_time(0, 0),
        "--partial-dir must NOT stamp mtime=0 on disconnect"
    );
}

// ---------------------------------------------------------------------------
// Upstream parity: zero-byte transfers suppress partial retention
// ---------------------------------------------------------------------------

/// upstream: cleanup.c:130 - keep_partial && got_literal guard.
/// If no literal data was received (bytes_written == 0), the partial file
/// is not retained even with --partial enabled.
#[test]
fn parity_zero_bytes_suppresses_partial_retention() {
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("zero_partial.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    // Begin but send no chunks before shutdown.
    h.file_tx.send(begin_msg(dest.clone(), 100)).unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    assert!(
        !dest.exists(),
        "zero-byte partial must NOT be retained (upstream got_literal guard)"
    );

    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

/// Same suppression for --partial-dir with zero bytes.
#[test]
fn parity_zero_bytes_suppresses_partial_dir_retention() {
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("zero_pd.dat");
    let partial_dir = dir.path().join(".rsync-partial-zero");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx.send(begin_msg(dest.clone(), 100)).unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    let partial_path = partial_dir.join("zero_pd.dat");
    assert!(
        !partial_path.exists(),
        "zero-byte partial must NOT be retained in partial-dir"
    );
    assert!(
        !dest.exists(),
        "destination must not exist for zero-byte partial"
    );

    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

// ---------------------------------------------------------------------------
// Upstream parity: CleanupManager unregisters temp on partial retention
// ---------------------------------------------------------------------------

/// When a partial file is retained via --partial (renamed to dest), it must
/// be unregistered from CleanupManager so a subsequent cleanup() call does
/// not delete the retained file. upstream: the temp path no longer exists
/// after rename, so cleanup must not try to remove it.
#[test]
fn parity_partial_retention_unregisters_from_cleanup_manager() {
    let manager = engine::CleanupManager::global();
    manager.reset_for_testing();

    let dir = test_support::create_tempdir();
    let dest = dir.path().join("cleanup_unreg.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx.send(begin_msg(dest.clone(), 1024)).unwrap();
    h.file_tx
        .send(FileMessage::Chunk(test_data(100)))
        .unwrap();

    // Wait for the chunk to be processed.
    let _recycled = h.buf_return_rx.recv().unwrap();

    // Before interrupt: temp file is registered.
    assert!(
        manager.temp_file_count() >= 1,
        "temp file must be registered before interrupt"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    drop(h.file_tx);
    h.join_handle.join().unwrap();

    // After partial retention: temp was renamed to dest, so the old temp
    // path is unregistered. Calling cleanup() must not delete the retained file.
    manager.cleanup();

    assert!(
        dest.exists(),
        "retained partial file must survive CleanupManager::cleanup()"
    );
    assert_eq!(fs::read(&dest).unwrap(), test_data(100));
}

/// When a partial file is retained via --partial-dir, the temp is renamed
/// into the partial directory. The original temp path must be unregistered.
#[test]
fn parity_partial_dir_retention_unregisters_from_cleanup_manager() {
    let manager = engine::CleanupManager::global();
    manager.reset_for_testing();

    let dir = test_support::create_tempdir();
    let dest = dir.path().join("cleanup_pd_unreg.dat");
    let partial_dir = dir.path().join(".rsync-partial-cm");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx.send(begin_msg(dest.clone(), 1024)).unwrap();
    h.file_tx
        .send(FileMessage::Chunk(test_data(100)))
        .unwrap();

    let _recycled = h.buf_return_rx.recv().unwrap();

    assert!(
        manager.temp_file_count() >= 1,
        "temp file must be registered before interrupt"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    drop(h.file_tx);
    h.join_handle.join().unwrap();

    // After partial-dir retention: cleanup must not touch the partial file.
    manager.cleanup();

    let partial_path = partial_dir.join("cleanup_pd_unreg.dat");
    assert!(
        partial_path.exists(),
        "retained partial-dir file must survive CleanupManager::cleanup()"
    );
    assert_eq!(fs::read(&partial_path).unwrap(), test_data(100));
    assert!(
        !dest.exists(),
        "destination must not exist when using partial-dir"
    );
}

// ---------------------------------------------------------------------------
// Upstream parity: multi-chunk partial captures all written data
// ---------------------------------------------------------------------------

/// When multiple chunks are written before interrupt, the retained partial
/// must contain the concatenation of all chunks - not just the last one.
/// upstream: receiver.c writes each token sequentially to the temp file.
#[test]
fn parity_multi_chunk_partial_captures_all_data() {
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("multi_chunk_parity.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx.send(begin_msg(dest.clone(), 1024)).unwrap();

    // Send three chunks with distinct content.
    h.file_tx
        .send(FileMessage::Chunk(b"AAAA".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"BBBB".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"CCCC".to_vec()))
        .unwrap();

    h.file_tx
        .send(FileMessage::Abort {
            reason: "mid-transfer interrupt".into(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    assert!(dest.exists(), "partial must be retained after abort");
    assert_eq!(
        fs::read(&dest).unwrap(),
        b"AAAABBBBCCCC",
        "partial must contain concatenation of all written chunks"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Same multi-chunk test for --partial-dir.
#[test]
fn parity_multi_chunk_partial_dir_captures_all_data() {
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("multi_chunk_pd.dat");
    let partial_dir = dir.path().join(".rsync-partial-multi");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx.send(begin_msg(dest.clone(), 1024)).unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"1111".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"2222".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"3333".to_vec()))
        .unwrap();

    h.file_tx
        .send(FileMessage::Abort {
            reason: "mid-transfer interrupt".into(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    let partial_path = partial_dir.join("multi_chunk_pd.dat");
    assert!(
        partial_path.exists(),
        "partial-dir must retain file after abort"
    );
    assert!(!dest.exists());
    assert_eq!(
        fs::read(&partial_path).unwrap(),
        b"111122223333",
        "partial-dir file must contain all written chunks"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}
