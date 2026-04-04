use super::*;
use crate::format::BatchFlags;
use crate::{BatchConfig, BatchMode, BatchWriter};
use std::path::Path;
use tempfile::TempDir;

#[allow(clippy::field_reassign_with_default)]
fn create_test_batch(path: &Path) {
    let config = BatchConfig::new(BatchMode::Write, path.to_string_lossy().to_string(), 30)
        .with_checksum_seed(12345);

    let mut writer = BatchWriter::new(config).unwrap();
    let mut flags = BatchFlags::default();
    flags.recurse = true;
    writer.write_header(flags).unwrap();
    writer.write_data(b"test data here").unwrap();
    writer.finalize().unwrap();
}

mod reader_creation_tests {
    use super::*;

    #[test]
    fn create_with_valid_file() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let reader = BatchReader::new(config);
        assert!(reader.is_ok());
    }

    #[test]
    fn create_with_nonexistent_file() {
        let config = BatchConfig::new(
            BatchMode::Read,
            "/nonexistent/path/batch.file".to_owned(),
            30,
        );

        let reader = BatchReader::new(config);
        assert!(reader.is_err());
    }

    #[test]
    fn header_is_none_before_read() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let reader = BatchReader::new(config).unwrap();
        assert!(reader.header().is_none());
    }
}

mod header_tests {
    use super::*;

    #[test]
    fn read_header_success() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        let flags = reader.read_header().unwrap();

        assert!(flags.recurse);
        assert!(reader.header().is_some());
    }

    #[test]
    fn double_header_read_error() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();
        let result = reader.read_header();
        assert!(result.is_err());
    }

    #[test]
    fn adopts_protocol_version_from_header() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            28, // Different from the 30 used to write
        );

        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        // Config should adopt the protocol version from the batch header
        assert_eq!(reader.config().protocol_version, 30);
        assert_eq!(reader.header().unwrap().protocol_version, 30);
    }

    #[test]
    fn adopts_compat_flags_from_header() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("compat.batch");

        // Write a batch with non-zero compat flags
        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        )
        .with_compat_flags(0x3F)
        .with_checksum_seed(99);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.finalize().unwrap();

        // Read back with different config values
        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            31,
        );
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        // Config must adopt the compat flags from the batch header
        assert_eq!(reader.config().compat_flags, Some(0x3F));
        assert_eq!(reader.header().unwrap().compat_flags, Some(0x3F));
    }

    #[test]
    fn adopts_checksum_seed_from_header() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("seed.batch");

        // Write a batch with a specific checksum seed
        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        )
        .with_checksum_seed(0xDEAD);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.finalize().unwrap();

        // Read back with default seed (0)
        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        // Config must adopt the checksum seed from the batch header
        assert_eq!(reader.config().checksum_seed, 0xDEAD);
        assert_eq!(reader.header().unwrap().checksum_seed, 0xDEAD);
    }

    #[test]
    fn adopts_none_compat_flags_for_old_protocol() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("old_proto.batch");

        // Write with protocol 28 (no compat flags)
        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            28,
        )
        .with_checksum_seed(42);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.finalize().unwrap();

        // Read back - compat_flags should be None for protocol < 30
        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            31, // config says 31 but batch says 28
        );
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        assert_eq!(reader.config().protocol_version, 28);
        assert_eq!(reader.config().compat_flags, None);
        assert_eq!(reader.config().checksum_seed, 42);
    }
}

mod data_tests {
    use super::*;

    #[test]
    fn read_data_without_header() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        let mut buf = [0u8; 100];
        assert!(reader.read_data(&mut buf).is_err());
    }

    #[test]
    fn read_data_success() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let mut buf = [0u8; 100];
        let n = reader.read_data(&mut buf).unwrap();
        assert!(n > 0);
        assert_eq!(&buf[..14], b"test data here");
    }

    #[test]
    fn read_exact_without_header() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        let mut buf = [0u8; 10];
        assert!(reader.read_exact(&mut buf).is_err());
    }

    #[test]
    fn read_exact_success() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"test");
    }

    #[test]
    fn read_exact_insufficient_data() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        // Try to read more data than available
        let mut buf = [0u8; 1000];
        let result = reader.read_exact(&mut buf);
        assert!(result.is_err());
    }
}

