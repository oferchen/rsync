//! File-list surface: receiving raw and incremental file lists, sender
//! attribute decoding, sum-head wire format, NDX-segment conversion, id
//! lists, directory creation, the receiver-side filter chain that gates
//! deletions, and the delete-pipeline hook fed by INC_RECURSE segments.
//!
// TODO: decompose further - this file groups several related surfaces and
// still exceeds the 650-line cap. A follow-up should split it into
// per-surface modules (e.g. `wire_attrs`, `incremental_receiver`,
// `id_lists`, `ndx_convert`, `delete_pipeline_hook`, `filter_chain`).

use std::ffi::OsString;
use std::io::{self, Cursor, Read, Write};
use std::path::PathBuf;

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;
use protocol::stats::DeleteStats;

use super::super::ReceiverContext;
use super::super::directory::FailedDirectories;
use super::super::stats::TransferStats;
use super::super::wire::{SenderAttrs, SumHead};
use super::support::{
    TestDeletionWriter, config_with_flags, test_config, test_handshake,
    test_handshake_with_protocol,
};
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::pipeline::PipelineConfig;
use crate::role::ServerRole;

#[test]
fn receiver_context_creation() {
    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    assert_eq!(ctx.protocol().as_u8(), 32);
    assert!(ctx.file_list().is_empty());
}

#[test]
fn receiver_empty_file_list() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Empty file list (just the end marker)
    let data = [0u8];
    let mut cursor = Cursor::new(&data[..]);

    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert!(ctx.file_list().is_empty());
}

#[test]
fn receiver_single_file() {
    use protocol::flist::{FileEntry, FileListWriter};

    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Create a proper file list using FileListWriter for protocol 32
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(handshake.protocol);

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.write_entry(&mut data, &entry).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let count = ctx.receive_file_list(&mut cursor).unwrap();

    assert_eq!(count, 1);
    assert_eq!(ctx.file_list().len(), 1);
    assert_eq!(ctx.file_list()[0].name(), "test.txt");
}

#[test]
fn sum_head_new_creates_with_correct_values() {
    let sum_head = SumHead::new(100, 1024, 16, 512);
    assert_eq!(sum_head.count, 100);
    assert_eq!(sum_head.blength, 1024);
    assert_eq!(sum_head.s2length, 16);
    assert_eq!(sum_head.remainder, 512);
}

#[test]
fn sum_head_empty_creates_zero_values() {
    let sum_head = SumHead::empty();
    assert_eq!(sum_head.count, 0);
    assert_eq!(sum_head.blength, 0);
    assert_eq!(sum_head.s2length, 0);
    assert_eq!(sum_head.remainder, 0);
    assert!(sum_head.is_empty());
}

#[test]
fn sum_head_default_is_empty() {
    let sum_head = SumHead::default();
    assert!(sum_head.is_empty());
    assert_eq!(sum_head, SumHead::empty());
}

#[test]
fn sum_head_is_empty_false_for_nonzero_count() {
    let sum_head = SumHead::new(1, 1024, 16, 0);
    assert!(!sum_head.is_empty());
}

#[test]
fn sum_head_write_produces_correct_wire_format() {
    let sum_head = SumHead::new(10, 700, 16, 100);
    let mut output = Vec::new();
    sum_head.write(&mut output).unwrap();

    assert_eq!(output.len(), 16);
    // All values as 32-bit little-endian
    assert_eq!(
        i32::from_le_bytes([output[0], output[1], output[2], output[3]]),
        10
    );
    assert_eq!(
        i32::from_le_bytes([output[4], output[5], output[6], output[7]]),
        700
    );
    assert_eq!(
        i32::from_le_bytes([output[8], output[9], output[10], output[11]]),
        16
    );
    assert_eq!(
        i32::from_le_bytes([output[12], output[13], output[14], output[15]]),
        100
    );
}

#[test]
fn sum_head_read_parses_wire_format() {
    // Prepare wire data: count=5, blength=512, s2length=16, remainder=128
    let mut data = Vec::new();
    data.extend_from_slice(&5i32.to_le_bytes());
    data.extend_from_slice(&512i32.to_le_bytes());
    data.extend_from_slice(&16i32.to_le_bytes());
    data.extend_from_slice(&128i32.to_le_bytes());

    let sum_head = SumHead::read(&mut Cursor::new(data)).unwrap();

    assert_eq!(sum_head.count, 5);
    assert_eq!(sum_head.blength, 512);
    assert_eq!(sum_head.s2length, 16);
    assert_eq!(sum_head.remainder, 128);
}

#[test]
fn sum_head_round_trip() {
    let original = SumHead::new(100, 1024, 20, 256);

    let mut buf = Vec::new();
    original.write(&mut buf).unwrap();

    let decoded = SumHead::read(&mut Cursor::new(buf)).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn sum_head_read_insufficient_data() {
    // Only 8 bytes instead of 16
    let data = vec![0u8; 8];
    let result = SumHead::read(&mut Cursor::new(data));
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn sender_attrs_read_protocol_28_returns_default_iflags() {
    // Protocol 28 just reads the NDX byte, no iflags
    let data = vec![0x05u8]; // NDX byte only
    let attrs = SenderAttrs::read(&mut Cursor::new(data), 28).unwrap();

    assert_eq!(attrs.iflags, SenderAttrs::ITEM_TRANSFER);
    assert!(attrs.fnamecmp_type.is_none());
    assert!(attrs.xname.is_none());
}

#[test]
fn sender_attrs_read_protocol_29_parses_iflags() {
    // NDX byte + iflags (0x8000 = ITEM_TRANSFER)
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x8000u16.to_le_bytes()); // iflags

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x8000);
    assert!(attrs.fnamecmp_type.is_none());
    assert!(attrs.xname.is_none());
}

