//! Tests for the disk commit thread.

use std::fs;

use crate::pipeline::messages::{BeginMessage, FileMessage};
use crate::pipeline::spsc::TryRecvError;

use super::config::{DiskCommitConfig, PartialMode};
use super::thread::spawn_disk_thread;

#[test]
fn default_config() {
    let config = DiskCommitConfig::default();
    assert!(!config.do_fsync);
    assert_eq!(
        config.channel_capacity,
        super::config::DEFAULT_CHANNEL_CAPACITY
    );
    assert_eq!(config.io_uring_policy, fast_io::IoUringPolicy::Auto);
}

#[test]
fn config_io_uring_policy_disabled() {
    let config = DiskCommitConfig {
        io_uring_policy: fast_io::IoUringPolicy::Disabled,
        ..DiskCommitConfig::default()
    };
    assert_eq!(config.io_uring_policy, fast_io::IoUringPolicy::Disabled);
}

#[test]
fn config_io_uring_depth_default_is_none() {
    let config = DiskCommitConfig::default();
    assert_eq!(config.io_uring_depth, None);
}

#[test]
fn config_io_uring_depth_override() {
    let config = DiskCommitConfig {
        io_uring_depth: Some(256),
        ..DiskCommitConfig::default()
    };
    assert_eq!(config.io_uring_depth, Some(256));
}

#[test]
fn config_iocp_policy_disabled() {
    let config = DiskCommitConfig {
        iocp_policy: fast_io::IocpPolicy::Disabled,
        ..DiskCommitConfig::default()
    };
    assert_eq!(config.iocp_policy, fast_io::IocpPolicy::Disabled);
}

