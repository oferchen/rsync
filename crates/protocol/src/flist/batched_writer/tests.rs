use std::io::Cursor;
use std::time::Duration;

use super::config::{BatchConfig, DEFAULT_FLUSH_TIMEOUT, DEFAULT_MAX_BYTES, DEFAULT_MAX_ENTRIES};
use super::writer::BatchedFileListWriter;
use crate::flist::entry::FileEntry;
use crate::flist::read::FileListReader;
use crate::{CompatibilityFlags, ProtocolVersion};

fn test_protocol() -> ProtocolVersion {
    ProtocolVersion::try_from(32u8).unwrap()
}

#[test]
fn batch_config_default() {
    let config = BatchConfig::default();
    assert_eq!(config.max_entries, DEFAULT_MAX_ENTRIES);
    assert_eq!(config.max_bytes, DEFAULT_MAX_BYTES);
    assert_eq!(config.flush_timeout, DEFAULT_FLUSH_TIMEOUT);
}

#[test]
fn batch_config_builder() {
    let config = BatchConfig::new()
        .with_max_entries(100)
        .with_max_bytes(128 * 1024)
        .with_flush_timeout(Duration::from_millis(200));

    assert_eq!(config.max_entries, 100);
    assert_eq!(config.max_bytes, 128 * 1024);
    assert_eq!(config.flush_timeout, Duration::from_millis(200));
}

#[test]
fn batch_config_no_auto_flush() {
    let config = BatchConfig::no_auto_flush();
    assert_eq!(config.max_entries, usize::MAX);
    assert_eq!(config.max_bytes, usize::MAX);
    assert_eq!(config.flush_timeout, Duration::MAX);
}

#[test]
fn new_batched_writer_is_empty() {
    let writer = BatchedFileListWriter::new(test_protocol());
    assert!(writer.is_empty());
    assert_eq!(writer.pending_entries(), 0);
    assert_eq!(writer.pending_bytes(), 0);
}

#[test]
fn add_entry_accumulates_in_buffer() {
    let mut writer =
        BatchedFileListWriter::with_config(test_protocol(), BatchConfig::no_auto_flush());
    let mut output = Vec::new();

    let entry1 = FileEntry::new_file("test1.txt".into(), 100, 0o644);
    let entry2 = FileEntry::new_file("test2.txt".into(), 200, 0o644);

    assert!(!writer.add_entry(&mut output, &entry1).unwrap());
    assert!(!writer.add_entry(&mut output, &entry2).unwrap());

    assert_eq!(writer.pending_entries(), 2);
    assert!(writer.pending_bytes() > 0);
    assert!(output.is_empty());
}

#[test]
fn explicit_flush_writes_to_output() {
    let mut writer =
        BatchedFileListWriter::with_config(test_protocol(), BatchConfig::no_auto_flush());
    let mut output = Vec::new();

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.add_entry(&mut output, &entry).unwrap();

    let pending_bytes = writer.pending_bytes();
    assert!(pending_bytes > 0);

    writer.flush(&mut output).unwrap();

    assert!(writer.is_empty());
    assert_eq!(output.len(), pending_bytes);
    assert_eq!(writer.stats().batches_flushed, 1);
    assert_eq!(writer.stats().explicit_flushes, 1);
}

#[test]
fn auto_flush_on_entry_count() {
    let config = BatchConfig::new().with_max_entries(2);
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    let entry1 = FileEntry::new_file("test1.txt".into(), 100, 0o644);
    let entry2 = FileEntry::new_file("test2.txt".into(), 200, 0o644);

    assert!(!writer.add_entry(&mut output, &entry1).unwrap());
    assert!(writer.add_entry(&mut output, &entry2).unwrap());

    assert!(writer.is_empty());
    assert!(!output.is_empty());
    assert_eq!(writer.stats().flushes_by_count, 1);
}

#[test]
fn auto_flush_on_byte_size() {
    let config = BatchConfig::new().with_max_entries(1000).with_max_bytes(50);
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    let mut flushed = false;
    for i in 0..100 {
        let entry = FileEntry::new_file(format!("file{i}.txt").into(), 100 * i as u64, 0o644);
        if writer.add_entry(&mut output, &entry).unwrap() {
            flushed = true;
            break;
        }
    }

    assert!(flushed);
    assert!(writer.stats().flushes_by_size > 0);
}

#[test]
fn finish_flushes_remaining_and_writes_end() {
    let mut writer =
        BatchedFileListWriter::with_config(test_protocol(), BatchConfig::no_auto_flush());
    let mut output = Vec::new();

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.add_entry(&mut output, &entry).unwrap();

    writer.finish(&mut output, None).unwrap();

    assert!(writer.is_empty());
    assert_eq!(writer.stats().batches_flushed, 1);

    // Verify end marker was written (single zero byte for basic protocol)
    assert!(*output.last().unwrap() == 0);
}