#[test]
fn sender_attrs_read_with_basis_type() {
    // NDX byte + iflags (0x8800 = ITEM_TRANSFER | ITEM_BASIS_TYPE_FOLLOWS) + fnamecmp_type
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x8800u16.to_le_bytes()); // iflags with BASIS_TYPE_FOLLOWS
    data.push(0x02); // fnamecmp_type = BasisDir(2)

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x8800);
    assert_eq!(
        attrs.fnamecmp_type,
        Some(protocol::FnameCmpType::BasisDir(2))
    );
    assert!(attrs.xname.is_none());
}

#[test]
fn sender_attrs_read_with_short_xname() {
    // NDX byte + iflags (0x9000 = ITEM_TRANSFER | ITEM_XNAME_FOLLOWS) + xname
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x9000u16.to_le_bytes()); // iflags with XNAME_FOLLOWS
    data.push(0x04); // xname length (short form)
    data.extend_from_slice(b"test"); // xname content

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x9000);
    assert!(attrs.fnamecmp_type.is_none());
    assert_eq!(attrs.xname, Some(b"test".to_vec()));
}

#[test]
fn sender_attrs_read_with_long_xname() {
    // NDX + iflags + xname with extended length (> 127 bytes requires 2-byte length)
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x9000u16.to_le_bytes()); // iflags with XNAME_FOLLOWS
    // Length 300 = 0x80 | (300 / 256) = 0x81, then 300 % 256 = 44
    data.push(0x81); // High byte: 0x80 flag + 1
    data.push(0x2C); // Low byte: 44 (1*256 + 44 = 300)
    data.extend(vec![b'x'; 300]); // xname content (300 'x' characters)

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x9000);
    assert!(attrs.fnamecmp_type.is_none());
    assert_eq!(attrs.xname.as_ref().unwrap().len(), 300);
}

#[test]
fn sender_attrs_read_empty_returns_eof_error() {
    let data: Vec<u8> = vec![];
    let result = SenderAttrs::read(&mut Cursor::new(data), 29);

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn sender_attrs_constants_match_upstream() {
    // Verify our constants match upstream rsync.h values
    assert_eq!(SenderAttrs::ITEM_TRANSFER, 0x8000);
    assert_eq!(SenderAttrs::ITEM_BASIS_TYPE_FOLLOWS, 0x0800);
    assert_eq!(SenderAttrs::ITEM_XNAME_FOLLOWS, 0x1000);
}

#[test]
fn sender_attrs_read_with_codec_protocol_30_delta_encoded() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Simulate sender encoding NDX 0 for protocol 30+
    // With prev_positive=-1, ndx=0, diff=1, encoded as single byte 0x01
    let mut sender_codec = create_ndx_codec(31);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, 0).unwrap();
    // Add iflags (ITEM_TRANSFER = 0x8000)
    wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

    // Receiver reads with its own codec
    let mut receiver_codec = create_ndx_codec(31);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, 0);
    assert_eq!(attrs.iflags, 0x8000);
}

#[test]
fn sender_attrs_read_with_codec_protocol_30_sequential_indices() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Simulate sender sending sequential indices 0, 1, 2
    let mut sender_codec = create_ndx_codec(31);
    let mut wire_data = Vec::new();
    for ndx in 0..3 {
        sender_codec.write_ndx(&mut wire_data, ndx).unwrap();
        wire_data.extend_from_slice(&0x8000u16.to_le_bytes());
    }

    // Receiver reads all three
    let mut receiver_codec = create_ndx_codec(31);
    let mut cursor = Cursor::new(&wire_data);

    for expected_ndx in 0..3 {
        let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();
        assert_eq!(ndx, expected_ndx, "expected NDX {expected_ndx}");
        assert_eq!(attrs.iflags, 0x8000);
    }
}

#[test]
fn sender_attrs_read_with_codec_legacy_protocol_29() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Protocol 29 uses 4-byte LE NDX
    let mut sender_codec = create_ndx_codec(29);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, 42).unwrap();
    // Add iflags
    wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

    let mut receiver_codec = create_ndx_codec(29);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, 42);
    assert_eq!(attrs.iflags, 0x8000);
}

#[test]
fn sender_attrs_read_with_codec_protocol_28_no_iflags() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Protocol 28: 4-byte LE NDX, no iflags
    let mut sender_codec = create_ndx_codec(28);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, 5).unwrap();
    // No iflags for protocol < 29

    let mut receiver_codec = create_ndx_codec(28);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, 5);
    // Default iflags for protocol < 29
    assert_eq!(attrs.iflags, SenderAttrs::ITEM_TRANSFER);
}

#[test]
fn sender_attrs_read_with_codec_large_index() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Test with a large index that requires extended encoding in protocol 30+
    let large_index = 50000;

    let mut sender_codec = create_ndx_codec(31);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, large_index).unwrap();
    wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

    let mut receiver_codec = create_ndx_codec(31);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, large_index);
    assert_eq!(attrs.iflags, 0x8000);
}

#[test]
fn receive_id_lists_skips_when_numeric_ids_true() {
    let handshake = test_handshake();
    let config = config_with_flags(true, true, true);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // With numeric_ids=true, no data should be read even with owner/group set
    let data: &[u8] = &[];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    // Cursor position unchanged - nothing read
    assert_eq!(cursor.position(), 0);
}

#[test]
fn receive_id_lists_reads_uid_list_when_owner_set() {
    let handshake = test_handshake();
    let config = config_with_flags(true, false, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Empty UID list: varint 0 terminator only
    let data: &[u8] = &[0];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 1);
}

#[test]
fn receive_id_lists_reads_gid_list_when_group_set() {
    let handshake = test_handshake();
    let config = config_with_flags(false, true, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Empty GID list: varint 0 terminator only
    let data: &[u8] = &[0];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 1);
}

