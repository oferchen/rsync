//! Tests for the disk commit thread.

use std::fs;

use crate::pipeline::messages::{BeginMessage, FileMessage};
use crate::pipeline::spsc::TryRecvError;

use super::config::DiskCommitConfig;
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