mod file_entry_tests {
    use super::*;

    #[test]
    fn read_file_entry_without_header() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        let result = reader.read_file_entry();
        assert!(result.is_err());
    }

    #[test]
    fn read_file_entry_returns_none_on_eof() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("empty.batch");

        // Create a batch with just header, no file entries
        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.finalize().unwrap();

        // Read it back
        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        // Should return None on EOF
        let entry = reader.read_file_entry().unwrap();
        assert!(entry.is_none());
    }
}

mod config_tests {
    use super::*;

    #[test]
    fn config_accessor() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let reader = BatchReader::new(config).unwrap();
        assert_eq!(reader.config().protocol_version, 30);
    }

    #[test]
    fn header_accessor_before_read() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let reader = BatchReader::new(config).unwrap();
        assert!(reader.header().is_none());
    }

    #[test]
    fn header_accessor_after_read() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();
        let header = reader.header().unwrap();
        assert_eq!(header.protocol_version, 30);
    }

    #[test]
    fn io_error_starts_at_zero() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let reader = BatchReader::new(config).unwrap();
        assert_eq!(reader.io_error(), 0);
    }
}

mod flist_deserialization_tests {
    use super::*;
    use protocol::flist::{FileEntry as ProtocolFileEntry, FileListWriter};

    /// Write a batch with protocol flist entries and read them back using
    /// `read_protocol_flist`. This validates the core deserialization path
    /// that batch replay depends on.
    #[test]
    fn protocol_flist_roundtrip_basic() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("flist_basic.batch");
        let protocol_version = 31;

        // Write phase
        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(42);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            preserve_gid: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let entries = vec![
            {
                let mut e = ProtocolFileEntry::new_file("alpha.txt".into(), 1024, 0o644);
                e.set_mtime(1_700_000_000, 0);
                e.set_uid(1000);
                e.set_gid(1000);
                e
            },
            {
                let mut e = ProtocolFileEntry::new_file("beta.txt".into(), 2048, 0o644);
                e.set_mtime(1_700_000_001, 0);
                e.set_uid(1001);
                e.set_gid(1001);
                e
            },
            {
                let mut e = ProtocolFileEntry::new_directory("subdir".into(), 0o755);
                e.set_mtime(1_700_000_002, 0);
                e.set_uid(1000);
                e.set_gid(1000);
                e
            },
        ];

        for entry in &entries {
            let mut buf = Vec::new();
            flist_writer.write_entry(&mut buf, entry).unwrap();
            writer.write_data(&buf).unwrap();
        }

        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        // Write empty uid and gid ID lists (terminator-only).
        // upstream: uidlist.c:send_id_lists() sends these after the flist
        // when preserve_uid/gid is set and !inc_recurse.
        let uid_list = protocol::idlist::IdList::new();
        let gid_list = protocol::idlist::IdList::new();
        let mut id_buf = Vec::new();
        uid_list
            .write(&mut id_buf, false, protocol_version as u8)
            .unwrap();
        gid_list
            .write(&mut id_buf, false, protocol_version as u8)
            .unwrap();
        writer.write_data(&id_buf).unwrap();

        writer.finalize().unwrap();