#[test]
fn receive_id_lists_reads_both_when_owner_and_group_set() {
    let handshake = test_handshake();
    let config = config_with_flags(true, true, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Both lists: two varint 0 terminators
    let data: &[u8] = &[0, 0];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 2);
}

#[test]
fn receive_id_lists_skips_both_when_neither_flag_set() {
    let handshake = test_handshake();
    let config = config_with_flags(false, false, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    let data: &[u8] = &[];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 0);
}

#[test]
fn incremental_receiver_reads_entries() {
    // Create test data with a simple file list
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    // Add a directory and a file
    let dir = FileEntry::new_directory("testdir".into(), 0o755);
    let file = FileEntry::new_file("testdir/file.txt".into(), 100, 0o644);

    writer.write_entry(&mut data, &dir).unwrap();
    writer.write_entry(&mut data, &file).unwrap();
    writer.write_end(&mut data, None).unwrap();

    // Create handshake and config
    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    // Create incremental receiver
    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // First entry should be the directory (it has no parent dependency)
    let entry1 = receiver.next_ready().unwrap().unwrap();
    assert!(entry1.is_dir());
    assert_eq!(entry1.name(), "testdir");

    // Second entry should be the file (parent dir now exists)
    let entry2 = receiver.next_ready().unwrap().unwrap();
    assert!(entry2.is_file());
    assert_eq!(entry2.name(), "testdir/file.txt");

    // No more entries
    assert!(receiver.next_ready().unwrap().is_none());
    assert!(receiver.is_empty());
    assert_eq!(receiver.entries_read(), 2);
}

#[test]
fn incremental_receiver_handles_empty_list() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let writer = protocol::flist::FileListWriter::new(protocol);
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    assert!(receiver.next_ready().unwrap().is_none());
    assert!(receiver.is_empty());
    assert_eq!(receiver.entries_read(), 0);
}

#[test]
fn incremental_receiver_collect_sorted() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    // Add entries in random order
    let file1 = FileEntry::new_file("z_file.txt".into(), 50, 0o644);
    let file2 = FileEntry::new_file("a_file.txt".into(), 100, 0o644);
    let dir = FileEntry::new_directory("m_dir".into(), 0o755);

    writer.write_entry(&mut data, &file1).unwrap();
    writer.write_entry(&mut data, &file2).unwrap();
    writer.write_entry(&mut data, &dir).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    let receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // collect_sorted should return entries in sorted order
    let entries = receiver.collect_sorted().unwrap();
    assert_eq!(entries.len(), 3);

    // Files should come before directories at the same level
    assert_eq!(entries[0].name(), "a_file.txt");
    assert_eq!(entries[1].name(), "z_file.txt");
    assert_eq!(entries[2].name(), "m_dir");
}

#[test]
fn incremental_receiver_iterator_interface() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    let file = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.write_entry(&mut data, &file).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    let receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // Use iterator interface
    let entries: Vec<_> = receiver.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name(), "test.txt");
}

#[test]
fn incremental_receiver_mark_directory_created() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    // Add only a nested file (no directory entry)
    let file = FileEntry::new_file("existing/nested.txt".into(), 100, 0o644);
    writer.write_entry(&mut data, &file).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // Mark the parent directory as already created
    receiver.mark_directory_created("existing");

    // Now the nested file should be immediately ready
    let entry = receiver.next_ready().unwrap().unwrap();
    assert_eq!(entry.name(), "existing/nested.txt");
}

#[test]
fn transfer_stats_has_incremental_fields() {
    let stats = TransferStats {
        files_listed: 0,
        files_transferred: 0,
        bytes_received: 0,
        bytes_sent: 0,
        total_source_bytes: 0,
        metadata_errors: vec![],
        io_error: 0,
        error_count: 0,
        entries_received: 100,
        directories_created: 10,
        directories_failed: 2,
        files_skipped: 5,
        delete_stats: DeleteStats::new(),
        delete_limit_exceeded: false,
        literal_data: 0,
        matched_data: 0,
        redo_count: 0,
    };

    assert_eq!(stats.entries_received, 100);
    assert_eq!(stats.directories_created, 10);
    assert_eq!(stats.directories_failed, 2);
    assert_eq!(stats.files_skipped, 5);
}

mod incremental_receiver_tests {
    use super::*;

    /// Helper: create wire-encoded file list data from entries.
    fn encode_entries(entries: &[FileEntry]) -> Vec<u8> {
        let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
        let mut data = Vec::new();
        let mut writer = protocol::flist::FileListWriter::new(protocol);

        for entry in entries {
            writer.write_entry(&mut data, entry).unwrap();
        }
        writer.write_end(&mut data, None).unwrap();

        data
    }

