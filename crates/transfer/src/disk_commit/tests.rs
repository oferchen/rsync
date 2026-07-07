//! Tests for the disk commit thread.

use std::ffi::OsString;
use std::fs;

use crate::pipeline::messages::{BeginMessage, FileMessage};
use crate::pipeline::spsc::TryRecvError;

use super::config::{BackupConfig, DELAY_UPDATES_PARTIAL_DIR, DiskCommitConfig, PartialMode};
use super::process::{DelayedUpdateEntry, delay_updates_staging_path, handle_delayed_updates};
use super::thread::spawn_disk_thread;

#[test]
fn default_config() {
    let config = DiskCommitConfig::default();
    assert!(!config.do_fsync);
    assert!(!config.delay_updates);
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
    let h = spawn_disk_thread(config).unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// EDG-PANIC.5 regression: `spawn_disk_thread` returns `io::Result` so a
/// `thread::Builder::spawn` failure (typically `EAGAIN`/`RLIMIT_NPROC`)
/// propagates to the receiver instead of aborting the process. The OS does
/// not let a unit test induce a spawn failure portably, so the test locks
/// in the signature shape: the call returns a typed result and the success
/// path yields a fully-formed handle.
#[test]
fn spawn_returns_io_result_handle() {
    let config = DiskCommitConfig::default();
    let result: std::io::Result<super::DiskThreadHandle> = spawn_disk_thread(config);
    let h = result.expect("spawn must succeed on a healthy runtime");
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn spawn_with_io_uring_disabled() {
    let config = DiskCommitConfig {
        io_uring_policy: fast_io::IoUringPolicy::Disabled,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();
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
    let h = spawn_disk_thread(config).unwrap();

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
    let h = spawn_disk_thread(config).unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn spawn_and_shutdown() {
    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn write_and_commit_file() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("output.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();

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

    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();

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
    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();

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
    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();
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

    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();

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

    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();

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

    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();

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
    let h = spawn_disk_thread(config).unwrap();

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
    let h = spawn_disk_thread(config).unwrap();

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

/// Verifies that with `delay_updates` enabled, the disk thread stages files
/// to `.~tmp~/<filename>` instead of renaming to the final destination.
/// The final rename is deferred to the receiver's phase 2 sweep.
///
/// upstream: receiver.c:906-929 - delay_updates stages to partial dir
#[test]
fn delay_updates_stages_to_partial_dir() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("delayed.dat");

    let config = DiskCommitConfig {
        delay_updates: true,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 13,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"delayed write".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 13);

    // File should NOT be at the final path yet.
    assert!(
        !file_path.exists(),
        "final path should not exist when delay_updates is active"
    );

    // File should be staged in the .~tmp~ partial dir.
    let staging_path = dir.path().join(".~tmp~").join("delayed.dat");
    assert!(
        staging_path.exists(),
        "file should be staged at .~tmp~/<basename>"
    );
    assert_eq!(fs::read(&staging_path).unwrap(), b"delayed write");

    // CommitResult should report the delayed path.
    assert_eq!(
        result.delayed_path,
        Some(staging_path.clone()),
        "CommitResult must report the staging path"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();

    // Cleanup: remove .~tmp~ dir
    let _ = fs::remove_file(&staging_path);
    let _ = fs::remove_dir(dir.path().join(".~tmp~"));
}

/// Verifies that the coalesced WholeFile path also stages to `.~tmp~`
/// when `delay_updates` is enabled.
#[test]
fn delay_updates_whole_file_stages_to_partial_dir() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("whole_delayed.dat");

    let config = DiskCommitConfig {
        delay_updates: true,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 14,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"whole delayed!".to_vec(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 14);
    assert!(
        !file_path.exists(),
        "final path should not exist with delay_updates"
    );

    let staging_path = dir.path().join(".~tmp~").join("whole_delayed.dat");
    assert!(staging_path.exists(), "file staged at .~tmp~/<basename>");
    assert_eq!(fs::read(&staging_path).unwrap(), b"whole delayed!");
    assert_eq!(result.delayed_path, Some(staging_path));

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
    let h = spawn_disk_thread(config).unwrap();

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
    let h = spawn_disk_thread(config).unwrap();

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
    let h = spawn_disk_thread(config).unwrap();

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
    let h = spawn_disk_thread(config).unwrap();

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
    let h = spawn_disk_thread(config).unwrap();

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
    let h = spawn_disk_thread(config).unwrap();

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
    let h = spawn_disk_thread(config).unwrap();

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
    let h = spawn_disk_thread(config).unwrap();

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

/// Verifies that the disk commit thread registers temp files with the global
/// `CleanupManager` during transfer, and that calling `cleanup()` removes
/// orphaned temp files from disk. This simulates the signal handler path
/// where a SIGINT/SIGTERM fires mid-transfer: the handler calls `cleanup()`
/// before exit, removing temp files that never reached the commit+rename step.
#[test]
fn cleanup_manager_removes_orphaned_temp_files_on_simulated_signal() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let manager = engine::CleanupManager::global();
    manager.reset_for_testing();

    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("orphan_cleanup.dat");

    let config = DiskCommitConfig::default();
    let h = spawn_disk_thread(config).unwrap();

    // Send Begin (creates temp file and registers it with CleanupManager)
    // and a Chunk so that data is written.
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
        .send(FileMessage::Chunk(b"orphan data".to_vec()))
        .unwrap();

    // Wait for the buffer to be recycled - this proves the disk thread has
    // processed both Begin (creating + registering the temp file) and Chunk.
    let _recycled = h.buf_return_rx.recv().unwrap();

    // The temp file is now on disk and registered with CleanupManager.
    assert!(
        manager.temp_file_count() >= 1,
        "temp file must be registered with CleanupManager during active transfer"
    );

    // Find the orphaned temp file on disk. It follows the `.filename.XXXXXX`
    // naming convention in the same directory as the destination file.
    let parent = file_path.parent().unwrap();
    let orphans: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.starts_with(".orphan_cleanup.dat.")
        })
        .collect();
    assert!(
        !orphans.is_empty(),
        "temp file with .filename.XXXXXX pattern must exist on disk"
    );

    // Simulate SIGINT cleanup: call cleanup() directly, bypassing normal commit.
    manager.cleanup();

    // The orphaned temp file should be removed from disk.
    let remaining: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.starts_with(".orphan_cleanup.dat.")
        })
        .collect();
    assert!(
        remaining.is_empty(),
        "orphaned temp file must be removed after CleanupManager::cleanup()"
    );

    // The final destination should not exist (never committed).
    assert!(
        !file_path.exists(),
        "destination file must not exist - transfer was never committed"
    );

    // Drop sender to unblock the disk thread so it can exit.
    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

/// Verifies that successfully committed files are unregistered from
/// `CleanupManager`, so a subsequent `cleanup()` call does not remove them.
/// This is the counterpart to the orphan test: committed files must survive.
#[test]
fn cleanup_manager_unregisters_on_successful_commit() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let manager = engine::CleanupManager::global();
    manager.reset_for_testing();

    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("committed_file.dat");

    let config = DiskCommitConfig::default();
    let h = spawn_disk_thread(config).unwrap();

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 13,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"committed dat".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 13);

    // After successful commit, the temp file is renamed to the final path
    // and unregistered from CleanupManager.
    assert_eq!(
        manager.temp_file_count(),
        0,
        "committed file must be unregistered from CleanupManager"
    );

    // Calling cleanup() should have no effect on the committed file.
    manager.cleanup();
    assert!(
        file_path.exists(),
        "committed file must survive CleanupManager::cleanup()"
    );
    assert_eq!(fs::read(&file_path).unwrap(), b"committed dat");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Verifies cleanup across multiple in-flight files: one committed, one
/// orphaned. Only the orphan should be removed by `cleanup()`.
#[test]
fn cleanup_manager_mixed_committed_and_orphaned_files() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let manager = engine::CleanupManager::global();
    manager.reset_for_testing();

    let dir = test_support::create_tempdir();
    let committed_path = dir.path().join("committed.dat");
    let orphan_path = dir.path().join("orphan.dat");

    let config = DiskCommitConfig::default();
    let h = spawn_disk_thread(config).unwrap();

    // File 1: commit successfully.
    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: committed_path.clone(),
            target_size: 4,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"good".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();
    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 4);

    // File 2: begin + chunk but no commit (will be orphaned).
    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: orphan_path.clone(),
            target_size: 1024,
            file_entry_index: 1,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"orphan".to_vec()))
        .unwrap();

    // Wait for the orphan chunk to be processed.
    let _recycled = h.buf_return_rx.recv().unwrap(); // committed file's buffer
    let _recycled = h.buf_return_rx.recv().unwrap(); // orphan file's buffer

    // At this point file 1 is committed (unregistered), file 2 is orphaned
    // (still registered). CleanupManager should have at least 1 entry.
    assert!(
        manager.temp_file_count() >= 1,
        "orphaned temp file must be registered"
    );

    // Verify the orphan temp file exists on disk.
    let parent = dir.path();
    let orphan_temps: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.starts_with(".orphan.dat.")
        })
        .collect();
    assert!(
        !orphan_temps.is_empty(),
        "orphan temp file must exist on disk before cleanup"
    );

    // Simulate signal: cleanup removes orphaned temp files.
    manager.cleanup();

    // Orphan temp file should be gone.
    let remaining: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.starts_with(".orphan.dat.")
        })
        .collect();
    assert!(
        remaining.is_empty(),
        "orphaned temp file must be removed after cleanup()"
    );

    // Committed file must survive.
    assert!(
        committed_path.exists(),
        "committed file must survive cleanup()"
    );
    assert_eq!(fs::read(&committed_path).unwrap(), b"good");

    // Orphan destination must not exist.
    assert!(
        !orphan_path.exists(),
        "orphan destination must not exist - never committed"
    );

    // Clean up: drop sender so disk thread exits.
    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