        // Read phase
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );
        let mut reader = BatchReader::new(read_config).unwrap();
        reader.read_header().unwrap();

        let read_entries = reader.read_protocol_flist().unwrap();
        assert_eq!(read_entries.len(), 3);
        assert_eq!(read_entries[0].name(), "alpha.txt");
        assert_eq!(read_entries[0].size(), 1024);
        assert_eq!(read_entries[0].uid(), Some(1000));
        assert_eq!(read_entries[1].name(), "beta.txt");
        assert_eq!(read_entries[1].size(), 2048);
        assert_eq!(read_entries[2].name(), "subdir");
        assert!(read_entries[2].is_dir());

        // io_error should be zero for a clean flist
        assert_eq!(reader.io_error(), 0);
    }

    /// Validates that `always_checksum` (--checksum / -c) is correctly wired
    /// to the flist reader. When this flag is set, each regular file entry
    /// in the flist carries a trailing checksum. If the reader doesn't consume
    /// these bytes, subsequent entries will be deserialized incorrectly.
    ///
    /// upstream: flist.c:670 writes checksum bytes, flist.c:1202 reads them
    #[test]
    fn protocol_flist_roundtrip_with_always_checksum() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("flist_checksum.batch");
        let protocol_version = 31;
        let csum_len = 16; // MD5 digest length

        // Write phase
        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(99);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            always_checksum: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol).with_always_checksum(csum_len);

        // Two regular files - each will have a checksum after it on the wire
        let entries = vec![
            {
                let mut e = ProtocolFileEntry::new_file("file1.dat".into(), 500, 0o644);
                e.set_mtime(1_700_000_000, 0);
                e.set_checksum(vec![0xAA; csum_len]);
                e
            },
            {
                let mut e = ProtocolFileEntry::new_file("file2.dat".into(), 1500, 0o644);
                e.set_mtime(1_700_000_001, 0);
                e.set_checksum(vec![0xBB; csum_len]);
                e
            },
        ];

        for entry in &entries {
            let mut buf = Vec::new();
            flist_writer.write_entry(&mut buf, entry).unwrap();
            writer.write_data(&buf).unwrap();
        }

        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();
        writer.finalize().unwrap();

        // Read phase - the reader must correctly consume checksum bytes
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );
        let mut reader = BatchReader::new(read_config).unwrap();
        reader.read_header().unwrap();

        let read_entries = reader.read_protocol_flist().unwrap();
        assert_eq!(
            read_entries.len(),
            2,
            "should read both entries when always_checksum is wired correctly"
        );
        assert_eq!(read_entries[0].name(), "file1.dat");
        assert_eq!(read_entries[0].size(), 500);
        assert_eq!(read_entries[1].name(), "file2.dat");
        assert_eq!(read_entries[1].size(), 1500);
    }

    /// Verifies that `default_flist_csum_len` returns 16 for all supported
    /// protocol versions, matching upstream MD4/MD5 digest length.
    #[test]
    fn default_flist_csum_len_values() {
        use super::super::flist::default_flist_csum_len;
        for proto in [27, 28, 29, 30, 31, 32] {
            assert_eq!(
                default_flist_csum_len(proto),
                16,
                "flist_csum_len should be 16 for protocol {proto}"
            );
        }
    }

    /// Verifies that an empty flist (just the end marker) reads back as
    /// an empty vec with zero io_error.
    #[test]
    fn protocol_flist_empty() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("flist_empty.batch");
        let protocol_version = 31;

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let mut writer = BatchWriter::new(write_config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let flist_writer = FileListWriter::new(protocol);

        // Write only the end marker, no entries
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();
        writer.finalize().unwrap();

        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );
        let mut reader = BatchReader::new(read_config).unwrap();
        reader.read_header().unwrap();

        let read_entries = reader.read_protocol_flist().unwrap();
        assert!(read_entries.is_empty());
        assert_eq!(reader.io_error(), 0);
    }
}

mod token_delta_tests {
    use super::*;

    /// Write a batch file with token-format delta data and verify that
    /// `read_file_delta_tokens` decodes it correctly.
    #[test]
    fn read_token_delta_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("token_delta.batch");