    /// Helper: create an `IncrementalFileListReceiver` from raw wire data.
    fn make_receiver(
        data: Vec<u8>,
    ) -> super::super::super::IncrementalFileListReceiver<Cursor<Vec<u8>>> {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);
        ctx.incremental_file_list_receiver(Cursor::new(data))
    }

    #[test]
    fn try_read_one_returns_false_when_finished() {
        // Create a receiver that's already marked as finished
        let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
        let flist_reader = protocol::flist::FileListReader::new(protocol);

        // Empty data - will hit EOF immediately
        let empty_data: Vec<u8> = vec![0]; // Single zero byte = end of list marker
        let source = Cursor::new(empty_data);

        let incremental = protocol::flist::IncrementalFileList::new();

        let mut receiver = super::super::super::IncrementalFileListReceiver {
            flist_reader,
            source,
            incremental,
            finished_reading: true, // Already finished
            entries_read: 0,
            use_qsort: false,
        };

        // Should return false since already finished
        assert!(!receiver.try_read_one().unwrap());
    }

    #[test]
    fn try_read_one_on_empty_list_returns_false() {
        // An empty file list (only the end-of-list marker) should
        // cause try_read_one to hit EOF and return false.
        let data = encode_entries(&[]);
        let mut receiver = make_receiver(data);

        assert!(!receiver.try_read_one().unwrap());
        assert!(receiver.is_finished_reading());
        assert_eq!(receiver.entries_read(), 0);
    }

    #[test]
    fn try_read_one_reads_single_entry() {
        let file = FileEntry::new_file("hello.txt".into(), 42, 0o644);
        let data = encode_entries(&[file]);
        let mut receiver = make_receiver(data);

        // First call reads one entry
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 1);
        assert_eq!(receiver.ready_count(), 1);
        assert!(!receiver.is_finished_reading());

        // The entry should be available via pop / next_ready
        let entry = receiver.next_ready().unwrap().unwrap();
        assert_eq!(entry.name(), "hello.txt");
        assert_eq!(entry.size(), 42);
    }

    #[test]
    fn try_read_one_reads_entries_one_at_a_time() {
        let entries = vec![
            FileEntry::new_file("a.txt".into(), 10, 0o644),
            FileEntry::new_file("b.txt".into(), 20, 0o644),
            FileEntry::new_file("c.txt".into(), 30, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read one at a time
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 1);
        assert_eq!(receiver.ready_count(), 1);

        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 2);
        assert_eq!(receiver.ready_count(), 2);

        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 3);
        assert_eq!(receiver.ready_count(), 3);

        // Next call hits end-of-list
        assert!(!receiver.try_read_one().unwrap());
        assert!(receiver.is_finished_reading());

        // All three entries should be ready
        let names: Vec<String> = std::iter::from_fn(|| receiver.next_ready().ok().flatten())
            .map(|e| e.name().to_string())
            .collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn try_read_one_after_eof_is_idempotent() {
        let data = encode_entries(&[FileEntry::new_file("only.txt".into(), 1, 0o644)]);
        let mut receiver = make_receiver(data);

        // Read the single entry
        assert!(receiver.try_read_one().unwrap());
        // Hit EOF
        assert!(!receiver.try_read_one().unwrap());
        // Subsequent calls continue to return false
        assert!(!receiver.try_read_one().unwrap());
        assert!(!receiver.try_read_one().unwrap());
        assert!(receiver.is_finished_reading());
    }

    #[test]
    fn try_read_one_child_before_parent_stays_pending() {
        // Child file arrives before its parent directory.
        // try_read_one should add it to pending, not ready.
        let entries = vec![
            FileEntry::new_file("subdir/child.txt".into(), 100, 0o644),
            FileEntry::new_directory("subdir".into(), 0o755),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read child first - goes to pending since "subdir" doesn't exist
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 1);
        assert_eq!(receiver.ready_count(), 0);
        assert_eq!(receiver.pending_count(), 1);

        // Read parent directory - should release child too
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 2);
        assert_eq!(receiver.ready_count(), 2); // dir + file
        assert_eq!(receiver.pending_count(), 0);
    }

    #[test]
    fn try_read_one_with_pre_marked_directory() {
        // Mark a directory as created before reading. A child entry
        // should become immediately ready.
        let entries = vec![FileEntry::new_file("existing/file.txt".into(), 50, 0o644)];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        receiver.mark_directory_created("existing");

        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 1);
        assert_eq!(receiver.pending_count(), 0);

        let entry = receiver.next_ready().unwrap().unwrap();
        assert_eq!(entry.name(), "existing/file.txt");
    }

    #[test]
    fn try_read_one_deeply_nested_out_of_order() {
        // Push entries in reverse depth order, then verify resolution.
        let entries = vec![
            FileEntry::new_file("a/b/c/deep.txt".into(), 1, 0o644),
            FileEntry::new_directory("a/b/c".into(), 0o755),
            FileEntry::new_directory("a/b".into(), 0o755),
            FileEntry::new_directory("a".into(), 0o755),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read deep file - pending (no ancestors)
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 0);
        assert_eq!(receiver.pending_count(), 1);

        // Read "a/b/c" - pending (parent "a/b" missing)
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 0);
        assert_eq!(receiver.pending_count(), 2);

        // Read "a/b" - pending (parent "a" missing)
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 0);
        assert_eq!(receiver.pending_count(), 3);

        // Read "a" - cascading release: a -> a/b -> a/b/c -> deep.txt
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 4);
        assert_eq!(receiver.pending_count(), 0);
    }

    #[test]
    fn try_read_one_interleaved_with_next_ready() {
        let entries = vec![
            FileEntry::new_file("first.txt".into(), 1, 0o644),
            FileEntry::new_file("second.txt".into(), 2, 0o644),
            FileEntry::new_file("third.txt".into(), 3, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read one, consume it, read next
        assert!(receiver.try_read_one().unwrap());
        let e1 = receiver.next_ready().unwrap().unwrap();
        assert_eq!(e1.name(), "first.txt");
        assert_eq!(receiver.ready_count(), 0);

        assert!(receiver.try_read_one().unwrap());
        let e2 = receiver.next_ready().unwrap().unwrap();
        assert_eq!(e2.name(), "second.txt");

        assert!(receiver.try_read_one().unwrap());
        let e3 = receiver.next_ready().unwrap().unwrap();
        assert_eq!(e3.name(), "third.txt");

        // No more
        assert!(!receiver.try_read_one().unwrap());
        assert!(receiver.next_ready().unwrap().is_none());
    }

    #[test]
    fn try_read_one_interleaved_with_drain_ready() {
        let entries = vec![
            FileEntry::new_file("x.txt".into(), 1, 0o644),
            FileEntry::new_file("y.txt".into(), 2, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read both entries
        assert!(receiver.try_read_one().unwrap());
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 2);

        // Drain all at once
        let drained = receiver.drain_ready();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].name(), "x.txt");
        assert_eq!(drained[1].name(), "y.txt");
        assert_eq!(receiver.ready_count(), 0);

        // EOF
        assert!(!receiver.try_read_one().unwrap());
    }

    #[test]
    fn try_read_one_directory_and_children() {
        let entries = vec![
            FileEntry::new_directory("mydir".into(), 0o755),
            FileEntry::new_file("mydir/alpha.txt".into(), 10, 0o644),
            FileEntry::new_file("mydir/beta.txt".into(), 20, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read directory
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 1);

        // Read children - they should be immediately ready since parent exists
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 2);

        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 3);

        // Verify order
        let names: Vec<String> = std::iter::from_fn(|| receiver.next_ready().ok().flatten())
            .map(|e| e.name().to_string())
            .collect();
        assert_eq!(names, vec!["mydir", "mydir/alpha.txt", "mydir/beta.txt"]);
    }

    #[test]
    fn try_read_one_is_empty_tracks_state_correctly() {
        let entries = vec![FileEntry::new_file("f.txt".into(), 1, 0o644)];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Not empty initially (haven't read yet, not finished)
        assert!(!receiver.is_finished_reading());

        // Read the entry
        assert!(receiver.try_read_one().unwrap());
        // Not empty: still has a ready entry
        assert!(!receiver.is_empty());

        // Hit EOF
        assert!(!receiver.try_read_one().unwrap());
        // Still not empty: one ready entry remains
        assert!(!receiver.is_empty());

        // Consume the entry
        receiver.next_ready().unwrap();
        // Now truly empty
        assert!(receiver.is_empty());
    }

    #[test]
    fn try_read_one_reads_symlink_entry() {
        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.links = true;
        let ctx = ReceiverContext::new(&handshake, config);

        // Encode a symlink entry with links preserved
        let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
        let mut data = Vec::new();
        let mut writer = protocol::flist::FileListWriter::new(protocol);
        writer = writer.with_preserve_links(true);

        let symlink = FileEntry::new_symlink("link.txt".into(), "/target".into());
        writer.write_entry(&mut data, &symlink).unwrap();
        writer.write_end(&mut data, None).unwrap();

        let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(data));

        assert!(receiver.try_read_one().unwrap());
        let entry = receiver.next_ready().unwrap().unwrap();
        assert!(entry.is_symlink());
        assert_eq!(entry.name(), "link.txt");
    }

    #[test]
    fn try_read_one_increments_entries_read() {
        let entries = vec![
            FileEntry::new_file("one.txt".into(), 1, 0o644),
            FileEntry::new_file("two.txt".into(), 2, 0o644),
            FileEntry::new_file("three.txt".into(), 3, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        assert_eq!(receiver.entries_read(), 0);

        receiver.try_read_one().unwrap();
        assert_eq!(receiver.entries_read(), 1);

        receiver.try_read_one().unwrap();
        assert_eq!(receiver.entries_read(), 2);

        receiver.try_read_one().unwrap();
        assert_eq!(receiver.entries_read(), 3);

        // EOF does not increment
        receiver.try_read_one().unwrap();
        assert_eq!(receiver.entries_read(), 3);
    }

    #[test]
    fn try_read_one_partial_then_collect_sorted() {
        let entries = vec![
            FileEntry::new_file("z.txt".into(), 1, 0o644),
            FileEntry::new_file("a.txt".into(), 2, 0o644),
            FileEntry::new_file("m.txt".into(), 3, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read one entry via try_read_one
        assert!(receiver.try_read_one().unwrap());
        // Consume it so it doesn't appear in collect_sorted's drain
        let first = receiver.next_ready().unwrap().unwrap();
        assert_eq!(first.name(), "z.txt");

        // Now collect the remaining entries sorted
        let sorted = receiver.collect_sorted().unwrap();
        assert_eq!(sorted.len(), 2);
        // "a.txt" should come before "m.txt" after sorting
        assert_eq!(sorted[0].name(), "a.txt");
        assert_eq!(sorted[1].name(), "m.txt");
    }

    #[test]
    fn mark_finished_prevents_further_reads() {
        let entries = vec![
            FileEntry::new_file("a.txt".into(), 1, 0o644),
            FileEntry::new_file("b.txt".into(), 2, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read one entry
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 1);

        // Mark as finished (simulating error recovery)
        receiver.mark_finished();

        // try_read_one should now return false even though data remains
        assert!(!receiver.try_read_one().unwrap());
        assert!(receiver.is_finished_reading());
        assert_eq!(receiver.entries_read(), 1);
    }

    #[test]
    fn try_read_one_stats_are_accessible() {
        let entries = vec![FileEntry::new_file("stat_test.txt".into(), 999, 0o644)];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        assert!(receiver.try_read_one().unwrap());
        // Stats should reflect one regular file read
        let stats = receiver.stats();
        assert_eq!(stats.num_files, 1);
        assert_eq!(stats.total_size, 999);
    }
}

#[test]
fn run_pipelined_incremental_compiles() {
    // This test just verifies the method signature is correct
    fn _check_signature<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        ctx: &mut ReceiverContext,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
    ) {
        let _ = ctx.run_pipelined_incremental(reader, writer, PipelineConfig::default(), None);
    }
}

mod create_directory_incremental_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creates_directory_successfully() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        let entry = FileEntry::new_directory("subdir".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        let result = ctx.create_directory_incremental(dest, &entry, &opts, &mut failed, None);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(true)); // Returns Some(true) for new dir
        assert!(dest.join("subdir").exists());
        assert_eq!(failed.count(), 0);
    }

    #[test]
    fn skips_child_of_failed_parent() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        let entry = FileEntry::new_directory("failed_parent/child".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();
        failed.mark_failed("failed_parent");

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        let result = ctx.create_directory_incremental(dest, &entry, &opts, &mut failed, None);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None); // Returns None for skipped
        assert!(!dest.join("failed_parent/child").exists());
        assert_eq!(failed.count(), 2); // Parent + child marked as failed
    }
}