#[test]
fn finish_with_io_error() {
    let protocol = test_protocol();
    let compat_flags = CompatibilityFlags::SAFE_FILE_LIST;
    let mut writer = BatchedFileListWriter::with_compat_flags_and_config(
        protocol,
        compat_flags,
        BatchConfig::no_auto_flush(),
    );
    let mut output = Vec::new();

    writer.finish(&mut output, Some(42)).unwrap();

    assert!(!output.is_empty());
}

#[test]
fn add_entries_batch() {
    let config = BatchConfig::new().with_max_entries(3);
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    let entries: Vec<FileEntry> = (0..7)
        .map(|i| FileEntry::new_file(format!("file{i}.txt").into(), 100 * i as u64, 0o644))
        .collect();

    let flushes = writer.add_entries(&mut output, &entries).unwrap();

    // With 7 entries and max_entries=3, we should have 2 flushes
    // (entries 0,1,2 trigger flush, then 3,4,5 trigger flush, entry 6 pending)
    assert_eq!(flushes, 2);
    assert_eq!(writer.pending_entries(), 1);
}

#[test]
fn round_trip_batched_entries() {
    let protocol = test_protocol();
    let config = BatchConfig::new().with_max_entries(3);
    let mut writer = BatchedFileListWriter::with_config(protocol, config);
    let mut output = Vec::new();

    let entries: Vec<FileEntry> = (0..5)
        .map(|i| {
            let mut entry =
                FileEntry::new_file(format!("file{i}.txt").into(), 100 * i as u64, 0o644);
            entry.set_mtime(1700000000 + i as i64, 0);
            entry
        })
        .collect();

    writer.add_entries(&mut output, &entries).unwrap();
    writer.finish(&mut output, None).unwrap();

    let mut cursor = Cursor::new(&output);
    let mut reader = FileListReader::new(protocol);

    for (i, expected) in entries.iter().enumerate() {
        let read_entry = reader.read_entry(&mut cursor).unwrap();
        assert!(read_entry.is_some(), "Expected entry {i}");
        let read_entry = read_entry.unwrap();
        assert_eq!(read_entry.name(), expected.name());
        assert_eq!(read_entry.size(), expected.size());
    }

    assert!(reader.read_entry(&mut cursor).unwrap().is_none());
}

#[test]
fn stats_tracking() {
    let config = BatchConfig::new().with_max_entries(2);
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    for i in 0..5 {
        let entry = FileEntry::new_file(format!("file{i}.txt").into(), 100, 0o644);
        writer.add_entry(&mut output, &entry).unwrap();
    }

    writer.flush(&mut output).unwrap();

    assert_eq!(writer.stats().entries_written, 5);
    assert_eq!(writer.stats().batches_flushed, 3);
    assert_eq!(writer.stats().flushes_by_count, 2);
    assert_eq!(writer.stats().explicit_flushes, 1);
    assert!(writer.stats().bytes_written > 0);
}

#[test]
fn preserve_options_forwarding() {
    let writer = BatchedFileListWriter::new(test_protocol())
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_preserve_links(true)
        .with_preserve_devices(true)
        .with_preserve_specials(true)
        .with_preserve_hard_links(true)
        .with_preserve_atimes(true)
        .with_preserve_crtimes(true)
        .with_preserve_acls(true)
        .with_preserve_xattrs(true)
        .with_always_checksum(16);

    assert!(writer.is_empty());
}

#[test]
fn flush_empty_batch_is_noop() {
    let mut writer = BatchedFileListWriter::new(test_protocol());
    let mut output = Vec::new();

    writer.flush(&mut output).unwrap();

    assert!(output.is_empty());
    assert_eq!(writer.stats().batches_flushed, 0);
}

#[test]
fn check_timeout_flush() {
    let config = BatchConfig::new().with_flush_timeout(Duration::from_millis(100));
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.add_entry(&mut output, &entry).unwrap();

    // Before expiry: timeout has not elapsed yet
    assert!(!writer.check_timeout_flush(&mut output).unwrap());
    assert!(!writer.is_empty());

    // Simulate timeout expiry by backdating the batch start
    writer.expire_batch_timeout();

    assert!(writer.check_timeout_flush(&mut output).unwrap());
    assert!(writer.is_empty());
    assert_eq!(writer.stats().flushes_by_timeout, 1);
}

#[test]
fn check_timeout_flush_no_timeout_yet() {
    let config = BatchConfig::new().with_flush_timeout(Duration::from_secs(60));
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.add_entry(&mut output, &entry).unwrap();

    assert!(!writer.check_timeout_flush(&mut output).unwrap());
    assert!(!writer.is_empty());
}

#[test]
fn inner_access() {
    let mut writer = BatchedFileListWriter::new(test_protocol());

    let _inner = writer.inner();
    let _inner_mut = writer.inner_mut();
}

#[test]
fn into_inner_consumes_writer() {
    let config = BatchConfig::no_auto_flush();
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.add_entry(&mut output, &entry).unwrap();

    let _inner = writer.into_inner();
}