/// Verifies that `WholeFile` (coalesced single-chunk path) also unregisters
/// from `CleanupManager` after commit, matching the chunked path behavior.
#[test]
fn cleanup_manager_whole_file_unregisters_on_commit() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let manager = engine::CleanupManager::global();
    manager.reset_for_testing();

    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("whole_cleanup.dat");

    let config = DiskCommitConfig::default();
    let h = spawn_disk_thread(config).unwrap();

    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 10,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"whole file".to_vec(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 10);

    // After WholeFile commit, the temp file is unregistered.
    assert_eq!(
        manager.temp_file_count(),
        0,
        "WholeFile path must unregister from CleanupManager after commit"
    );

    // Cleanup must not affect the committed file.
    manager.cleanup();
    assert!(file_path.exists(), "committed file must survive cleanup()");
    assert_eq!(fs::read(&file_path).unwrap(), b"whole file");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Verifies that inplace writes (which skip temp+rename) do not register
/// with `CleanupManager`. Inplace writes go directly to the destination,
/// so there is no orphan temp file to clean up.
#[test]
fn cleanup_manager_inplace_skips_registration() {
    let _registry_lock = test_support::cleanup_registry_test_guard();
    let manager = engine::CleanupManager::global();
    manager.reset_for_testing();

    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("inplace.dat");

    let config = DiskCommitConfig::default();
    let h = spawn_disk_thread(config).unwrap();

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 7,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: true,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"inplace".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 7);

    // Inplace writes go directly to the destination - no temp file registration.
    assert_eq!(
        manager.temp_file_count(),
        0,
        "inplace writes must not register temp files with CleanupManager"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// #511 (data-loss regression): a mid-transfer error on an `--inplace`
/// transfer must LEAVE the pre-existing destination in place, never delete
/// it. The inplace path writes directly to the real destination (no
/// temp+rename), so the cleanup guard wraps the live dest. A prior
/// implementation dropped that guard on the error path, which unlinked the
/// user's destination - non-adversarial data loss from a network drop.
///
/// WHY it matters: upstream `receiver.c:1054` gates the destination unlink
/// on `!one_inplace`, so an interrupted inplace transfer never removes the
/// dest; the partial write stays (the documented `--inplace` risk the user
/// opted into). This test fails if the guard ever unlinks the inplace dest
/// again.
#[test]
fn inplace_abort_does_not_delete_existing_destination() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("inplace_abort_preexisting.dat");

    // Pre-existing destination the user must not lose.
    fs::write(&file_path, b"the user's existing data").unwrap();
    assert!(file_path.exists());

    let config = DiskCommitConfig::default();
    let h = spawn_disk_thread(config).unwrap();

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 100,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: true,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    // Partial bytes land directly in the real destination (inplace).
    h.file_tx
        .send(FileMessage::Chunk(b"partial".to_vec()))
        .unwrap();

    // Simulate a mid-transfer error (network drop) via Abort.
    h.file_tx
        .send(FileMessage::Abort {
            reason: "simulated mid-transfer network drop".into(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err(), "abort must surface as an error");

    // The whole fix: the destination must still exist after the aborted
    // inplace transfer. Upstream leaves the partial write; we must not
    // delete the user's file.
    assert!(
        file_path.exists(),
        "inplace abort must NOT delete the pre-existing destination"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// #511 control: a mid-transfer abort on the default (non-inplace)
/// temp+rename path with `PartialMode::None` still discards the temp file,
/// so a pre-existing destination is left exactly as it was - untouched by
/// the failed new transfer. This proves the fix is scoped to the inplace
/// guard and does not weaken temp-file cleanup on the error path.
#[test]
fn non_inplace_abort_discards_temp_and_leaves_existing_dest() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("non_inplace_abort_preexisting.dat");

    // Pre-existing destination. A non-inplace transfer writes to a temp
    // file and only renames onto this dest on success, so an aborted new
    // transfer must leave it untouched.
    fs::write(&file_path, b"prior destination content").unwrap();

    let config = DiskCommitConfig {
        partial_mode: PartialMode::None,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

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
        .send(FileMessage::Chunk(b"new partial".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Abort {
            reason: "simulated mid-transfer network drop".into(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    // The pre-existing destination is untouched: the temp file was discarded
    // and never renamed over it. The original content survives verbatim.
    assert!(
        file_path.exists(),
        "pre-existing destination must survive a failed non-inplace transfer"
    );
    assert_eq!(
        fs::read(&file_path).unwrap(),
        b"prior destination content",
        "pre-existing destination content must be untouched by the aborted temp transfer"
    );

    // No orphan temp file must remain in the destination directory.
    let leftover_temps: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".non_inplace_abort_preexisting.dat.")
        })
        .collect();
    assert!(
        leftover_temps.is_empty(),
        "aborted non-inplace transfer must discard its temp file"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Verifies that aborting a multi-chunk transfer mid-stream with
/// `PartialMode::PartialDir` moves the partial file to the partial-dir,
/// not the final destination. The partial file should contain only the
/// data written before the abort.
///
/// upstream: cleanup.c:105-115 - handle_partial_dir() on abort
#[test]
fn partial_dir_abort_mid_stream_moves_to_partial_dir() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("mid_abort.dat");
    let partial_dir = dir.path().join(".rsync-part");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    // Begin a file that would be 300 bytes total.
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

    // Write two chunks, then abort before completing.
    h.file_tx
        .send(FileMessage::Chunk(b"first-chunk-".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"second-chunk-".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Abort {
            reason: "simulated mid-transfer kill".into(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    // Partial file must be in the partial-dir, not at the destination.
    let partial_path = partial_dir.join("mid_abort.dat");
    assert!(
        partial_path.exists(),
        "partial file must be in partial-dir after abort"
    );
    assert!(
        !file_path.exists(),
        "destination must NOT have the file after abort with partial-dir"
    );

    // The partial file should have the data written before the abort.
    let content = fs::read(&partial_path).unwrap();
    assert_eq!(content, b"first-chunk-second-chunk-");
    assert!(
        !content.is_empty(),
        "partial file must contain data from written chunks"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Verifies that a channel disconnect (simulating SIGKILL / connection drop)
/// during a transfer with `PartialMode::PartialDir` retains the partial
/// file in the partial-dir. The final destination must not exist.
///
/// upstream: cleanup.c - retain partial on unexpected disconnect
#[test]
fn partial_dir_channel_disconnect_moves_to_partial_dir() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("disconnect_dir.dat");
    let partial_dir = dir.path().join(".rsync-part");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

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
        .send(FileMessage::Chunk(b"data-before-kill".to_vec()))
        .unwrap();

    // Drop the sender to simulate a sudden connection loss / kill.
    drop(h.file_tx);
    h.join_handle.join().unwrap();

    let partial_path = partial_dir.join("disconnect_dir.dat");
    assert!(
        partial_path.exists(),
        "partial file must be in partial-dir after channel disconnect"
    );
    assert!(
        !file_path.exists(),
        "destination must NOT exist after channel disconnect with partial-dir"
    );
    assert_eq!(fs::read(&partial_path).unwrap(), b"data-before-kill");
}

/// Verifies that the partial-dir is auto-created when it does not exist
/// at interrupt time. upstream rsync's cleanup.c creates the directory
/// on demand via handle_partial_dir().
#[test]
fn partial_dir_auto_created_on_interrupt() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("autocreate.dat");
    let partial_dir = dir.path().join(".auto-partial");

    // The partial-dir does NOT exist yet.
    assert!(!partial_dir.exists());

    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 200,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"trigger-autocreate".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    // The partial-dir must have been created on demand.
    assert!(
        partial_dir.exists(),
        "partial-dir must be auto-created when it does not exist"
    );
    assert!(partial_dir.is_dir());

    let partial_path = partial_dir.join("autocreate.dat");
    assert!(
        partial_path.exists(),
        "partial file must be placed in the auto-created directory"
    );
    assert_eq!(fs::read(&partial_path).unwrap(), b"trigger-autocreate");

    // Destination must not exist.
    assert!(
        !file_path.exists(),
        "destination must NOT exist when using partial-dir"
    );

    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

/// Verifies mid-stream interrupt with multiple chunks written: the partial
/// file in the partial-dir has a size between zero and the target, proving
/// it captured a true partial transfer.
#[test]
fn partial_dir_mid_stream_has_intermediate_size() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("midsize.dat");
    let partial_dir = dir.path().join(".rsync-part");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    let target_size: u64 = 1000;
    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    // Write 3 chunks of 100 bytes each (300 of 1000 target).
    let chunk = vec![0xAB_u8; 100];
    for _ in 0..3 {
        h.file_tx.send(FileMessage::Chunk(chunk.clone())).unwrap();
    }

    h.file_tx
        .send(FileMessage::Abort {
            reason: "interrupt at 300/1000 bytes".into(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    let partial_path = partial_dir.join("midsize.dat");
    assert!(partial_path.exists());

    let partial_size = fs::metadata(&partial_path).unwrap().len();
    assert!(
        partial_size > 0,
        "partial file must not be empty (got {partial_size} bytes)",
    );
    assert!(
        partial_size < target_size,
        "partial file ({partial_size} bytes) must be smaller than target ({target_size} bytes)",
    );
    assert_eq!(partial_size, 300);

    assert!(!file_path.exists(), "destination must not exist");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Verifies that the whole-file (coalesced) path does NOT retain a partial
/// when the transfer completes successfully - only interrupted transfers
/// use the partial-dir.
#[test]
fn partial_dir_whole_file_success_no_leftover_in_partial_dir() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("whole_ok.dat");
    let partial_dir = dir.path().join(".rsync-part");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 12,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"complete-dat".to_vec(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 12);

    // Destination should exist with full content.
    assert!(file_path.exists());
    assert_eq!(fs::read(&file_path).unwrap(), b"complete-dat");

    // No leftover in partial-dir (dir may or may not exist, but file must not).
    let would_be_partial = partial_dir.join("whole_ok.dat");
    assert!(
        !would_be_partial.exists(),
        "successful transfer must not leave a file in partial-dir"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

#[test]
fn delay_updates_staging_path_computes_correct_path() {
    let final_path = std::path::Path::new("/dest/subdir/file.txt");
    let staging = delay_updates_staging_path(final_path);
    assert_eq!(
        staging,
        std::path::PathBuf::from("/dest/subdir/.~tmp~/file.txt")
    );
}

#[test]
fn delay_updates_staging_path_root_level_file() {
    let final_path = std::path::Path::new("/dest/top.txt");
    let staging = delay_updates_staging_path(final_path);
    assert_eq!(staging, std::path::PathBuf::from("/dest/.~tmp~/top.txt"));
}

#[test]
fn delay_updates_staging_path_deeply_nested() {
    let final_path = std::path::Path::new("/dest/a/b/c/deep.txt");
    let staging = delay_updates_staging_path(final_path);
    assert_eq!(
        staging,
        std::path::PathBuf::from("/dest/a/b/c/.~tmp~/deep.txt")
    );
}

/// Files committed to `.~tmp~/` staging paths are renamed to final
/// destinations after `handle_delayed_updates()` runs.
#[test]
fn delay_updates_single_file_reaches_final_destination() {
    let dir = test_support::create_tempdir();
    let staging_dir = dir.path().join(DELAY_UPDATES_PARTIAL_DIR);
    fs::create_dir(&staging_dir).unwrap();

    let staging_path = staging_dir.join("file.txt");
    let final_path = dir.path().join("file.txt");
    fs::write(&staging_path, b"staged content").unwrap();

    let outcomes = vec![DelayedUpdateEntry {
        staging_path: staging_path.clone(),
        final_path: final_path.clone(),
    }];

    handle_delayed_updates(&outcomes).unwrap();

    assert!(
        final_path.exists(),
        "file must appear at final destination after sweep"
    );
    assert_eq!(fs::read(&final_path).unwrap(), b"staged content");
    assert!(
        !staging_path.exists(),
        "staging path must be empty after rename"
    );
}

/// Multiple files all reach their final destination after the sweep.
#[test]
fn delay_updates_multiple_files_reach_final_destination() {
    let dir = test_support::create_tempdir();
    let staging_dir = dir.path().join(DELAY_UPDATES_PARTIAL_DIR);
    fs::create_dir(&staging_dir).unwrap();

    let mut outcomes = Vec::new();
    for i in 0..5 {
        let name = format!("file{i}.dat");
        let content = format!("content-{i}");
        let staging = staging_dir.join(&name);
        let final_dest = dir.path().join(&name);
        fs::write(&staging, content.as_bytes()).unwrap();
        outcomes.push(DelayedUpdateEntry {
            staging_path: staging,
            final_path: final_dest,
        });
    }

    handle_delayed_updates(&outcomes).unwrap();

    for i in 0..5 {
        let name = format!("file{i}.dat");
        let expected = format!("content-{i}");
        let final_dest = dir.path().join(&name);
        assert!(
            final_dest.exists(),
            "{name} must exist at final destination"
        );
        assert_eq!(fs::read(&final_dest).unwrap(), expected.as_bytes());
    }

    // All staging files must be gone.
    assert!(
        fs::read_dir(&staging_dir).is_err() || fs::read_dir(&staging_dir).unwrap().count() == 0,
        "staging directory should be empty or removed"
    );
}

/// The `.~tmp~/` staging directory is removed after the sweep completes.
#[test]
fn delay_updates_staging_directory_cleaned_up() {
    let dir = test_support::create_tempdir();
    let staging_dir = dir.path().join(DELAY_UPDATES_PARTIAL_DIR);
    fs::create_dir(&staging_dir).unwrap();

    let staging_path = staging_dir.join("cleanup.txt");
    let final_path = dir.path().join("cleanup.txt");
    fs::write(&staging_path, b"cleanup test").unwrap();

    let outcomes = vec![DelayedUpdateEntry {
        staging_path,
        final_path,
    }];

    handle_delayed_updates(&outcomes).unwrap();

    assert!(
        !staging_dir.exists(),
        ".~tmp~ staging directory must be removed after sweep"
    );
}

/// Nested directory structures: files in subdirectories each have their own
/// `.~tmp~/` staging directory, all cleaned up after the sweep.
#[test]
fn delay_updates_nested_directories_all_reach_final_destination() {
    let dir = test_support::create_tempdir();
    let dest_root = dir.path().join("dest");

    // Create nested directory structure with staging dirs.
    let sub1 = dest_root.join("sub1");
    let sub2 = dest_root.join("sub1").join("sub2");
    fs::create_dir_all(&sub1).unwrap();
    fs::create_dir_all(&sub2).unwrap();

    let staging_root = dest_root.join(DELAY_UPDATES_PARTIAL_DIR);
    let staging_sub1 = sub1.join(DELAY_UPDATES_PARTIAL_DIR);
    let staging_sub2 = sub2.join(DELAY_UPDATES_PARTIAL_DIR);
    fs::create_dir(&staging_root).unwrap();
    fs::create_dir(&staging_sub1).unwrap();
    fs::create_dir(&staging_sub2).unwrap();

    // Stage files at each level.
    fs::write(staging_root.join("top.txt"), b"top level").unwrap();
    fs::write(staging_sub1.join("mid.txt"), b"mid level").unwrap();
    fs::write(staging_sub2.join("deep.txt"), b"deep level").unwrap();

    let outcomes = vec![
        DelayedUpdateEntry {
            staging_path: staging_root.join("top.txt"),
            final_path: dest_root.join("top.txt"),
        },
        DelayedUpdateEntry {
            staging_path: staging_sub1.join("mid.txt"),
            final_path: sub1.join("mid.txt"),
        },
        DelayedUpdateEntry {
            staging_path: staging_sub2.join("deep.txt"),
            final_path: sub2.join("deep.txt"),
        },
    ];

    handle_delayed_updates(&outcomes).unwrap();

    // Verify all files at final destinations.
    assert_eq!(fs::read(dest_root.join("top.txt")).unwrap(), b"top level");
    assert_eq!(fs::read(sub1.join("mid.txt")).unwrap(), b"mid level");
    assert_eq!(fs::read(sub2.join("deep.txt")).unwrap(), b"deep level");

    // All staging directories must be removed.
    assert!(
        !staging_root.exists(),
        "root .~tmp~ directory must be removed"
    );
    assert!(
        !staging_sub1.exists(),
        "sub1 .~tmp~ directory must be removed"
    );
    assert!(
        !staging_sub2.exists(),
        "sub2 .~tmp~ directory must be removed"
    );
}

/// Empty outcomes list is a no-op.
#[test]
fn delay_updates_empty_outcomes_is_noop() {
    handle_delayed_updates(&[]).unwrap();
}

/// End-to-end: disk commit thread writes files to staging paths, then
/// `handle_delayed_updates` renames them to final destinations.
///
/// This simulates the full --delay-updates remote receiver flow:
/// 1. The file_path in BeginMessage points to `.~tmp~/filename`.
/// 2. The disk thread commits the file to that staging path.
/// 3. After all files complete, handle_delayed_updates renames to final.
#[test]
fn delay_updates_disk_thread_e2e_single_file() {
    let dir = test_support::create_tempdir();
    let staging_dir = dir.path().join(DELAY_UPDATES_PARTIAL_DIR);
    fs::create_dir(&staging_dir).unwrap();

    let staging_path = staging_dir.join("e2e.dat");
    let final_path = dir.path().join("e2e.dat");

    // Step 1: Use the disk thread to write to the staging path.
    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: staging_path.clone(),
            target_size: 12,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();

    h.file_tx
        .send(FileMessage::Chunk(b"e2e content!".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 12);

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();

    // After disk thread commit, file should be at staging path, not final.
    assert!(
        staging_path.exists(),
        "file must be at staging path after disk thread commit"
    );
    assert!(
        !final_path.exists(),
        "file must NOT be at final path before sweep"
    );

    // Step 2: Run the delay-updates sweep.
    let outcomes = vec![DelayedUpdateEntry {
        staging_path: staging_path.clone(),
        final_path: final_path.clone(),
    }];

    handle_delayed_updates(&outcomes).unwrap();

    // After sweep, file should be at final destination.
    assert!(
        final_path.exists(),
        "file must appear at final destination after sweep"
    );
    assert_eq!(fs::read(&final_path).unwrap(), b"e2e content!");
    assert!(
        !staging_path.exists(),
        "staging path must be empty after sweep"
    );
    assert!(
        !staging_dir.exists(),
        ".~tmp~ directory must be removed after sweep"
    );
}

/// End-to-end: multiple files through the disk thread then bulk sweep.
#[test]
fn delay_updates_disk_thread_e2e_multiple_files() {
    let dir = test_support::create_tempdir();
    let staging_dir = dir.path().join(DELAY_UPDATES_PARTIAL_DIR);
    fs::create_dir(&staging_dir).unwrap();

    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();

    let mut outcomes = Vec::new();
    for i in 0..4 {
        let name = format!("multi{i}.dat");
        let content = format!("multi-content-{i}");
        let staging_path = staging_dir.join(&name);
        let final_path = dir.path().join(&name);

        h.file_tx
            .send(FileMessage::Begin(Box::new(BeginMessage {
                file_path: staging_path.clone(),
                target_size: content.len() as u64,
                file_entry_index: i,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            })))
            .unwrap();

        h.file_tx
            .send(FileMessage::Chunk(content.into_bytes()))
            .unwrap();
        h.file_tx.send(FileMessage::Commit).unwrap();

        let result = h.result_rx.recv().unwrap().unwrap();
        assert_eq!(result.file_entry_index, i);

        outcomes.push(DelayedUpdateEntry {
            staging_path,
            final_path,
        });
    }

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();

    // Before sweep: all files at staging, none at final.
    for outcome in &outcomes {
        assert!(
            outcome.staging_path.exists(),
            "staging must exist before sweep: {}",
            outcome.staging_path.display()
        );
        assert!(
            !outcome.final_path.exists(),
            "final must not exist before sweep: {}",
            outcome.final_path.display()
        );
    }

    handle_delayed_updates(&outcomes).unwrap();

    // After sweep: all files at final, staging dir removed.
    for (i, outcome) in outcomes.iter().enumerate() {
        let expected = format!("multi-content-{i}");
        assert!(
            outcome.final_path.exists(),
            "final must exist after sweep: {}",
            outcome.final_path.display()
        );
        assert_eq!(fs::read(&outcome.final_path).unwrap(), expected.as_bytes());
        assert!(
            !outcome.staging_path.exists(),
            "staging must be gone after sweep: {}",
            outcome.staging_path.display()
        );
    }
    assert!(
        !staging_dir.exists(),
        ".~tmp~ directory must be removed after sweep"
    );
}

/// End-to-end with whole-file coalesced writes through the disk thread.
#[test]
fn delay_updates_disk_thread_e2e_whole_file() {
    let dir = test_support::create_tempdir();
    let staging_dir = dir.path().join(DELAY_UPDATES_PARTIAL_DIR);
    fs::create_dir(&staging_dir).unwrap();

    let staging_path = staging_dir.join("whole.dat");
    let final_path = dir.path().join("whole.dat");

    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();

    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: staging_path.clone(),
                target_size: 17,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"whole file sweep!".to_vec(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result.bytes_written, 17);

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();

    assert!(staging_path.exists());
    assert!(!final_path.exists());

    let outcomes = vec![DelayedUpdateEntry {
        staging_path: staging_path.clone(),
        final_path: final_path.clone(),
    }];

    handle_delayed_updates(&outcomes).unwrap();

    assert!(final_path.exists());
    assert_eq!(fs::read(&final_path).unwrap(), b"whole file sweep!");
    assert!(!staging_dir.exists());
}

/// End-to-end with nested subdirectories through the disk thread.
#[test]
fn delay_updates_disk_thread_e2e_nested_dirs() {
    let dir = test_support::create_tempdir();
    let sub = dir.path().join("subdir");
    fs::create_dir(&sub).unwrap();

    let staging_root = dir.path().join(DELAY_UPDATES_PARTIAL_DIR);
    let staging_sub = sub.join(DELAY_UPDATES_PARTIAL_DIR);
    fs::create_dir(&staging_root).unwrap();
    fs::create_dir(&staging_sub).unwrap();

    let h = spawn_disk_thread(DiskCommitConfig::default()).unwrap();

    // File at root level staging.
    let staging_root_file = staging_root.join("root.txt");
    let final_root_file = dir.path().join("root.txt");
    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: staging_root_file.clone(),
            target_size: 9,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"root data".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();
    let _ = h.result_rx.recv().unwrap().unwrap();

    // File at sub level staging.
    let staging_sub_file = staging_sub.join("nested.txt");
    let final_sub_file = sub.join("nested.txt");
    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: staging_sub_file.clone(),
            target_size: 11,
            file_entry_index: 1,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"nested data".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();
    let _ = h.result_rx.recv().unwrap().unwrap();

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();

    // Before sweep.
    assert!(staging_root_file.exists());
    assert!(staging_sub_file.exists());
    assert!(!final_root_file.exists());
    assert!(!final_sub_file.exists());

    let outcomes = vec![
        DelayedUpdateEntry {
            staging_path: staging_root_file,
            final_path: final_root_file.clone(),
        },
        DelayedUpdateEntry {
            staging_path: staging_sub_file,
            final_path: final_sub_file.clone(),
        },
    ];

    handle_delayed_updates(&outcomes).unwrap();

    // After sweep.
    assert_eq!(fs::read(&final_root_file).unwrap(), b"root data");
    assert_eq!(fs::read(&final_sub_file).unwrap(), b"nested data");
    assert!(!staging_root.exists(), "root .~tmp~ must be removed");
    assert!(!staging_sub.exists(), "sub .~tmp~ must be removed");
}

/// Verifies the constant matches the upstream rsync `.~tmp~` value.
#[test]
fn delay_updates_partial_dir_constant_matches_upstream() {
    assert_eq!(DELAY_UPDATES_PARTIAL_DIR, ".~tmp~");
}

/// `delay_updates_staging_path` round-trips with `handle_delayed_updates`:
/// compute staging paths, create files there, sweep, and verify final.
#[test]
fn delay_updates_staging_path_roundtrip() {
    let dir = test_support::create_tempdir();
    let dest = dir.path().join("dest");
    fs::create_dir(&dest).unwrap();

    let files = ["alpha.txt", "beta.txt", "gamma.txt"];
    let mut outcomes = Vec::new();

    for (i, name) in files.iter().enumerate() {
        let final_path = dest.join(name);
        let staging_path = delay_updates_staging_path(&final_path);

        // Create staging directory and file.
        if let Some(parent) = staging_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let content = format!("data-{i}");
        fs::write(&staging_path, content.as_bytes()).unwrap();

        outcomes.push(DelayedUpdateEntry {
            staging_path,
            final_path,
        });
    }

    handle_delayed_updates(&outcomes).unwrap();

    for (i, name) in files.iter().enumerate() {
        let expected = format!("data-{i}");
        assert_eq!(
            fs::read(dest.join(name)).unwrap(),
            expected.as_bytes(),
            "{name} content mismatch"
        );
    }

    assert!(
        !dest.join(DELAY_UPDATES_PARTIAL_DIR).exists(),
        ".~tmp~ staging directory must be removed"
    );
}

/// Sweep replaces existing destination files atomically.
#[test]
fn delay_updates_sweep_replaces_existing_files() {
    let dir = test_support::create_tempdir();
    let staging_dir = dir.path().join(DELAY_UPDATES_PARTIAL_DIR);
    fs::create_dir(&staging_dir).unwrap();

    // Pre-existing destination file.
    let final_path = dir.path().join("existing.txt");
    fs::write(&final_path, b"old content").unwrap();

    // Staged replacement.
    let staging_path = staging_dir.join("existing.txt");
    fs::write(&staging_path, b"new content").unwrap();

    let outcomes = vec![DelayedUpdateEntry {
        staging_path,
        final_path: final_path.clone(),
    }];

    handle_delayed_updates(&outcomes).unwrap();

    assert_eq!(
        fs::read(&final_path).unwrap(),
        b"new content",
        "sweep must replace existing destination content"
    );
}

/// Verifies that a partial file retained via `--partial` has its mtime
/// stamped to epoch 0. Upstream cleanup.c:174-178 does this so that a
/// subsequent `--update` run does not skip the partial file as "up to date".
#[test]
fn partial_mode_partial_stamps_mtime_zero_on_shutdown() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("partial_mtime_zero.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

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
        .send(FileMessage::Chunk(b"partial content".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    assert!(file_path.exists(), "partial file must be retained");
    assert_eq!(fs::read(&file_path).unwrap(), b"partial content");

    // The retained partial file must have mtime = Unix epoch (1970-01-01).
    let mtime = filetime::FileTime::from_last_modification_time(&fs::metadata(&file_path).unwrap());
    let unix_epoch = filetime::FileTime::from_unix_time(0, 0);
    assert_eq!(
        mtime, unix_epoch,
        "partial file mtime must be stamped to Unix epoch"
    );

    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

/// Verifies that mtime=0 is also stamped on partial files retained via
/// abort (not just shutdown). Both interrupt paths go through
/// `retain_partial_file`.
#[test]
fn partial_mode_partial_stamps_mtime_zero_on_abort() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("partial_mtime_zero_abort.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

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
        .send(FileMessage::Chunk(b"abort partial".to_vec()))
        .unwrap();
    h.file_tx
        .send(FileMessage::Abort {
            reason: "test abort".into(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    assert!(file_path.exists(), "partial file must be retained on abort");

    let mtime = filetime::FileTime::from_last_modification_time(&fs::metadata(&file_path).unwrap());
    let unix_epoch = filetime::FileTime::from_unix_time(0, 0);
    assert_eq!(
        mtime, unix_epoch,
        "partial file mtime must be stamped to Unix epoch on abort"
    );

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Verifies that partial files retained via `--partial-dir` do NOT have
/// their mtime stamped to 0. Upstream only stamps mtime=0 for plain
/// `--partial` (cleanup.c:174-178), not `--partial-dir`.
#[test]
fn partial_mode_partial_dir_does_not_stamp_mtime_zero() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("partial_dir_mtime.dat");
    let partial_dir = dir.path().join(".rsync-partial");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::PartialDir(partial_dir.clone()),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

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
        .send(FileMessage::Chunk(b"partial dir content".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    let result = h.result_rx.recv().unwrap();
    assert!(result.is_err());

    let partial_path = partial_dir.join("partial_dir_mtime.dat");
    assert!(
        partial_path.exists(),
        "partial file must exist in partial-dir"
    );

    // partial-dir files should NOT have mtime stamped to epoch.
    let mtime =
        filetime::FileTime::from_last_modification_time(&fs::metadata(&partial_path).unwrap());
    let unix_epoch = filetime::FileTime::from_unix_time(0, 0);
    assert_ne!(
        mtime, unix_epoch,
        "partial-dir files must not have mtime stamped to epoch"
    );

    drop(h.file_tx);
    h.join_handle.join().unwrap();
}

/// Verifies that mtime=0 is stamped on partial files retained via channel
/// disconnect (connection drop). This is another interrupt path that goes
/// through `retain_partial_file`.
#[test]
fn partial_mode_partial_stamps_mtime_zero_on_disconnect() {
    let dir = test_support::create_tempdir();
    let file_path = dir.path().join("partial_mtime_disconnect.dat");

    let config = DiskCommitConfig {
        partial_mode: PartialMode::Partial,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

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
        .send(FileMessage::Chunk(b"disconnect partial".to_vec()))
        .unwrap();

    // Drop sender to simulate channel disconnect.
    drop(h.file_tx);
    h.join_handle.join().unwrap();

    assert!(
        file_path.exists(),
        "partial file must be retained on disconnect"
    );

    let mtime = filetime::FileTime::from_last_modification_time(&fs::metadata(&file_path).unwrap());
    let unix_epoch = filetime::FileTime::from_unix_time(0, 0);
    assert_eq!(
        mtime, unix_epoch,
        "partial file mtime must be stamped to Unix epoch on disconnect"
    );
}

/// Verifies that when a transfer with `delay_updates` is interrupted via
/// Shutdown after some files have been committed, already-staged files remain
/// in `.~tmp~/` and are NOT renamed to their final destinations.
///
/// This mirrors upstream behavior: `handle_delayed_updates()` is never called
/// when the transfer is interrupted, so partially-transferred files stay in
/// the staging directory as valid partials for resume.
///
/// upstream: receiver.c:584-585 - handle_delayed_updates() only at phase 2
#[test]
fn delay_updates_interrupt_leaves_committed_files_in_staging() {
    let dir = test_support::create_tempdir();

    let config = DiskCommitConfig {
        delay_updates: true,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    // Commit file 1 successfully - it should be staged in .~tmp~/.
    let file1_path = dir.path().join("file1.dat");
    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file1_path.clone(),
                target_size: 12,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"file1 staged".to_vec(),
        })
        .unwrap();

    let result1 = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result1.bytes_written, 12);
    assert!(result1.delayed_path.is_some());

    // Commit file 2 successfully - also staged.
    let file2_path = dir.path().join("file2.dat");
    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file2_path.clone(),
                target_size: 12,
                file_entry_index: 1,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"file2 staged".to_vec(),
        })
        .unwrap();

    let result2 = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result2.bytes_written, 12);
    assert!(result2.delayed_path.is_some());

    // Simulate interrupt: send Shutdown instead of continuing the transfer.
    // The rename sweep (handle_delayed_updates) is never called.
    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();

    // Final paths must NOT exist - no rename sweep was performed.
    assert!(
        !file1_path.exists(),
        "file1 final path must not exist after interrupt"
    );
    assert!(
        !file2_path.exists(),
        "file2 final path must not exist after interrupt"
    );

    // Staging paths must still exist with correct content.
    let staging1 = dir.path().join(".~tmp~").join("file1.dat");
    let staging2 = dir.path().join(".~tmp~").join("file2.dat");
    assert!(
        staging1.exists(),
        "file1 must remain in .~tmp~/ after interrupt"
    );
    assert!(
        staging2.exists(),
        "file2 must remain in .~tmp~/ after interrupt"
    );
    assert_eq!(fs::read(&staging1).unwrap(), b"file1 staged");
    assert_eq!(fs::read(&staging2).unwrap(), b"file2 staged");
}

/// Verifies that when a file is mid-transfer (Begin + Chunk sent, no Commit)
/// and the disk thread receives Shutdown with `delay_updates`, the in-progress
/// file's temp file is cleaned up but any previously committed files remain
/// staged in `.~tmp~/`.
///
/// upstream: receiver.c:584 - handle_delayed_updates() only after successful
/// completion; interrupted transfers leave staged files for resume.
#[test]
fn delay_updates_interrupt_mid_file_retains_prior_staged_files() {
    let dir = test_support::create_tempdir();

    let config = DiskCommitConfig {
        delay_updates: true,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    // Commit file 1 successfully.
    let file1_path = dir.path().join("committed.dat");
    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file1_path.clone(),
                target_size: 9,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"committed".to_vec(),
        })
        .unwrap();

    let result1 = h.result_rx.recv().unwrap().unwrap();
    assert_eq!(result1.bytes_written, 9);

    // Start file 2 but interrupt before Commit.
    let file2_path = dir.path().join("interrupted.dat");
    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file2_path.clone(),
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

    // Interrupt: Shutdown while file 2 is in progress.
    h.file_tx.send(FileMessage::Shutdown).unwrap();

    // The in-progress file returns an error.
    let result2 = h.result_rx.recv().unwrap();
    assert!(result2.is_err());

    h.join_handle.join().unwrap();

    // File 1 must remain staged in .~tmp~/.
    let staging1 = dir.path().join(".~tmp~").join("committed.dat");
    assert!(
        staging1.exists(),
        "previously committed file must remain staged after interrupt"
    );
    assert_eq!(fs::read(&staging1).unwrap(), b"committed");

    // Neither file should be at the final destination.
    assert!(!file1_path.exists(), "file1 must not be at final path");
    assert!(!file2_path.exists(), "file2 must not be at final path");
}

/// Verifies that the separation between "commit to staging" and "sweep to
/// final" is correct: after committing files with `delay_updates`, the disk
/// thread has only staged them; a manual `fs::rename` sweep confirms the
/// staged content is valid and complete. Without the sweep, files remain
/// as partials for resume.
///
/// upstream: receiver.c:422-450 - handle_delayed_updates() bulk rename
#[test]
fn delay_updates_manual_sweep_after_commit_moves_to_final() {
    let dir = test_support::create_tempdir();

    let config = DiskCommitConfig {
        delay_updates: true,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    // Commit two files.
    let file1_path = dir.path().join("sweep1.dat");
    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file1_path.clone(),
                target_size: 6,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"sweep1".to_vec(),
        })
        .unwrap();

    let r1 = h.result_rx.recv().unwrap().unwrap();

    let file2_path = dir.path().join("sweep2.dat");
    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file2_path.clone(),
                target_size: 6,
                file_entry_index: 1,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"sweep2".to_vec(),
        })
        .unwrap();

    let r2 = h.result_rx.recv().unwrap().unwrap();

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();

    // Before sweep: files in staging, not at final path.
    assert!(!file1_path.exists());
    assert!(!file2_path.exists());
    let staging1 = r1.delayed_path.clone().unwrap();
    let staging2 = r2.delayed_path.clone().unwrap();
    assert!(staging1.exists());
    assert!(staging2.exists());

    // Simulate the sweep manually (what handle_delayed_updates does).
    fs::rename(&staging1, &file1_path).unwrap();
    fs::rename(&staging2, &file2_path).unwrap();

    // After sweep: files at final path, staging paths removed.
    assert!(file1_path.exists());
    assert!(file2_path.exists());
    assert_eq!(fs::read(&file1_path).unwrap(), b"sweep1");
    assert_eq!(fs::read(&file2_path).unwrap(), b"sweep2");
    assert!(!staging1.exists());
    assert!(!staging2.exists());
}

/// Verifies that channel disconnect (simulating process crash) with
/// `delay_updates` leaves committed files in `.~tmp~/` staging directory.
///
/// upstream: when rsync is killed mid-transfer, the rename sweep never
/// runs and staged files persist for resume.
#[test]
fn delay_updates_channel_disconnect_preserves_staged_files() {
    let dir = test_support::create_tempdir();

    let config = DiskCommitConfig {
        delay_updates: true,
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    // Commit a file successfully.
    let file_path = dir.path().join("disconnect.dat");
    h.file_tx
        .send(FileMessage::WholeFile {
            begin: Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 14,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
            }),
            data: b"disconnect data".to_vec(),
        })
        .unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    assert!(result.delayed_path.is_some());

    // Drop the sender to simulate channel disconnect (crash).
    drop(h.file_tx);
    h.join_handle.join().unwrap();

    // File must NOT be at final path.
    assert!(
        !file_path.exists(),
        "final path must not exist after disconnect"
    );

    // File must remain staged.
    let staging = dir.path().join(".~tmp~").join("disconnect.dat");
    assert!(
        staging.exists(),
        "staged file must persist after channel disconnect"
    );
    assert_eq!(fs::read(&staging).unwrap(), b"disconnect data");
}

/// Verifies that committing a file through the pipelined disk thread with
/// `--backup` (default suffix) returns a [`crate::pipeline::messages::BackupNotice`]
/// on the [`crate::pipeline::messages::CommitResult`] when a pre-existing
/// destination file is renamed out of the way. This is the wire-transfer
/// equivalent of upstream `backup.test`'s line 27 invocation:
///
/// ```text
/// rsync -ai --info=backup --no-whole-file --backup '$fromdir/' '$todir/'
/// ```
///
/// Without the notice flowing back to the main thread, the upstream
/// `grep "backed up $fn to $fn~"` would never match because the disk
/// thread's `VerbosityConfig` is never seeded with the user's `--info=backup`.
#[test]
fn pipelined_backup_default_suffix_returns_destination_relative_notice() {
    let dir = test_support::create_tempdir();
    let dest_dir = dir.path().to_path_buf();
    let file_path = dest_dir.join("payload.bin");
    fs::write(&file_path, b"pre-existing payload").unwrap();

    let config = DiskCommitConfig {
        dest_dir: Some(dest_dir.clone()),
        backup: Some(BackupConfig {
            dest_dir: dest_dir.clone(),
            backup_dir: None,
            suffix: OsString::from("~"),
        }),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

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
        .send(FileMessage::Chunk(b"fresh".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    let notice = result
        .backup_notice
        .expect("disk thread must report the backup notice on the CommitResult");
    assert_eq!(notice.original, std::path::PathBuf::from("payload.bin"));
    assert_eq!(notice.backup, std::path::PathBuf::from("payload.bin~"));

    let backup_path = dest_dir.join("payload.bin~");
    assert_eq!(fs::read(&backup_path).unwrap(), b"pre-existing payload");
    assert_eq!(fs::read(&file_path).unwrap(), b"fresh");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}

/// Verifies that committing a file with `--backup --backup-dir` reports the
/// backup destination-relative to `dest_dir` so the upstream
/// `backup.test` grep `backed up $fn to .*/$fn$` matches (line 43, 56).
/// Mirrors upstream:
///
/// ```text
/// rsync -ai --info=backup --no-whole-file --backup \
///     --backup-dir='$bakdir' '$fromdir/' '$todir/'
/// ```
#[test]
fn pipelined_backup_with_backup_dir_reports_destination_relative_paths() {
    let dir = test_support::create_tempdir();
    let dest_dir = dir.path().join("todir");
    let backup_dir = dir.path().join("bakdir");
    fs::create_dir_all(&dest_dir).unwrap();
    fs::create_dir_all(&backup_dir).unwrap();

    let deep = dest_dir.join("deep");
    fs::create_dir_all(&deep).unwrap();
    let file_path = deep.join("name1");
    fs::write(&file_path, b"pre-existing content").unwrap();

    let config = DiskCommitConfig {
        dest_dir: Some(dest_dir.clone()),
        backup: Some(BackupConfig {
            dest_dir: dest_dir.clone(),
            backup_dir: Some(backup_dir.clone()),
            suffix: OsString::from("~"),
        }),
        ..DiskCommitConfig::default()
    };
    let h = spawn_disk_thread(config).unwrap();

    h.file_tx
        .send(FileMessage::Begin(Box::new(BeginMessage {
            file_path: file_path.clone(),
            target_size: 7,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
        })))
        .unwrap();
    h.file_tx
        .send(FileMessage::Chunk(b"updated".to_vec()))
        .unwrap();
    h.file_tx.send(FileMessage::Commit).unwrap();

    let result = h.result_rx.recv().unwrap().unwrap();
    let notice = result
        .backup_notice
        .expect("disk thread must report the backup notice when --backup-dir is set");
    // upstream: backup.c:352 emits paths relative to the destination root
    // for both `fname` and `buf`, matching upstream test grep
    // `backed up $fn to .*/$fn$`.
    assert_eq!(notice.original, std::path::PathBuf::from("deep/name1"));
    assert!(
        notice.backup.ends_with("deep/name1~"),
        "backup path must end with the original relative path; got {:?}",
        notice.backup
    );

    // Original was moved into the backup-dir tree (with the trailing suffix).
    let backup_path = backup_dir.join("deep/name1~");
    assert_eq!(fs::read(&backup_path).unwrap(), b"pre-existing content");
    // Destination now holds the new content.
    assert_eq!(fs::read(&file_path).unwrap(), b"updated");

    h.file_tx.send(FileMessage::Shutdown).unwrap();
    h.join_handle.join().unwrap();
}