#[cfg(feature = "incremental-flist")]
mod incremental_mode_tests {
    use super::super::support::PHASE1_CHECKSUM_LENGTH;
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn failed_directories_skips_nested_children() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("a/b");

        // Direct child
        assert!(failed.failed_ancestor("a/b/file.txt").is_some());
        // Nested child
        assert!(failed.failed_ancestor("a/b/c/d/file.txt").is_some());
        // Sibling - not affected
        assert!(failed.failed_ancestor("a/c/file.txt").is_none());
        // Parent - not affected
        assert!(failed.failed_ancestor("a/file.txt").is_none());
    }

    #[test]
    fn failed_directories_handles_root_level() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("toplevel");

        assert!(failed.failed_ancestor("toplevel/sub/file.txt").is_some());
        assert!(failed.failed_ancestor("other/file.txt").is_none());
    }

    #[test]
    fn stats_tracks_incremental_fields() {
        let stats = TransferStats {
            entries_received: 100,
            directories_created: 20,
            directories_failed: 2,
            files_skipped: 10,
            files_transferred: 68,
            ..Default::default()
        };

        // Verify consistency
        assert_eq!(
            stats.directories_created + stats.directories_failed,
            22 // total directories
        );
    }

    #[test]
    fn create_directory_incremental_nested() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        // Create nested directory
        let entry = FileEntry::new_directory("a/b/c".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        let result = ctx.create_directory_incremental(dest, &entry, &opts, &mut failed, None);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(true));
        assert!(dest.join("a/b/c").exists());
    }

    #[test]
    fn failed_directories_propagates_to_deeply_nested() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("level1");

        // All descendants should be affected
        assert!(failed.failed_ancestor("level1/level2").is_some());
        assert!(failed.failed_ancestor("level1/level2/level3").is_some());
        assert!(
            failed
                .failed_ancestor("level1/level2/level3/file.txt")
                .is_some()
        );
    }

    #[test]
    fn checksum_length_phase1_equals_short_sum_length() {
        assert_eq!(
            PHASE1_CHECKSUM_LENGTH.get(),
            signature::block_size::SHORT_SUM_LENGTH,
        );
        assert_eq!(PHASE1_CHECKSUM_LENGTH.get(), 2);
    }

    #[test]
    fn checksum_length_redo_equals_max_sum_length() {
        assert_eq!(
            super::super::super::REDO_CHECKSUM_LENGTH.get(),
            signature::block_size::MAX_SUM_LENGTH,
        );
        assert_eq!(super::super::super::REDO_CHECKSUM_LENGTH.get(), 16);
    }

    #[test]
    fn checksum_length_phase1_less_than_redo() {
        assert!(PHASE1_CHECKSUM_LENGTH < super::super::super::REDO_CHECKSUM_LENGTH);
    }

    #[test]
    fn transfer_stats_default_values() {
        let stats = TransferStats::default();

        assert_eq!(stats.entries_received, 0);
        assert_eq!(stats.directories_created, 0);
        assert_eq!(stats.directories_failed, 0);
        assert_eq!(stats.files_skipped, 0);
        assert_eq!(stats.files_transferred, 0);
        assert_eq!(stats.bytes_received, 0);
    }
}

