//! crates/batch/src/tests.rs
//!
//! Integration tests for batch mode.

#[cfg(test)]
mod integration {
    use crate::format::BatchFlags;
    use crate::reader::BatchReader;
    use crate::writer::BatchWriter;
    use crate::{BatchConfig, BatchMode};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_batch_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("roundtrip.batch");

        // Write a batch file
        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        )
        .with_checksum_seed(99999)
        .with_compat_flags(42);

        let mut writer = BatchWriter::new(write_config).unwrap();

        let mut flags = BatchFlags::default();
        flags.recurse = true;
        flags.preserve_uid = true;
        flags.preserve_gid = true;
        flags.preserve_links = true;
        flags.do_compression = true;

        writer.write_header(flags).unwrap();
        writer.write_data(b"file list data here").unwrap();
        writer.write_data(b"delta operations here").unwrap();
        writer.finalize().unwrap();

        // Read the batch file back
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        // Verify flags match
        assert_eq!(flags, read_flags);

        // Verify data can be read back
        let mut buf = vec![0u8; 100];
        let n = reader.read_data(&mut buf).unwrap();
        assert!(n > 0);
        assert!(buf[..n].starts_with(b"file list data here"));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_batch_protocol_28() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("protocol28.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            28, // Protocol 28
        );

        let mut writer = BatchWriter::new(config).unwrap();

        let mut flags = BatchFlags::default();
        flags.recurse = true;
        flags.preserve_hard_links = true;

        writer.write_header(flags).unwrap();
        writer.finalize().unwrap();

        // Read back
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            28,
        );

        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        assert!(read_flags.recurse);
        assert!(read_flags.preserve_hard_links);

        // Verify compat_flags is None for protocol 28
        assert!(reader.header().unwrap().compat_flags.is_none());
    }

    #[test]
    fn test_batch_empty_data() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("empty.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.finalize().unwrap();

        // Read back empty batch
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(read_config).unwrap();
        reader.read_header().unwrap();

        let mut buf = [0u8; 10];
        let n = reader.read_data(&mut buf).unwrap();
        assert_eq!(n, 0); // EOF
    }

    #[test]
    fn test_batch_large_data() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("large.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();

        // Write 1MB of data
        let large_data = vec![0xAB; 1024 * 1024];
        writer.write_data(&large_data).unwrap();
        writer.finalize().unwrap();

        // Read back
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(read_config).unwrap();
        reader.read_header().unwrap();

        let mut read_data = Vec::new();
        let mut buf = vec![0u8; 4096];
        loop {
            let n = reader.read_data(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            read_data.extend_from_slice(&buf[..n]);
        }

        assert_eq!(read_data.len(), large_data.len());
        assert_eq!(read_data, large_data);
    }

    #[test]
    fn test_batch_config_modes() {
        let config = BatchConfig::new(BatchMode::Write, "test".to_owned(), 30);
        assert!(config.is_write_mode());
        assert!(!config.is_read_mode());
        assert!(config.should_transfer());

        let config2 = BatchConfig::new(BatchMode::OnlyWrite, "test".to_owned(), 30);
        assert!(config2.is_write_mode());
        assert!(!config2.is_read_mode());
        assert!(!config2.should_transfer());

        let config3 = BatchConfig::new(BatchMode::Read, "test".to_owned(), 30);
        assert!(!config3.is_write_mode());
        assert!(config3.is_read_mode());
        assert!(config3.should_transfer());
    }

    #[test]
    fn test_batch_script_path() {
        let config = BatchConfig::new(BatchMode::Write, "mybatch".to_owned(), 30);
        assert_eq!(config.script_file_path(), "mybatch.sh");

        let config2 = BatchConfig::new(BatchMode::Write, "/tmp/test.batch".to_owned(), 30);
        assert_eq!(config2.script_file_path(), "/tmp/test.batch.sh");
    }

    #[test]
    fn test_batch_header_and_file_entries_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("full_roundtrip.batch");

        let protocol_version = 31;
        let checksum_seed = 0xCAFE_BABEu32 as i32;
        let compat_flags_val = 0x07;

        // -- Write phase --
        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(checksum_seed)
        .with_compat_flags(compat_flags_val);

        let mut writer = BatchWriter::new(write_config).unwrap();

        let flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            preserve_gid: true,
            preserve_links: true,
            preserve_devices: false,
            preserve_hard_links: true,
            always_checksum: false,
            xfer_dirs: true,
            do_compression: true,
            iconv: false,
            preserve_acls: true,
            preserve_xattrs: true,
            inplace: false,
            append: false,
            append_verify: false,
        };

        writer.write_header(flags).unwrap();

        // Write file entries with varying metadata
        let entries = vec![
            crate::format::FileEntry {
                path: "src/main.rs".to_owned(),
                mode: 0o100644,
                size: 2048,
                mtime: 1_700_000_000,
                uid: Some(1000),
                gid: Some(1000),
            },
            crate::format::FileEntry {
                path: "README.md".to_owned(),
                mode: 0o100644,
                size: 512,
                mtime: 1_699_000_000,
                uid: None,
                gid: None,
            },
            crate::format::FileEntry {
                path: "bin/tool".to_owned(),
                mode: 0o100755,
                size: 65536,
                mtime: 1_698_000_000,
                uid: Some(0),
                gid: Some(0),
            },
        ];

        for entry in &entries {
            writer.write_file_entry(entry).unwrap();
        }

        writer.finalize().unwrap();

        // -- Read phase --
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            0, // intentionally different - reader should adopt from header
        );

        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        // Verify header fields
        let header = reader.header().unwrap();
        assert_eq!(header.protocol_version, protocol_version);
        assert_eq!(header.checksum_seed, checksum_seed);
        assert_eq!(header.compat_flags, Some(compat_flags_val));

        // Verify the reader adopted the protocol version from the header
        assert_eq!(reader.config().protocol_version, protocol_version);

        // Verify stream flags match exactly
        assert_eq!(read_flags, flags);

        // Verify file entries round-trip correctly
        for expected in &entries {
            let actual = reader
                .read_file_entry()
                .unwrap()
                .expect("expected a file entry");
            assert_eq!(
                actual.path, expected.path,
                "path mismatch for {}",
                expected.path
            );
            assert_eq!(
                actual.mode, expected.mode,
                "mode mismatch for {}",
                expected.path
            );
            assert_eq!(
                actual.size, expected.size,
                "size mismatch for {}",
                expected.path
            );
            assert_eq!(
                actual.mtime, expected.mtime,
                "mtime mismatch for {}",
                expected.path
            );
            assert_eq!(
                actual.uid, expected.uid,
                "uid mismatch for {}",
                expected.path
            );
            assert_eq!(
                actual.gid, expected.gid,
                "gid mismatch for {}",
                expected.path
            );
        }

        // No more entries
        let trailing = reader.read_file_entry().unwrap();
        assert!(trailing.is_none(), "expected no more file entries");
    }

    #[test]
    fn test_batch_header_and_stats_roundtrip_protocol_28() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("proto28_roundtrip.batch");

        let protocol_version = 28;
        let checksum_seed = 42;

        // -- Write phase --
        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(checksum_seed);

        let mut writer = BatchWriter::new(write_config).unwrap();

        let flags = BatchFlags {
            recurse: true,
            preserve_hard_links: true,
            always_checksum: true,
            // Protocol-29+ and protocol-30+ fields should be masked out
            xfer_dirs: true,
            preserve_acls: true,
            ..Default::default()
        };

        writer.write_header(flags).unwrap();

        let stats = crate::format::BatchStats {
            total_read: 4096,
            total_written: 8192,
            total_size: 1_000_000,
            flist_buildtime: None,
            flist_xfertime: None,
        };
        writer.write_stats(&stats).unwrap();
        writer.finalize().unwrap();

        // -- Read phase --
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            28,
        );

        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        let header = reader.header().unwrap();
        assert_eq!(header.protocol_version, protocol_version);
        assert_eq!(header.checksum_seed, checksum_seed);
        assert!(
            header.compat_flags.is_none(),
            "protocol 28 has no compat flags"
        );

        // Protocol-29+ bits should be masked out
        assert!(read_flags.recurse);
        assert!(read_flags.preserve_hard_links);
        assert!(read_flags.always_checksum);
        assert!(
            !read_flags.xfer_dirs,
            "xfer_dirs should be masked for protocol 28"
        );
        assert!(
            !read_flags.preserve_acls,
            "preserve_acls should be masked for protocol 28"
        );

        // Read stats back
        let read_stats = reader.read_stats().unwrap();
        assert_eq!(read_stats.total_read, stats.total_read);
        assert_eq!(read_stats.total_written, stats.total_written);
        assert_eq!(read_stats.total_size, stats.total_size);
        assert!(read_stats.flist_buildtime.is_none());
        assert!(read_stats.flist_xfertime.is_none());
    }

    #[test]
    fn test_batch_file_corruption() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("corrupt.batch");

        // Write a truncated batch file
        fs::write(&batch_path, b"CORRUPT").unwrap();

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        let result = reader.read_header();
        assert!(result.is_err()); // Should fail on corrupt data
    }

    /// Verifies that a batch file containing protocol-format flist entries
    /// can be written and read back correctly. This exercises the full path
    /// that upstream rsync uses: header + protocol wire flist + end marker.
    #[test]
    fn test_protocol_flist_roundtrip() {
        use protocol::flist::{FileEntry, FileListWriter};

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("flist_roundtrip.batch");

        let protocol_version = 31;
        let checksum_seed = 12345;

        // -- Write phase --
        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(checksum_seed);

        let mut writer = BatchWriter::new(write_config).unwrap();

        let flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            preserve_gid: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        // Encode file entries using the protocol wire format
        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let entries = vec![
            {
                let mut e = FileEntry::new_file("src/main.rs".into(), 2048, 0o644);
                e.set_mtime(1_700_000_000, 0);
                e.set_uid(1000);
                e.set_gid(1000);
                e
            },
            {
                let mut e = FileEntry::new_directory("src".into(), 0o755);
                e.set_mtime(1_700_000_000, 0);
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

        // Write end-of-list marker
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        writer.finalize().unwrap();

        // -- Read phase --
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let mut reader = BatchReader::new(read_config).unwrap();
        reader.read_header().unwrap();

        let read_entries = reader.read_protocol_flist().unwrap();
        assert_eq!(read_entries.len(), entries.len());

        assert_eq!(read_entries[0].name(), "src/main.rs");
        assert_eq!(read_entries[0].size(), 2048);
        assert_eq!(read_entries[0].uid(), Some(1000));
        assert_eq!(read_entries[0].gid(), Some(1000));

        assert_eq!(read_entries[1].name(), "src");
    }

    /// Verifies that known upstream-compatible batch file bytes can be
    /// parsed correctly. This tests a manually constructed batch file
    /// matching the upstream format:
    ///   stream_flags(i32) + protocol_version(i32) + compat_flags(varint)
    ///   + checksum_seed(i32) + flist body + end marker.
    #[test]
    fn test_known_batch_header_format() {
        use std::io::Cursor;

        // Build a batch header in the exact upstream wire format
        let mut data = Vec::new();

        // Stream flags: recurse(bit 0) + preserve_uid(bit 1) = 0x03
        data.extend_from_slice(&3i32.to_le_bytes());

        // Protocol version: 31
        data.extend_from_slice(&31i32.to_le_bytes());

        // Compat flags (varint): 0 (no special compat flags)
        protocol::write_varint(&mut data, 0).unwrap();

        // Checksum seed: 42
        data.extend_from_slice(&42i32.to_le_bytes());

        // Parse via Cursor
        let mut cursor = Cursor::new(&data);
        let header = crate::format::BatchHeader::read_from(&mut cursor).unwrap();

        assert_eq!(header.protocol_version, 31);
        assert_eq!(header.checksum_seed, 42);
        assert_eq!(header.compat_flags, Some(0));
        assert!(header.stream_flags.recurse);
        assert!(header.stream_flags.preserve_uid);
        assert!(!header.stream_flags.preserve_gid);

        // Verify write_to produces the same bytes
        let mut written = Vec::new();
        header.write_to(&mut written).unwrap();
        assert_eq!(written, data);
    }

    /// Verifies that the preserve_devices stream flag correctly enables
    /// both preserve_devices and preserve_specials on the flist reader.
    /// upstream: -D = --devices --specials (batch.c:flag_ptr[4])
    #[test]
    fn test_protocol_flist_roundtrip_with_devices() {
        use protocol::flist::{FileEntry, FileListWriter};

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("devices.batch");

        let protocol_version = 31;

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let mut writer = BatchWriter::new(write_config).unwrap();

        let flags = BatchFlags {
            recurse: true,
            preserve_devices: true, // This should imply specials too
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        entry.set_mtime(1_700_000_000, 0);

        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_data(&buf).unwrap();

        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        writer.finalize().unwrap();

        // Read back - preserve_specials should be set from preserve_devices
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let mut reader = BatchReader::new(read_config).unwrap();
        reader.read_header().unwrap();

        let read_entries = reader.read_protocol_flist().unwrap();
        assert_eq!(read_entries.len(), 1);
        assert_eq!(read_entries[0].name(), "test.txt");
        assert_eq!(read_entries[0].size(), 100);
    }
}
