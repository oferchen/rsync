use super::*;
use std::io::Cursor;
use std::time::Duration;

#[test]
fn test_ack_entry_success() {
    let entry = AckEntry::success(42);
    assert_eq!(entry.ndx, 42);
    assert_eq!(entry.status, AckStatus::Success);
    assert!(entry.error_msg.is_none());
}

#[test]
fn test_ack_entry_error() {
    let entry = AckEntry::error(10, "test error");
    assert_eq!(entry.ndx, 10);
    assert_eq!(entry.status, AckStatus::Error);
    assert_eq!(entry.error_msg.as_deref(), Some("test error"));
}

#[test]
fn test_ack_entry_roundtrip_success() {
    let entry = AckEntry::success(100);
    let mut buf = Vec::new();
    entry.write(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let read_entry = AckEntry::read(&mut cursor).unwrap();

    assert_eq!(read_entry.ndx, 100);
    assert_eq!(read_entry.status, AckStatus::Success);
    assert!(read_entry.error_msg.is_none());
}

#[test]
fn test_ack_entry_roundtrip_error() {
    let entry = AckEntry::error(50, "file not found");
    let mut buf = Vec::new();
    entry.write(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let read_entry = AckEntry::read(&mut cursor).unwrap();

    assert_eq!(read_entry.ndx, 50);
    assert_eq!(read_entry.status, AckStatus::Error);
    assert_eq!(read_entry.error_msg.as_deref(), Some("file not found"));
}

#[test]
fn test_ack_batcher_queue_and_take() {
    let mut batcher = AckBatcher::new(AckBatcherConfig::default().with_batch_size(4));

    batcher.queue_success(0);
    batcher.queue_success(1);
    batcher.queue_skipped(2);

    assert_eq!(batcher.pending_count(), 3);
    assert!(!batcher.should_flush());

    batcher.queue_success(3);
    assert!(batcher.should_flush());

    let batch = batcher.take_batch();
    assert_eq!(batch.len(), 4);
    assert!(batcher.is_empty());
}

#[test]
fn test_ack_batcher_error_triggers_flush() {
    let mut batcher = AckBatcher::new(AckBatcherConfig::default().with_batch_size(16));

    batcher.queue_success(0);
    assert!(!batcher.should_flush());

    batcher.queue_error(1, "test error");
    assert!(batcher.should_flush());
}

#[test]
fn test_ack_batcher_disabled() {
    let mut batcher = AckBatcher::disabled();

    batcher.queue_success(0);
    assert!(batcher.should_flush());
}

#[test]
fn test_batch_write_and_read() {
    let batch = vec![
        AckEntry::success(0),
        AckEntry::success(1),
        AckEntry::skipped(2),
        AckEntry::error(3, "error msg"),
    ];

    let mut buf = Vec::new();
    AckBatcher::write_batch(&batch, &mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let read_batch = AckBatcher::read_batch(&mut cursor).unwrap();

    assert_eq!(read_batch.len(), 4);
    assert_eq!(read_batch[0].ndx, 0);
    assert_eq!(read_batch[0].status, AckStatus::Success);
    assert_eq!(read_batch[2].status, AckStatus::Skipped);
    assert_eq!(read_batch[3].status, AckStatus::Error);
    assert_eq!(read_batch[3].error_msg.as_deref(), Some("error msg"));
}

#[test]
fn test_batcher_config_clamps_values() {
    let config = AckBatcherConfig::default()
        .with_batch_size(0)
        .with_timeout_ms(10000);

    assert_eq!(config.batch_size, MIN_BATCH_SIZE);
    assert_eq!(config.batch_timeout_ms, MAX_BATCH_TIMEOUT_MS);

    let config2 = AckBatcherConfig::default().with_batch_size(1000);
    assert_eq!(config2.batch_size, MAX_BATCH_SIZE);
}

#[test]
fn test_batcher_stats() {
    let mut batcher = AckBatcher::new(AckBatcherConfig::default().with_batch_size(2));

    batcher.queue_success(0);
    batcher.queue_success(1);
    let _ = batcher.take_batch();

    batcher.queue_success(2);
    batcher.queue_success(3);
    let _ = batcher.take_batch();

    let stats = batcher.stats();
    assert_eq!(stats.total_sent, 4);
    assert_eq!(stats.batches_sent, 2);
    assert!((stats.average_batch_size - 2.0).abs() < f64::EPSILON);
    assert!((stats.efficiency_ratio() - 2.0).abs() < f64::EPSILON);
}

#[test]
fn test_empty_batch_write() {
    let batch: Vec<AckEntry> = Vec::new();
    let mut buf = Vec::new();
    AckBatcher::write_batch(&batch, &mut buf).unwrap();
    assert!(buf.is_empty());
}

#[test]
fn test_ack_status_from_u8() {
    assert_eq!(AckStatus::from_u8(0), AckStatus::Success);
    assert_eq!(AckStatus::from_u8(1), AckStatus::Error);
    assert_eq!(AckStatus::from_u8(2), AckStatus::Skipped);
    assert_eq!(AckStatus::from_u8(3), AckStatus::ChecksumError);
    assert_eq!(AckStatus::from_u8(4), AckStatus::IoError);
    assert_eq!(AckStatus::from_u8(255), AckStatus::Error);
}

#[test]
fn test_ack_status_is_error() {
    assert!(!AckStatus::Success.is_error());
    assert!(!AckStatus::Skipped.is_error());
    assert!(AckStatus::Error.is_error());
    assert!(AckStatus::ChecksumError.is_error());
    assert!(AckStatus::IoError.is_error());
}

#[test]
fn test_flush_if_needed_no_pending() {
    let mut batcher = AckBatcher::with_defaults();
    let mut buf = Vec::new();
    let count = batcher.flush_if_needed(&mut buf).unwrap();
    assert_eq!(count, 0);
    assert!(buf.is_empty());
}

#[test]
fn test_force_flush() {
    let mut batcher = AckBatcher::new(AckBatcherConfig::default().with_batch_size(100));
    batcher.queue_success(0);
    batcher.queue_success(1);

    assert!(!batcher.should_flush());

    let mut buf = Vec::new();
    let count = batcher.force_flush(&mut buf).unwrap();

    assert_eq!(count, 2);
    assert!(batcher.is_empty());
    assert!(!buf.is_empty());
}

#[test]
fn test_time_until_timeout_no_pending() {
    let batcher = AckBatcher::with_defaults();
    assert!(batcher.time_until_timeout().is_none());
}

#[test]
fn test_time_until_timeout_with_pending() {
    let mut batcher = AckBatcher::new(AckBatcherConfig::default().with_timeout_ms(1000));
    batcher.queue_success(0);

    let timeout = batcher.time_until_timeout();
    assert!(timeout.is_some());
    assert!(timeout.unwrap() > Duration::from_millis(900));
}

#[test]
fn test_ack_entry_checksum_error() {
    let entry = AckEntry::checksum_error(5, "mismatch");
    assert_eq!(entry.status, AckStatus::ChecksumError);
    assert!(entry.status.is_error());
}

#[test]
fn test_ack_entry_io_error() {
    let entry = AckEntry::io_error(7, "disk full");
    assert_eq!(entry.status, AckStatus::IoError);
    assert!(entry.status.is_error());
}

#[test]
fn test_batch_roundtrip_various_statuses() {
    let batch = vec![
        AckEntry::success(0),
        AckEntry::skipped(1),
        AckEntry::error(2, "generic error"),
        AckEntry::checksum_error(3, "checksum mismatch"),
        AckEntry::io_error(4, "permission denied"),
    ];

    let mut buf = Vec::new();
    AckBatcher::write_batch(&batch, &mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let read_batch = AckBatcher::read_batch(&mut cursor).unwrap();

    assert_eq!(read_batch.len(), 5);
    assert_eq!(read_batch[0].status, AckStatus::Success);
    assert_eq!(read_batch[1].status, AckStatus::Skipped);
    assert_eq!(read_batch[2].status, AckStatus::Error);
    assert_eq!(read_batch[3].status, AckStatus::ChecksumError);
    assert_eq!(read_batch[4].status, AckStatus::IoError);

    assert_eq!(read_batch[2].error_msg.as_deref(), Some("generic error"));
    assert_eq!(
        read_batch[3].error_msg.as_deref(),
        Some("checksum mismatch")
    );
    assert_eq!(
        read_batch[4].error_msg.as_deref(),
        Some("permission denied")
    );
}

#[test]
fn test_transfer_scenario_batching() {
    let config = AckBatcherConfig::default().with_batch_size(4);
    let mut batcher = AckBatcher::new(config);
    let mut network_output = Vec::new();
    let mut batches_sent = 0;

    for ndx in 0..10i32 {
        let result = if ndx == 5 {
            AckEntry::io_error(ndx, "write failed")
        } else if ndx == 7 {
            AckEntry::skipped(ndx)
        } else {
            AckEntry::success(ndx)
        };

        batcher.queue(result);

        if batcher.should_flush() {
            let count = batcher.force_flush(&mut network_output).unwrap();
            if count > 0 {
                batches_sent += 1;
            }
        }
    }

    let count = batcher.force_flush(&mut network_output).unwrap();
    if count > 0 {
        batches_sent += 1;
    }

    assert!(batches_sent >= 1);
    assert!(batches_sent <= 5);

    let mut cursor = Cursor::new(&network_output);
    let mut all_entries = Vec::new();

    while cursor.position() < network_output.len() as u64 {
        let batch = AckBatcher::read_batch(&mut cursor).unwrap();
        all_entries.extend(batch);
    }

    assert_eq!(all_entries.len(), 10);
    assert_eq!(all_entries[5].status, AckStatus::IoError);
    assert_eq!(all_entries[7].status, AckStatus::Skipped);
    for i in [0, 1, 2, 3, 4, 6, 8, 9] {
        assert_eq!(all_entries[i].status, AckStatus::Success);
    }
}

#[test]
fn test_large_transfer_efficiency() {
    let file_count = 1000;
    let batch_size = 16;

    let config = AckBatcherConfig::default().with_batch_size(batch_size);
    let mut batcher = AckBatcher::new(config);
    let mut network_output = Vec::new();

    for ndx in 0..file_count {
        batcher.queue_success(ndx);

        if batcher.should_flush() {
            batcher.force_flush(&mut network_output).unwrap();
        }
    }
    batcher.force_flush(&mut network_output).unwrap();

    let stats = batcher.stats();

    assert_eq!(stats.total_sent, file_count as u64);
    let expected_batches = (file_count as f64 / batch_size as f64).ceil() as u64;
    assert!(stats.batches_sent <= expected_batches);

    let efficiency = stats.efficiency_ratio();
    assert!(
        efficiency >= (batch_size - 1) as f64,
        "efficiency {efficiency} should be >= {}",
        batch_size - 1
    );
}

#[test]
fn test_error_immediate_flush() {
    let config = AckBatcherConfig::default()
        .with_batch_size(100)
        .with_timeout_ms(10000);

    let mut batcher = AckBatcher::new(config);

    batcher.queue_success(0);
    batcher.queue_success(1);
    assert!(!batcher.should_flush());

    batcher.queue_error(2, "test error");
    assert!(batcher.should_flush());
}

#[test]
fn test_large_batch_roundtrip() {
    let mut batch = Vec::with_capacity(256);

    for i in 0..256i32 {
        let entry = match i % 5 {
            0 => AckEntry::success(i),
            1 => AckEntry::skipped(i),
            2 => AckEntry::error(i, format!("error for file {i}")),
            3 => AckEntry::checksum_error(i, format!("checksum failed for {i}")),
            _ => AckEntry::io_error(i, format!("io error at {i}")),
        };
        batch.push(entry);
    }

    let mut buf = Vec::new();
    AckBatcher::write_batch(&batch, &mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let read_batch = AckBatcher::read_batch(&mut cursor).unwrap();

    assert_eq!(read_batch.len(), 256);
    for (i, entry) in read_batch.iter().enumerate() {
        assert_eq!(entry.ndx, i as i32);
        match i % 5 {
            0 => assert_eq!(entry.status, AckStatus::Success),
            1 => assert_eq!(entry.status, AckStatus::Skipped),
            2 => {
                assert_eq!(entry.status, AckStatus::Error);
                assert_eq!(
                    entry.error_msg.as_deref(),
                    Some(&*format!("error for file {i}"))
                );
            }
            3 => {
                assert_eq!(entry.status, AckStatus::ChecksumError);
                assert_eq!(
                    entry.error_msg.as_deref(),
                    Some(&*format!("checksum failed for {i}"))
                );
            }
            _ => {
                assert_eq!(entry.status, AckStatus::IoError);
                assert_eq!(
                    entry.error_msg.as_deref(),
                    Some(&*format!("io error at {i}"))
                );
            }
        }
    }
}

#[test]
fn test_disabled_batching() {
    let mut batcher = AckBatcher::disabled();
    let mut output = Vec::new();

    batcher.queue_success(0);
    assert!(batcher.should_flush());
    let count = batcher.force_flush(&mut output).unwrap();
    assert_eq!(count, 1);

    batcher.queue_success(1);
    assert!(batcher.should_flush());
    let count = batcher.force_flush(&mut output).unwrap();
    assert_eq!(count, 1);

    let stats = batcher.stats();
    assert_eq!(stats.total_sent, 2);
    assert_eq!(stats.batches_sent, 2);
    assert!((stats.efficiency_ratio() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn test_pipeline_config_integration() {
    use crate::pipeline::PipelineConfig;

    let pipeline_config = PipelineConfig::default()
        .with_ack_batch_size(32)
        .with_ack_batch_timeout_ms(100)
        .with_ack_batching(true);

    let ack_config = pipeline_config.ack_batcher_config();
    assert!(ack_config.is_enabled());
    assert_eq!(ack_config.batch_size, 32);
    assert_eq!(ack_config.batch_timeout_ms, 100);

    let batcher = AckBatcher::new(ack_config);
    assert_eq!(batcher.batch_size(), 32);
    assert!(batcher.is_enabled());
}

#[test]
fn test_ack_entry_wire_format_success() {
    let entry = AckEntry::success(0x12345678);
    let mut buf = Vec::new();
    entry.write(&mut buf).unwrap();

    assert_eq!(buf.len(), 5);
    assert_eq!(&buf[0..4], &[0x78, 0x56, 0x34, 0x12]);
    assert_eq!(buf[4], 0);
}

#[test]
fn test_ack_entry_wire_format_skipped() {
    let entry = AckEntry::skipped(-1);
    let mut buf = Vec::new();
    entry.write(&mut buf).unwrap();

    assert_eq!(buf.len(), 5);
    assert_eq!(&buf[0..4], &[0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(buf[4], 2);
}

#[test]
fn test_ack_entry_wire_format_error_with_message() {
    let entry = AckEntry::error(42, "test");
    let mut buf = Vec::new();
    entry.write(&mut buf).unwrap();

    assert_eq!(buf.len(), 11);
    assert_eq!(&buf[0..4], &[42, 0, 0, 0]);
    assert_eq!(buf[4], 1);
    assert_eq!(&buf[5..7], &[4, 0]);
    assert_eq!(&buf[7..11], b"test");
}

#[test]
fn test_ack_entry_wire_format_checksum_error() {
    let entry = AckEntry::checksum_error(100, "bad");
    let mut buf = Vec::new();
    entry.write(&mut buf).unwrap();

    assert_eq!(buf.len(), 10);
    assert_eq!(&buf[0..4], &[100, 0, 0, 0]);
    assert_eq!(buf[4], 3);
    assert_eq!(&buf[5..7], &[3, 0]);
    assert_eq!(&buf[7..10], b"bad");
}

#[test]
fn test_ack_entry_wire_format_io_error() {
    let entry = AckEntry::io_error(255, "IO");
    let mut buf = Vec::new();
    entry.write(&mut buf).unwrap();

    assert_eq!(buf.len(), 9);
    assert_eq!(&buf[0..4], &[255, 0, 0, 0]);
    assert_eq!(buf[4], 4);
    assert_eq!(&buf[5..7], &[2, 0]);
    assert_eq!(&buf[7..9], b"IO");
}

#[test]
fn test_ack_entry_message_truncation_at_64kb() {
    let long_msg = "x".repeat(70000);
    let entry = AckEntry::error(1, long_msg.clone());

    let mut buf = Vec::new();
    entry.write(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let read_entry = AckEntry::read(&mut cursor).unwrap();

    let read_msg = read_entry.error_msg.unwrap();
    assert_eq!(read_msg.len(), u16::MAX as usize);
    assert!(read_msg.chars().all(|c| c == 'x'));
}

#[test]
fn test_ack_entry_read_truncated_ndx() {
    let buf = [0x01, 0x02];
    let mut cursor = Cursor::new(&buf);
    let result = AckEntry::read(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn test_ack_entry_read_truncated_status() {
    let buf = [0x01, 0x00, 0x00, 0x00];
    let mut cursor = Cursor::new(&buf);
    let result = AckEntry::read(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn test_ack_entry_read_truncated_error_len() {
    let buf = [0x01, 0x00, 0x00, 0x00, 0x01];
    let mut cursor = Cursor::new(&buf);
    let result = AckEntry::read(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn test_ack_entry_read_truncated_error_msg() {
    let buf = [
        0x01, 0x00, 0x00, 0x00, // ndx = 1
        0x01, // status = Error
        0x0A, 0x00, // len = 10
        b'a', b'b', // only 2 bytes of message
    ];
    let mut cursor = Cursor::new(&buf);
    let result = AckEntry::read(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn test_ack_entry_read_zero_length_error_msg() {
    let buf = [
        0x2A, 0x00, 0x00, 0x00, // ndx = 42
        0x01, // status = Error
        0x00, 0x00, // len = 0
    ];
    let mut cursor = Cursor::new(&buf);
    let result = AckEntry::read(&mut cursor).unwrap();

    assert_eq!(result.ndx, 42);
    assert_eq!(result.status, AckStatus::Error);
    assert!(result.error_msg.is_none());
}

#[test]
fn test_batch_wire_format_count_prefix() {
    let batch = vec![
        AckEntry::success(1),
        AckEntry::success(2),
        AckEntry::success(3),
    ];

    let mut buf = Vec::new();
    AckBatcher::write_batch(&batch, &mut buf).unwrap();

    assert_eq!(&buf[0..2], &[3, 0]);
    assert_eq!(buf.len(), 17);
}

#[test]
fn test_batch_read_truncated_count() {
    let buf = [0x05];
    let mut cursor = Cursor::new(&buf);
    let result = AckBatcher::read_batch(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn test_batch_read_truncated_entries() {
    let buf = [
        0x05, 0x00, // count = 5
        0x01, 0x00, 0x00, 0x00, 0x00, // entry 0 (success)
        0x02, 0x00, // partial entry 1 (only 2 bytes of NDX)
    ];
    let mut cursor = Cursor::new(&buf);
    let result = AckBatcher::read_batch(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn test_ack_entry_non_utf8_error_message() {
    let buf = [
        0x01, 0x00, 0x00, 0x00, // ndx = 1
        0x01, // status = Error
        0x04, 0x00, // len = 4
        0x80, 0x81, 0x82, 0x83, // invalid UTF-8 bytes
    ];
    let mut cursor = Cursor::new(&buf);
    let result = AckEntry::read(&mut cursor).unwrap();

    assert_eq!(result.ndx, 1);
    assert_eq!(result.status, AckStatus::Error);
    let msg = result.error_msg.unwrap();
    assert!(msg.contains('\u{FFFD}'));
}

#[test]
fn test_ack_entry_roundtrip_all_statuses_wire_verified() {
    let test_cases = vec![
        (AckEntry::success(0), 0u8, None::<&str>),
        (AckEntry::skipped(1), 2u8, None),
        (AckEntry::error(2, "err"), 1u8, Some("err")),
        (AckEntry::checksum_error(3, "chk"), 3u8, Some("chk")),
        (AckEntry::io_error(4, "io"), 4u8, Some("io")),
    ];

    for (entry, expected_status, expected_msg) in test_cases {
        let mut buf = Vec::new();
        entry.write(&mut buf).unwrap();

        assert_eq!(buf[4], expected_status);

        let mut cursor = Cursor::new(&buf);
        let read_entry = AckEntry::read(&mut cursor).unwrap();

        assert_eq!(read_entry.ndx, entry.ndx);
        assert_eq!(read_entry.status, entry.status);
        assert_eq!(read_entry.error_msg.as_deref(), expected_msg);
    }
}