#[test]
fn spawn_with_iocp_disabled() {
    let config = DiskCommitConfig {
        iocp_policy: fast_io::IocpPolicy::Disabled,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn spawn_with_io_uring_disabled() {
    let config = DiskCommitConfig {
        io_uring_policy: fast_io::IoUringPolicy::Disabled,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn write_file_with_io_uring_auto_policy() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("io_uring_auto.dat");

    let config = DiskCommitConfig {
        io_uring_policy: fast_io::IoUringPolicy::Auto,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 5,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"uring".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 5);
    assert_eq!(fs::read(&file_path).unwrap(), b"uring");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn channel_capacity_default_passthrough() {
    let config = DiskCommitConfig::default();
    assert_eq!(config.effective_channel_capacity(), 128);
}

#[test]
fn channel_capacity_clamp_below_min() {
    let config = DiskCommitConfig {
        channel_capacity: 1,
        ..DiskCommitConfig::default()
    };
    assert_eq!(config.effective_channel_capacity(), 8);
}

#[test]
fn channel_capacity_clamp_above_max() {
    let config = DiskCommitConfig {
        channel_capacity: 10_000,
        ..DiskCommitConfig::default()
    };
    assert_eq!(config.effective_channel_capacity(), 4096);
}

#[test]
fn channel_capacity_within_range() {
    let config = DiskCommitConfig {
        channel_capacity: 256,
        ..DiskCommitConfig::default()
    };
    assert_eq!(config.effective_channel_capacity(), 256);
}

#[test]
fn channel_capacity_boundary_min() {
    let config = DiskCommitConfig {
        channel_capacity: 8,
        ..DiskCommitConfig::default()
    };
    assert_eq!(config.effective_channel_capacity(), 8);
}

#[test]
fn channel_capacity_boundary_max() {
    let config = DiskCommitConfig {
        channel_capacity: 4096,
        ..DiskCommitConfig::default()
    };
    assert_eq!(config.effective_channel_capacity(), 4096);
}

#[test]
fn spawn_with_custom_capacity() {
    let config = DiskCommitConfig {
        channel_capacity: 16,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn spawn_and_shutdown() {
    let h = spawn_disk_thread(DiskCommitConfig::default());
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn write_and_commit_file() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("output.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default());

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 1024,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"hello world".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 11);
    assert_eq!(result.file_entry_index, 0);
    assert!(result.metadata_error.is_none());

    assert_eq!(fs::read(&file_path).unwrap(), b"hello world");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn abort_cleans_up_temp_file() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("aborted.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default());

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 1,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"partial".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Abort {
            reason: "test abort".into(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    assert!(!file_path.exists());

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn multiple_files_sequential() {
    let dir = test_support::create_tempdir();
    let h = spawn_disk_thread(DiskCommitConfig::default());

    for i in 0..3 {
        let path = dir.path().join(format!("file{i}.dat"));
        let data = format!("content-{i}");

        h.file_tx
            .send(FileMessage::Begin(Box::new(BeginMessage {
                file_path: path.clone(),
                target_size: data.len() as u64,
                file_entry_index: i,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            })))
            .unwrap();

        h.file_tx
            .send(FileMessage::Chunk(data.into_bytes()))
            .unwrap();
        h.file_tx.send(FileMessage::Commit).unwrap();

        let result = h.result_rx.recv().unwrap().unwrap();
        assert_eq!(result.file_entry_index, i);
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("content-{i}"));
    }

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn channel_disconnect_stops_thread() {
    let h = spawn_disk_thread(DiskCommitConfig::default());
    drop(h.file_tx);

    h.join_handle.join().unwrap();

    assert!(matches!(
        h.result_rx.try_recv(),
        Err(TryRecvError::Disconnected)
    ));
}

#[test]
fn multi_chunk_file() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("multi_chunk.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default());

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 300,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx.send(FileMessage::Chunk(b"aaa".to_vec())).unwrap();
    h.file_tx.send(FileMessage::Chunk(b"bbb".to_vec())).unwrap();
    h.file_tx.send(FileMessage::Chunk(b"ccc".to_vec())).unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 9);
    assert_eq!(fs::read(&file_path).unwrap(), b"aaabbbccc");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn buffer_recycling() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("recycle.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default());

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"hello".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b" world".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 11);

    // Disk thread should have returned 2 buffers for recycling.
    let recycled1 = h.buf_return_rx.recv().unwrap();
    let recycled2 = h.buf_return_rx.recv().unwrap();
    assert!(recycled1.capacity() >= 5);
    assert!(recycled2.capacity() >= 6);

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn whole_file_coalesced() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("whole.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default());

    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 9,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"whole dat".to_vec(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 9);
    assert_eq!(result.file_entry_index, 0);
    assert!(result.metadata_error.is_none());
    assert_eq!(fs::read(&file_path).unwrap(), b"whole dat");

    // Buffer should be returned for recycling.
    let recycled = h.buf_return_rx.recv().unwrap();
    assert!(recycled.capacity() >= 9);

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Verifies the io_uring rename dispatch in `commit_file`: the temp-file
/// rename must succeed regardless of whether io_uring handles it or the
/// `std::fs::rename` fallback does. This exercises the same path as
/// production remote transfers.
#[test]
fn commit_file_rename_via_io_uring_or_fallback() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("iouring_rename_commit.dat");

    let config = DiskCommitConfig {
        io_uring_policy: fast_io::IoUringPolicy::Auto,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 14,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"rename content".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 14);

    assert!(file_path.exists());
    assert_eq!(fs::read(&file_path).unwrap(), b"rename content");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Verifies the io_uring rename works when replacing an existing destination.
#[test]
fn commit_file_rename_replaces_existing_via_io_uring_or_fallback() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("iouring_replace.dat");

    fs::write(&file_path, b"old content").unwrap();

    let config = DiskCommitConfig {
        io_uring_policy: fast_io::IoUringPolicy::Auto,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 11,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"new content".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 11);
    assert_eq!(fs::read(&file_path).unwrap(), b"new content");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Verifies the whole-file path also uses the io_uring rename dispatch.
#[test]
fn whole_file_commit_via_io_uring_or_fallback() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("whole_iouring.dat");

    let config = DiskCommitConfig {
        io_uring_policy: fast_io::IoUringPolicy::Auto,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 13,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"whole iouring".to_vec(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 13);
    assert_eq!(fs::read(&file_path).unwrap(), b"whole iouring");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

// --- PIR-2: Partial file retention tests ---

#[test]
fn partial_mode_default_is_none() {
    let config = DiskCommitConfig::default();
    assert_eq!(config.partial_mode, PartialMode::None);
}

#[test]
fn partial_mode_partial_retains_file_on_shutdown() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("partial_shutdown.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"partial data".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    // With PartialMode::Partial, the temp file should be renamed to dest.
    assert!(
        file_path.exists(),
        "partial file must be retained at destination on shutdown"
    );
    assert_eq!(fs::read(&file_path).unwrap(), b"partial data");

    // Drop the sender so the disk thread's recv() returns Err and exits.
    // The Shutdown above was consumed by process_file, not disk_thread_main.
    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

#[test]
fn partial_mode_none_deletes_file_on_shutdown() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("no_partial_shutdown.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::None,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"will be deleted".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    // With PartialMode::None, the temp file should be deleted.
    assert!(
        !file_path.exists(),
        "temp file must be deleted when partial mode is None"
    );

    // Drop the sender so the disk thread's recv() returns Err and exits.
    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

#[test]
fn partial_mode_partial_retains_file_on_abort() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("partial_abort.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"abort data".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Abort {
            reason: "test abort".into(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    // With PartialMode::Partial, the temp file should be renamed to dest.
    assert!(
        file_path.exists(),
        "partial file must be retained at destination on abort"
    );
    assert_eq!(fs::read(&file_path).unwrap(), b"abort data");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn partial_mode_partial_dir_retains_file_in_directory() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("partial_dir_test.dat");
    let partial_dir = dir.path().join(".rsync-partial");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"partial dir data".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    // The temp file should be moved to partial-dir/filename.
    let partial_path = partial_dir.join("partial_dir_test.dat");
    assert!(
        partial_path.exists(),
        "partial file must be retained in partial directory"
    );
    assert_eq!(fs::read(&partial_path).unwrap(), b"partial dir data");
    // The original dest path should not exist.
    assert!(
        !file_path.exists(),
        "destination file must not exist when using partial-dir"
    );

    // Drop the sender so the disk thread's recv() returns Err and exits.
    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

#[test]
fn partial_mode_no_retention_without_data() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("no_data_partial.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    // Begin but send no chunks - upstream's got_literal would be false.
    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    // No data was written, so no partial retention.
    assert!(
        !file_path.exists(),
        "no partial retention without literal data"
    );

    // Drop the sender so the disk thread's recv() returns Err and exits.
    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

#[test]
fn partial_mode_channel_disconnect_retains_file() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("disconnect_partial.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"disconnect data".to_vec()))
        .unwrap();

    // Drop the sender to simulate channel disconnect.
    drop(h.file_tx);

    // The disk thread should exit and retain the partial file.
    h.join_handle.join().unwrap();

    assert!(
        file_path.exists(),
        "partial file must be retained on channel disconnect"
    );
    assert_eq!(fs::read(&file_path).unwrap(), b"disconnect data");
}

#[test]
fn partial_mode_relative_partial_dir() {
    let dir = test_support::create_tempdir();
    let dest_dir = dir.path().join("dest");
    fs::create_dir(&dest_dir).unwrap();
    let file_path = dest_dir.join("relative_partial.dat");

    // Relative partial-dir: resolved relative to the file's parent directory.
    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(std::path::PathBuf::from(".rsync-partial")),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config);

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"relative partial".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    let partial_path = dest_dir.join(".rsync-partial").join("relative_partial.dat");
    assert!(
        partial_path.exists(),
        "partial file must be retained in relative partial directory"
    );
    assert_eq!(fs::read(&partial_path).unwrap(), b"relative partial");

    // Drop the sender so the disk thread's recv() returns Err and exits.
    drop(h.file_tx);
    h.join_handle.join().unwrap();
}