#[test]
fn with_compat_flags_creates_valid_writer() {
    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS | CompatibilityFlags::SAFE_FILE_LIST;

    let writer = BatchedFileListWriter::with_compat_flags(protocol, flags);
    assert!(writer.is_empty());
}

#[test]
fn auto_flush_on_byte_size_deterministic() {
    let config = BatchConfig::new().with_max_entries(1000).with_max_bytes(30);
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    let entry1 = FileEntry::new_file("short_file_name_01.txt".into(), 10, 0o644);
    let entry2 = FileEntry::new_file("short_file_name_02.txt".into(), 20, 0o644);
    let entry3 = FileEntry::new_file("short_file_name_03.txt".into(), 30, 0o644);
    let entry4 = FileEntry::new_file("short_file_name_04.txt".into(), 40, 0o644);

    writer.add_entry(&mut output, &entry1).unwrap();
    writer.add_entry(&mut output, &entry2).unwrap();
    writer.add_entry(&mut output, &entry3).unwrap();
    writer.add_entry(&mut output, &entry4).unwrap();

    assert!(
        writer.stats().flushes_by_size >= 1,
        "Expected byte size flush to trigger, got flushes_by_size={}",
        writer.stats().flushes_by_size
    );
    assert!(!output.is_empty());
}

#[test]
fn large_entry_exceeding_buffer_size() {
    let config = BatchConfig::new().with_max_entries(1000).with_max_bytes(10);
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    let large_entry = FileEntry::new_file(
        "this_is_a_very_long_filename_that_exceeds_buffer_size.txt".into(),
        1000000,
        0o644,
    );

    let flushed = writer.add_entry(&mut output, &large_entry).unwrap();

    assert!(flushed, "Large entry should trigger immediate flush");
    assert!(writer.is_empty(), "Buffer should be empty after flush");
    assert!(
        !output.is_empty(),
        "Output should contain the flushed entry"
    );
    assert_eq!(writer.stats().entries_written, 1);
    assert_eq!(writer.stats().batches_flushed, 1);
    assert_eq!(writer.stats().flushes_by_size, 1);
}

#[test]
fn multiple_large_entries_each_triggers_flush() {
    let config = BatchConfig::new().with_max_entries(1000).with_max_bytes(10);
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    for i in 0..3 {
        let entry = FileEntry::new_file(
            format!("very_long_filename_that_exceeds_buffer_{i}.txt").into(),
            1000 * i as u64,
            0o644,
        );
        let flushed = writer.add_entry(&mut output, &entry).unwrap();
        assert!(flushed, "Entry {i} should trigger flush due to size");
    }

    assert_eq!(writer.stats().entries_written, 3);
    assert_eq!(writer.stats().batches_flushed, 3);
    assert_eq!(writer.stats().flushes_by_size, 3);
}

#[test]
fn stats_track_flushes_by_size_correctly() {
    let config = BatchConfig::new().with_max_entries(100).with_max_bytes(50);
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    let mut size_flushes = 0u64;
    for i in 0..10 {
        let entry = FileEntry::new_file(format!("file{i}.txt").into(), 100, 0o644);
        if writer.add_entry(&mut output, &entry).unwrap() {
            size_flushes += 1;
        }
    }

    assert_eq!(writer.stats().flushes_by_size, size_flushes);
    assert_eq!(writer.stats().flushes_by_count, 0);
    assert_eq!(writer.stats().flushes_by_timeout, 0);
    assert_eq!(writer.stats().explicit_flushes, 0);

    writer.flush(&mut output).unwrap();

    assert_eq!(writer.stats().flushes_by_size, size_flushes);
    assert_eq!(writer.stats().explicit_flushes, 1);
    assert!(writer.stats().batches_flushed > 0);
}

#[test]
fn mixed_flush_types_tracked_separately() {
    let config = BatchConfig::new().with_max_entries(2).with_max_bytes(1000);
    let mut writer = BatchedFileListWriter::with_config(test_protocol(), config);
    let mut output = Vec::new();

    let entry1 = FileEntry::new_file("a.txt".into(), 10, 0o644);
    let entry2 = FileEntry::new_file("b.txt".into(), 20, 0o644);
    writer.add_entry(&mut output, &entry1).unwrap();
    writer.add_entry(&mut output, &entry2).unwrap();

    assert_eq!(writer.stats().flushes_by_count, 1);
    assert_eq!(writer.stats().flushes_by_size, 0);

    let entry3 = FileEntry::new_file("c.txt".into(), 30, 0o644);
    writer.add_entry(&mut output, &entry3).unwrap();
    writer.flush(&mut output).unwrap();

    assert_eq!(writer.stats().flushes_by_count, 1);
    assert_eq!(writer.stats().flushes_by_size, 0);
    assert_eq!(writer.stats().explicit_flushes, 1);
    assert_eq!(writer.stats().batches_flushed, 2);
}