#[test]
fn receiver_filter_chain_protects_from_deletion() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    // Create files at destination (extra files that should be deleted)
    std::fs::write(dest.join("normal.txt"), b"delete me").unwrap();
    std::fs::write(dest.join("protected.conf"), b"keep me").unwrap();
    std::fs::write(dest.join("source.txt"), b"from sender").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new(&handshake, config);

    // File list includes "." and "source.txt" - anything else at dest is extraneous
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("source.txt".into(), 11, 0o644));

    // Set up filter chain with protect rule for *.conf
    let global =
        ::filters::FilterSet::from_rules([::filters::FilterRule::protect("*.conf")]).unwrap();
    ctx.set_filter_chain(::filters::FilterChain::new(global));

    let mut writer = TestDeletionWriter;
    let (stats, _) = ctx.delete_extraneous_files(dest, &mut writer).unwrap();

    // normal.txt should be deleted (not in file list, not protected)
    assert!(
        !dest.join("normal.txt").exists(),
        "normal.txt should be deleted"
    );

    // protected.conf should survive due to protect rule
    assert!(
        dest.join("protected.conf").exists(),
        "protected.conf should be protected from deletion"
    );

    // source.txt should survive (it's in the file list)
    assert!(dest.join("source.txt").exists());

    assert!(stats.files >= 1); // At least normal.txt was deleted
}

#[test]
fn receiver_filter_chain_empty_allows_all_deletions() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("file1.txt"), b"data1").unwrap();
    std::fs::write(dest.join("file2.log"), b"data2").unwrap();
    std::fs::write(dest.join("keep.txt"), b"keep").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new(&handshake, config);

    // File list has "." and "keep.txt" - file1/file2 are extraneous
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("keep.txt".into(), 4, 0o644));

    // Empty filter chain - all deletions should proceed
    let mut writer = TestDeletionWriter;
    let (stats, _) = ctx.delete_extraneous_files(dest, &mut writer).unwrap();

    assert!(!dest.join("file1.txt").exists());
    assert!(!dest.join("file2.log").exists());
    assert!(dest.join("keep.txt").exists());
    assert_eq!(stats.files, 2);
}