        // Write a batch with token-format delta data for one file
        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        )
        .with_checksum_seed(42);

        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();

        // Write token-format delta data: one literal + end marker
        let mut buf = Vec::new();
        protocol::wire::delta::write_token_literal(&mut buf, b"hello batch world").unwrap();
        protocol::wire::delta::write_token_end(&mut buf).unwrap();
        writer.write_data(&buf).unwrap();
        writer.finalize().unwrap();

        // Read back
        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let ops = reader.read_file_delta_tokens().unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            protocol::wire::DeltaOp::Literal(data) => {
                assert_eq!(data, b"hello batch world");
            }
            _ => panic!("expected literal op"),
        }
    }

    /// Verify that multiple files' delta streams can be read sequentially.
    #[test]
    fn read_multiple_file_deltas() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("multi_delta.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        )
        .with_checksum_seed(7);

        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();

        // File 1: literal "AAA" + end
        let mut buf = Vec::new();
        protocol::wire::delta::write_token_literal(&mut buf, b"AAA").unwrap();
        protocol::wire::delta::write_token_end(&mut buf).unwrap();
        writer.write_data(&buf).unwrap();

        // File 2: literal "BBBBB" + block match 0 + end
        let mut buf2 = Vec::new();
        protocol::wire::delta::write_token_literal(&mut buf2, b"BBBBB").unwrap();
        protocol::wire::delta::write_token_block_match(&mut buf2, 0).unwrap();
        protocol::wire::delta::write_token_end(&mut buf2).unwrap();
        writer.write_data(&buf2).unwrap();

        writer.finalize().unwrap();

        // Read back
        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        // File 1
        let ops1 = reader.read_file_delta_tokens().unwrap();
        assert_eq!(ops1.len(), 1);
        assert_eq!(ops1[0], protocol::wire::DeltaOp::Literal(b"AAA".to_vec()));

        // File 2
        let ops2 = reader.read_file_delta_tokens().unwrap();
        assert_eq!(ops2.len(), 2);
        assert_eq!(ops2[0], protocol::wire::DeltaOp::Literal(b"BBBBB".to_vec()));
        assert!(matches!(
            ops2[1],
            protocol::wire::DeltaOp::Copy { block_index: 0, .. }
        ));
    }

    #[test]
    fn read_delta_tokens_without_header() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");
        create_test_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        let result = reader.read_file_delta_tokens();
        assert!(result.is_err());
    }
}