#[test]
fn receiver_set_and_get_filter_chain() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Default filter chain should be empty
    assert!(ctx.filter_chain().is_empty());

    // Set a chain with rules
    let global =
        ::filters::FilterSet::from_rules([::filters::FilterRule::exclude("*.bak")]).unwrap();
    let chain = ::filters::FilterChain::new(global);
    ctx.set_filter_chain(chain);

    assert!(!ctx.filter_chain().is_empty());
}

// Protocol 28/29 io_error after file list (flist.c:2738-2742)

/// Verifies that `receive_file_list` reads the 4-byte LE io_error flag
/// after the file list end marker for protocol < 30.
///
/// upstream: flist.c:2738-2742 - the sender writes `write_int(f, io_error)`
/// after the id lists. Without this read, subsequent wire data is misaligned,
/// causing "received request to transfer non-regular file" errors.
#[test]
fn receive_file_list_reads_io_error_for_proto28() {
    let handshake = test_handshake_with_protocol(28);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(28u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Wire bytes: 0x00 end marker + 4-byte LE io_error (value 3 = IOERR_GENERAL | IOERR_DEL_LIMIT)
    let io_error_value: i32 = 3;
    let mut wire = vec![0x00u8]; // end marker
    wire.extend_from_slice(&io_error_value.to_le_bytes());

    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0, "empty file list should have 0 entries");
    assert_eq!(
        ctx.flist_io_error, io_error_value,
        "io_error should be read from wire"
    );
}

/// Verifies that `receive_file_list` reads io_error for protocol 29 (also < 30).
#[test]
fn receive_file_list_reads_io_error_for_proto29() {
    let handshake = test_handshake_with_protocol(29);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(29u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Wire: end marker + io_error = 0 (no error)
    let mut wire = vec![0x00u8];
    wire.extend_from_slice(&0i32.to_le_bytes());

    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert_eq!(ctx.flist_io_error, 0, "zero io_error should not set field");
}

/// Verifies that protocol >= 30 does NOT read the 4-byte io_error (uses
/// MSG_IO_ERROR multiplexed frames instead).
#[test]
fn receive_file_list_skips_io_error_for_proto30() {
    let handshake = test_handshake_with_protocol(30);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(30u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Wire: just end marker, no io_error bytes. If the code tried to read
    // 4 more bytes it would fail with UnexpectedEof.
    let wire = vec![0x00u8];
    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert_eq!(ctx.flist_io_error, 0);
}

/// Verifies that `ignore_errors` prevents accumulating the io_error flag.
#[test]
fn receive_file_list_ignore_errors_suppresses_io_error() {
    let handshake = test_handshake_with_protocol(28);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(28u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: true,
            ..Default::default()
        },
        deletion: crate::config::DeletionConfig {
            ignore_errors: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Wire: end marker + io_error = 7
    let mut wire = vec![0x00u8];
    wire.extend_from_slice(&7i32.to_le_bytes());

    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        ctx.flist_io_error, 0,
        "ignore_errors should suppress io_error accumulation"
    );
}

#[test]
fn receiver_ndx_convert_call_counter_increments() {
    // INC_RECURSE diagnostic I4 (#2199): every flat_to_wire_ndx invocation
    // must bump the global call counter. The assertion uses >= because the
    // counter is shared across the process and other tests may run
    // concurrently.
    use super::super::ndx_convert_totals;

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    let (calls_before, _) = ndx_convert_totals();

    let _ = ctx.flat_to_wire_ndx(0);
    let _ = ctx.flat_to_wire_ndx(0);
    let _ = ctx.flat_to_wire_ndx(0);

    let (calls_after, _) = ndx_convert_totals();
    assert!(
        calls_after >= calls_before + 3,
        "expected at least 3 new ndx_convert calls (before={calls_before}, after={calls_after})"
    );
}

#[test]
fn receiver_ndx_convert_partition_point_depth_grows() {
    // INC_RECURSE diagnostic I4 (#2199): the cumulative partition_point depth
    // must monotonically grow as the segment table is queried. A 4-segment
    // table contributes at least depth(4)=3 per call. Uses >= because the
    // counter is shared across the process.
    use super::super::{ndx_convert_totals, partition_point_depth};

    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);
    // Default ndx_segments has one entry; extend it to four.
    ctx.ndx_segments.push((10, 11));
    ctx.ndx_segments.push((20, 22));
    ctx.ndx_segments.push((30, 33));

    let per_call_depth = partition_point_depth(ctx.ndx_segments.len());
    assert!(
        per_call_depth >= 3,
        "expected partition_point_depth(4) >= 3, got {per_call_depth}"
    );

    const N: u64 = 8;
    let (_, cmps_before) = ndx_convert_totals();
    for _ in 0..N {
        let _ = ctx.flat_to_wire_ndx(0);
    }
    let (_, cmps_after) = ndx_convert_totals();

    assert!(
        cmps_after >= cmps_before + N * per_call_depth,
        "cumulative partition_point depth should grow by at least {} \
         (before={cmps_before}, after={cmps_after})",
        N * per_call_depth
    );
}

/// Verifies that [`super::ReceiverContext::wire_to_flat_ndx`] is the
/// inverse of [`super::ReceiverContext::flat_to_wire_ndx`] across a
/// multi-segment table built up by INC_RECURSE.
#[test]
fn wire_to_flat_ndx_round_trips_with_flat_to_wire() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Simulate INC_RECURSE: initial segment (0, 1) plus two extras.
    ctx.ndx_segments = vec![(0, 1), (5, 7), (12, 15)];
    ctx.file_list = (0..18)
        .map(|i| FileEntry::new_file(PathBuf::from(format!("f{i}")), 0, 0o644))
        .collect();

    for flat in 0..18usize {
        let wire = ctx.flat_to_wire_ndx(flat);
        assert_eq!(
            ctx.wire_to_flat_ndx(wire),
            Some(flat),
            "round-trip failed at flat={flat} wire={wire}"
        );
    }

    // Out-of-range wire NDXes (the reserved 0 under INC_RECURSE and any
    // value above the last segment's max) must return None.
    assert_eq!(ctx.wire_to_flat_ndx(0), None);
    assert_eq!(ctx.wire_to_flat_ndx(i32::MAX), None);
}