mod inc_recurse_flist_tests {
    use super::*;
    use protocol::CompatibilityFlags;
    use protocol::ProtocolVersion;
    use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum};
    use protocol::flist::{FileEntry, FileListWriter};
    use std::io::Write;

    /// Build a synthetic batch file with INC_RECURSE incremental flist segments.
    ///
    /// Layout:
    /// 1. Batch header (stream_flags + proto_version + compat_flags + checksum_seed)
    /// 2. Initial flist segment: just "." (root directory)
    /// 3. NDX for sub-list of dir at index 0 (NDX_FLIST_OFFSET - 0)
    /// 4. Sub-flist segment: "file1.txt" and "file2.txt" (relative to ".")
    /// 5. NDX_FLIST_EOF
    fn build_inc_recurse_batch(path: &Path) {
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        // INC_RECURSE + VARINT_FLIST_FLAGS + SAFE_FILE_LIST
        let compat_flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::VARINT_FLIST_FLAGS
            | CompatibilityFlags::SAFE_FILE_LIST;

        let mut file = std::fs::File::create(path).unwrap();

        // --- Batch header ---
        let header = crate::format::BatchHeader {
            stream_flags: BatchFlags {
                recurse: true,
                ..BatchFlags::default()
            },
            protocol_version: 32,
            compat_flags: Some(compat_flags.bits() as i32),
            checksum_seed: 42,
        };
        header.write_to(&mut file).unwrap();

        // --- Initial flist segment: "." directory ---
        let mut flist_writer = FileListWriter::with_compat_flags(protocol, compat_flags);
        let dot_dir = FileEntry::new_directory(".".into(), 0o755);
        flist_writer.write_entry(&mut file, &dot_dir).unwrap();
        flist_writer.write_end(&mut file, None).unwrap();

        // --- NDX for sub-list: directory index 0 ---
        let mut ndx_codec = NdxCodecEnum::new(32);
        let sub_list_ndx = NDX_FLIST_OFFSET; // dir at index 0
        ndx_codec.write_ndx(&mut file, sub_list_ndx).unwrap();

        // --- Sub-flist segment: entries relative to "." ---
        let mut sub_writer = FileListWriter::with_compat_flags(protocol, compat_flags);
        let entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        let entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
        sub_writer.write_entry(&mut file, &entry1).unwrap();
        sub_writer.write_entry(&mut file, &entry2).unwrap();
        sub_writer.write_end(&mut file, None).unwrap();

        // --- NDX_FLIST_EOF ---
        ndx_codec.write_ndx(&mut file, NDX_FLIST_EOF).unwrap();

        file.flush().unwrap();
    }

    #[test]
    fn read_inc_recurse_flist_reads_all_segments() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("inc_recurse.batch");
        build_inc_recurse_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            32,
        );

        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let entries = reader.read_protocol_flist().unwrap();

        // Should have 3 entries: "." directory + "file1.txt" + "file2.txt"
        assert_eq!(
            entries.len(),
            3,
            "Expected 3 entries, got {}",
            entries.len()
        );

        // First entry is the root directory "."
        assert_eq!(entries[0].name(), ".");

        // Sub-list entries should have their parent directory prepended.
        // Since parent is ".", paths become "./file1.txt" and "./file2.txt".
        let name1 = entries[1].name();
        let name2 = entries[2].name();
        assert!(
            name1.contains("file1.txt"),
            "Expected file1.txt in name, got: {name1}"
        );
        assert!(
            name2.contains("file2.txt"),
            "Expected file2.txt in name, got: {name2}"
        );
    }

    #[test]
    fn read_inc_recurse_flist_preserves_ndx_codec_for_replay() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("inc_recurse_codec.batch");
        build_inc_recurse_batch(&batch_path);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            32,
        );

        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();
        let _entries = reader.read_protocol_flist().unwrap();

        // The NDX codec should be available for the delta replay phase.
        let codec = reader.take_ndx_codec();
        assert!(
            codec.is_some(),
            "NDX codec should be preserved after INC_RECURSE flist reading"
        );
    }

    #[test]
    fn read_non_inc_recurse_flist_has_no_ndx_codec() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("no_inc_recurse.batch");

        // Build a batch file WITHOUT INC_RECURSE
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let compat_flags =
            CompatibilityFlags::VARINT_FLIST_FLAGS | CompatibilityFlags::SAFE_FILE_LIST;

        let mut file = std::fs::File::create(&batch_path).unwrap();

        let header = crate::format::BatchHeader {
            stream_flags: BatchFlags {
                recurse: true,
                ..BatchFlags::default()
            },
            protocol_version: 32,
            compat_flags: Some(compat_flags.bits() as i32),
            checksum_seed: 42,
        };
        header.write_to(&mut file).unwrap();

        // Write a single flist with all entries (no INC_RECURSE)
        let mut flist_writer = FileListWriter::with_compat_flags(protocol, compat_flags);
        let dir_entry = FileEntry::new_directory(".".into(), 0o755);
        let file1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        flist_writer.write_entry(&mut file, &dir_entry).unwrap();
        flist_writer.write_entry(&mut file, &file1).unwrap();
        flist_writer.write_end(&mut file, None).unwrap();
        file.flush().unwrap();
        drop(file);

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            32,
        );

        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let entries = reader.read_protocol_flist().unwrap();
        assert_eq!(entries.len(), 2);

        // No NDX codec should be stored for non-INC_RECURSE mode.
        let codec = reader.take_ndx_codec();
        assert!(
            codec.is_none(),
            "NDX codec should not be set for non-INC_RECURSE batch"
        );
    }
}