/// DDP-B3 (#2257) integration check: a synthetic INC_RECURSE state with
/// a `DeleteContext` attached publishes one [`engine::delete::DeletePlan`]
/// per segment into the shared [`engine::delete::DeletePlanMap`], and the
/// emitter-side traversal cursor records the segment's child directories.
#[test]
fn delete_pipeline_hook_publishes_one_plan_per_segment() {
    use std::sync::Arc;

    use engine::delete::{DeleteContext, DeletePlanMap};

    // Build a destination tree with extras the receiver should plan to
    // delete:
    //   <root>/sub1/keep
    //   <root>/sub1/extra
    //   <root>/sub2/keep
    //   <root>/sub2/extra
    let tmp = tempfile::TempDir::new().unwrap();
    for sub in ["sub1", "sub2"] {
        let dir = tmp.path().join(sub);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("keep"), b"").unwrap();
        std::fs::write(dir.join("extra"), b"").unwrap();
    }

    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Build a flist matching what the receiver would have after the
    // initial segment plus two INC_RECURSE segments. Segment table is
    // laid out so wire NDX 1 -> "sub1", wire NDX 2 -> "sub2".
    ctx.file_list = vec![
        FileEntry::new_directory(PathBuf::from("sub1"), 0o755),
        FileEntry::new_directory(PathBuf::from("sub2"), 0o755),
        // sub1 segment entries (flat 2..=3)
        FileEntry::new_file(PathBuf::from("keep"), 0, 0o644),
        FileEntry::new_directory(PathBuf::from("nested1"), 0o755),
        // sub2 segment entries (flat 4..=5)
        FileEntry::new_file(PathBuf::from("keep"), 0, 0o644),
        FileEntry::new_directory(PathBuf::from("nested2"), 0o755),
    ];
    // Initial segment owns wire 1..=2 at flat 0..=1; segments owning
    // wire 4..=5 at flat 2..=3, then 7..=8 at flat 4..=5.
    ctx.ndx_segments = vec![(0, 1), (2, 4), (4, 7)];

    let map = Arc::new(DeletePlanMap::new());
    let delete_ctx = Arc::new(DeleteContext::with_shared_plan_map(
        Arc::clone(&map),
        tmp.path().to_path_buf(),
        true,
    ));
    ctx.set_delete_context(Some(Arc::clone(&delete_ctx)));

    // Observe the synthetic root segment first so the traversal cursor
    // knows sub1 and sub2 are children of the root. The receiver
    // normally does this implicitly when `receive_file_list` lands the
    // initial flist; here we feed it directly so the cursor walk in
    // the assertion below can descend into both subtrees.
    delete_ctx
        .observe_segment_for_delete(
            std::path::Path::new(""),
            &[
                FileEntry::new_directory(PathBuf::from("sub1"), 0o755),
                FileEntry::new_directory(PathBuf::from("sub2"), 0o755),
            ],
        )
        .expect("root observe ok");

    // dir_ndx wire 1 -> "sub1" segment at flat_start 2
    ctx.publish_segment_to_delete_pipeline(1, 2);
    // dir_ndx wire 2 -> "sub2" segment at flat_start 4
    ctx.publish_segment_to_delete_pipeline(2, 4);

    assert_eq!(
        map.len(),
        3,
        "root + two segments -> three plans (root, sub1, sub2)"
    );
    // Drop the root plan so the rest of the assertions focus on sub1
    // and sub2 alone.
    let _ = map.take(std::path::Path::new(""));
    let sub1_plan = map.take(std::path::Path::new("sub1")).expect("sub1 plan");
    let sub2_plan = map.take(std::path::Path::new("sub2")).expect("sub2 plan");

    let sub1_names: Vec<&std::ffi::OsStr> = sub1_plan
        .extras
        .iter()
        .map(|e| e.name.as_os_str())
        .collect();
    let sub2_names: Vec<&std::ffi::OsStr> = sub2_plan
        .extras
        .iter()
        .map(|e| e.name.as_os_str())
        .collect();
    assert_eq!(sub1_names, vec![std::ffi::OsStr::new("extra")]);
    assert_eq!(sub2_names, vec![std::ffi::OsStr::new("extra")]);

    // Cursor should have learned about nested1 + nested2 as child dirs.
    let mut cursor = delete_ctx.cursor.lock().unwrap();
    let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
    assert!(seq.contains(&PathBuf::from("sub1/nested1")));
    assert!(seq.contains(&PathBuf::from("sub2/nested2")));
}

/// DDP-B3 (#2257): with no [`engine::delete::DeleteContext`] attached,
/// the segment hook is a no-op even when invoked directly.
#[test]
fn delete_pipeline_hook_is_noop_when_no_context_attached() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);
    ctx.file_list = vec![FileEntry::new_directory(PathBuf::from("sub"), 0o755)];
    ctx.ndx_segments = vec![(0, 1)];

    // Should not panic, should not touch any external state. The
    // absence of a DeleteContext means publish is a pure return.
    ctx.publish_segment_to_delete_pipeline(1, 1);
    assert!(ctx.delete_context().is_none());
}
