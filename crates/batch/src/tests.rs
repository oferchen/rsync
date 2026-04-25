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

        // Write empty uid and gid ID lists (required by upstream protocol).
        // upstream: uidlist.c:recv_id_list() reads until id=0 terminator.
        // An empty list is a single varint30 zero (one 0x00 byte for proto >= 30).
        use protocol::idlist::IdList;
        let mut id_buf = Vec::new();
        IdList::new()
            .write(&mut id_buf, false, protocol_version as u8)
            .unwrap();
        writer.write_data(&id_buf).unwrap(); // uid list
        writer.write_data(&id_buf).unwrap(); // gid list

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

    /// Verifies that `read_protocol_flist` correctly handles the `always_checksum`
    /// stream flag by wiring it to the underlying `FileListReader`. Without this
    /// fix, checksum bytes after each regular file entry are not consumed, causing
    /// subsequent entries to be deserialized incorrectly.
    ///
    /// upstream: flist.c:670 `write_buf(f, sum, flist_csum_len)` writes the
    /// checksum, and flist.c:1202 `read_buf(f, bp, flist_csum_len)` reads it.
    #[test]
    fn test_protocol_flist_roundtrip_with_always_checksum() {
        use protocol::flist::{FileEntry, FileListWriter};

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("checksum_roundtrip.batch");

        let protocol_version = 31;
        let csum_len = 16; // MD5 digest length

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(0xBEEF);

        let mut writer = BatchWriter::new(write_config).unwrap();

        let flags = BatchFlags {
            recurse: true,
            always_checksum: true,
            preserve_uid: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_always_checksum(csum_len);

        // Three files with checksums - exercises cross-entry compression state
        let entries = vec![
            {
                let mut e = FileEntry::new_file("a.txt".into(), 100, 0o644);
                e.set_mtime(1_700_000_000, 0);
                e.set_uid(1000);
                e.set_checksum(vec![0x11; csum_len]);
                e
            },
            {
                let mut e = FileEntry::new_file("b.txt".into(), 200, 0o644);
                e.set_mtime(1_700_000_001, 0);
                e.set_uid(1000);
                e.set_checksum(vec![0x22; csum_len]);
                e
            },
            {
                let mut e = FileEntry::new_file("c.txt".into(), 300, 0o755);
                e.set_mtime(1_700_000_002, 0);
                e.set_uid(1001);
                e.set_checksum(vec![0x33; csum_len]);
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

        // Write empty uid ID list (preserve_uid is set).
        // upstream: uidlist.c:recv_id_list() reads until id=0 terminator.
        use protocol::idlist::IdList;
        let mut id_buf = Vec::new();
        IdList::new()
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
        let read_flags = reader.read_header().unwrap();
        assert!(read_flags.always_checksum);

        let read_entries = reader.read_protocol_flist().unwrap();
        assert_eq!(read_entries.len(), 3, "all three entries should be read");
        assert_eq!(read_entries[0].name(), "a.txt");
        assert_eq!(read_entries[0].size(), 100);
        assert_eq!(read_entries[1].name(), "b.txt");
        assert_eq!(read_entries[1].size(), 200);
        assert_eq!(read_entries[2].name(), "c.txt");
        assert_eq!(read_entries[2].size(), 300);
        assert_eq!(reader.io_error(), 0);
    }

    /// Verifies that `io_error()` on the reader is zero after a successful
    /// protocol flist read.
    #[test]
    fn test_io_error_zero_after_clean_flist() {
        use protocol::flist::{FileEntry, FileListWriter};

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("io_error.batch");
        let protocol_version = 31;

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let mut writer = BatchWriter::new(write_config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("test.txt".into(), 42, 0o644);
        entry.set_mtime(1_700_000_000, 0);
        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_data(&buf).unwrap();

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

        assert_eq!(read_entries.len(), 1);
        assert_eq!(reader.io_error(), 0);
    }

    /// End-to-end test: write a batch file with flist + token-format delta data,
    /// then replay it to a destination directory and verify file contents.
    #[test]
    fn test_replay_with_token_deltas() {
        use protocol::flist::{FileEntry, FileListWriter};

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("replay_test.batch");
        let dest_dir = temp_dir.path().join("dest");
        fs::create_dir_all(&dest_dir).unwrap();

        let protocol_version = 31;

        // -- Write phase: build a batch file with flist + token deltas --
        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(99);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        // Write one directory entry + one regular file entry in flist format
        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol);

        let dir_entry = {
            let mut e = FileEntry::new_directory("subdir".into(), 0o755);
            e.set_mtime(1_700_000_000, 0);
            e
        };
        let file_entry = {
            let mut e = FileEntry::new_file("subdir/hello.txt".into(), 13, 0o644);
            e.set_mtime(1_700_000_000, 0);
            e
        };

        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &dir_entry).unwrap();
        writer.write_data(&buf).unwrap();

        let mut buf2 = Vec::new();
        flist_writer.write_entry(&mut buf2, &file_entry).unwrap();
        writer.write_data(&buf2).unwrap();

        // Flist end marker
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        // NDX-framed delta data for the one regular file (whole-file literal)
        // upstream: receiver.c:recv_files() reads NDX + iflags + sum_head
        // + delta tokens + file checksum per file, with NDX_DONE for phases.
        {
            use protocol::codec::{NdxCodec, NdxCodecEnum};

            let mut ndx_codec = NdxCodecEnum::new(protocol_version as u8);
            let mut ndx_buf = Vec::new();

            // NDX for file entry (index 1 - the file, after the directory at 0)
            ndx_codec.write_ndx(&mut ndx_buf, 1).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // iflags: ITEM_TRANSFER (0x8000) - protocol >= 29
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();

            // sum_head: count=0, blength=0, s2length=0, remainder=0
            // (whole-file transfer with no basis, no file checksum)
            writer.write_data(&0i32.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();

            // Token-format delta: literal data
            let mut delta_buf = Vec::new();
            protocol::wire::delta::write_token_literal(&mut delta_buf, b"Hello, batch!").unwrap();
            protocol::wire::delta::write_token_end(&mut delta_buf).unwrap();
            writer.write_data(&delta_buf).unwrap();

            // File-level checksum (16 bytes) - upstream always writes this after delta stream
            // upstream: receiver.c - sender writes xfer_sum_len bytes of file checksum
            writer.write_data(&[0u8; 16]).unwrap();

            // NDX_DONE for phase 1 -> phase 2 transition
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // NDX_DONE for phase 2 -> end
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();
        }

        writer.finalize().unwrap();

        // -- Replay phase --
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let result = crate::replay::replay(&read_config, &dest_dir, 0).unwrap();

        assert_eq!(result.file_count, 2); // 1 dir + 1 file
        assert!(result.recurse);
        assert!(dest_dir.join("subdir").is_dir());
        assert!(dest_dir.join("subdir/hello.txt").exists());

        let content = fs::read(dest_dir.join("subdir/hello.txt")).unwrap();
        assert_eq!(content, b"Hello, batch!");
    }

    /// Verifies that batch files with `do_compression=true` flag store raw
    /// (uncompressed) protocol data and can be read back correctly.
    ///
    /// PR #3051 fixed a bug where compression was applied to batch file data.
    /// Batch files must always contain uncompressed data regardless of the
    /// `do_compression` flag - that flag only records that the original
    /// transfer used compression, so --read-batch knows to set the flag
    /// when replaying. The actual batch file body is a raw tee of the
    /// uncompressed protocol stream.
    /// upstream: batch.c - batch file body is always uncompressed
    #[test]
    fn test_batch_roundtrip_with_compression_flag() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("compress_flag.batch");

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        )
        .with_checksum_seed(12345);

        let mut writer = BatchWriter::new(write_config).unwrap();

        let flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            preserve_gid: true,
            do_compression: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        // Write raw protocol data - this must be stored uncompressed
        let data1 = b"file list data with compression flag set";
        let data2 = b"delta operations - must be readable without decompression";
        writer.write_data(data1).unwrap();
        writer.write_data(data2).unwrap();
        writer.finalize().unwrap();

        // Read back and verify the compression flag is preserved
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            31,
        );
        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        assert!(
            read_flags.do_compression,
            "do_compression flag must survive roundtrip"
        );
        assert_eq!(read_flags.recurse, flags.recurse);
        assert_eq!(read_flags.preserve_uid, flags.preserve_uid);
        assert_eq!(read_flags.preserve_gid, flags.preserve_gid);

        // Verify data reads back verbatim (uncompressed)
        let mut buf = vec![0u8; 200];
        let n = reader.read_data(&mut buf).unwrap();
        assert!(n > 0, "must read data from batch file");
        assert!(
            buf[..n].starts_with(data1),
            "batch data must be readable without decompression"
        );
    }

    /// Verifies that compressed batch delta replay with block matches works
    /// correctly using CPRES_ZLIB dictionary synchronization.
    ///
    /// This exercises the exact scenario that upstream rsync 3.4.1 fails on:
    /// a compressed delta batch with copy tokens (block matches) requires the
    /// decoder to feed matched block data into the inflate dictionary via
    /// see_token(). Without this, inflate fails with "invalid distance too
    /// far back" (error -3 at token.c:608).
    ///
    /// oc-rsync implements proper dictionary sync in batch replay, making it
    /// capable of reading compressed delta batches that upstream itself cannot.
    ///
    /// upstream: token.c:see_deflate_token() - dictionary sync during live transfer
    /// upstream: token.c:608 - inflate fails without dictionary sync in batch read
    #[test]
    fn test_replay_compressed_delta_with_block_matches() {
        use protocol::flist::{FileEntry, FileListWriter};
        use protocol::wire::CompressedTokenEncoder;

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("replay_delta_z.batch");
        let dest_dir = temp_dir.path().join("dest");
        fs::create_dir_all(&dest_dir).unwrap();

        let protocol_version = 31;

        // Create basis file at destination (pre-existing file for delta transfer).
        // 2000 bytes of 'B' - block_length will be chosen by choose_block_size.
        let basis_data = vec![b'B'; 2000];
        fs::write(dest_dir.join("data.bin"), &basis_data).unwrap();

        // Delta layout: copy block0(700) + copy block1(700) + literal(17) + copy block2(600).
        let patch = b"CHANGED_DATA_HERE";
        let output_size = 700 + 700 + patch.len() + 600; // 2017

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(42);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            do_compression: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol);

        // Directory entry
        let mut dir_entry = FileEntry::new_directory(".".into(), 0o755);
        dir_entry.set_mtime(1_700_000_000, 0);
        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &dir_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // File entry with output size
        let mut file_entry = FileEntry::new_file("data.bin".into(), output_size as u64, 0o644);
        file_entry.set_mtime(1_700_000_001, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &file_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // End of flist
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        // NDX-framed delta with block matches and literals.
        // Use CPRES_ZLIB (zlibx=false) so dictionary sync is required.
        {
            use protocol::codec::{NdxCodec, NdxCodecEnum};

            let mut ndx_codec = NdxCodecEnum::new(protocol_version as u8);
            let mut ndx_buf = Vec::new();

            // NDX for file entry (index 1)
            ndx_codec.write_ndx(&mut ndx_buf, 1).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // iflags: ITEM_TRANSFER (0x8000)
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();

            // sum_head: block geometry for basis.
            // Use block_length=700 (min block for 2000-byte file).
            // 2000 / 700 = 2 full blocks + 600 remainder.
            let block_length: i32 = 700;
            let block_count: i32 = 3; // ceil(2000/700) = 3
            let remainder: i32 = 2000 - 700 * 2; // 600
            let s2length: i32 = 16;
            writer.write_data(&block_count.to_le_bytes()).unwrap();
            writer.write_data(&block_length.to_le_bytes()).unwrap();
            writer.write_data(&s2length.to_le_bytes()).unwrap();
            writer.write_data(&remainder.to_le_bytes()).unwrap();

            // Encode compressed delta: copy block 0 (700 bytes), then literal
            // patch, then copy remaining blocks. This requires CPRES_ZLIB
            // dictionary sync because the encoder/decoder share state through
            // the basis block data fed via see_token().
            let mut token_buf = Vec::new();
            let mut encoder = CompressedTokenEncoder::default();
            encoder.set_zlibx(false); // CPRES_ZLIB mode

            // Block 0: copy from basis (bytes 0..700)
            encoder.send_block_match(&mut token_buf, 0).unwrap();
            encoder.see_token(&basis_data[0..700]).unwrap();

            // Delta layout: copy block 0, copy block 1, literal patch,
            // copy block 2. Multiple block matches exercise dictionary sync.

            // Block 1: copy from basis (bytes 700..1400)
            encoder.send_block_match(&mut token_buf, 1).unwrap();
            encoder.see_token(&basis_data[700..1400]).unwrap();

            // Literal: the patched data (17 bytes)
            encoder.send_literal(&mut token_buf, patch).unwrap();

            // Block 2: copy from basis (bytes 1400..2000, remainder=600)
            encoder.send_block_match(&mut token_buf, 2).unwrap();
            encoder.see_token(&basis_data[1400..2000]).unwrap();

            encoder.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();

            // File checksum (16 zero bytes)
            writer.write_data(&[0u8; 16]).unwrap();

            // NDX_DONE for phase 1 -> phase 2
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // NDX_DONE for phase 2 -> end
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();
        }

        writer.finalize().unwrap();

        // Replay the compressed delta batch
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let result = crate::replay::replay(&read_config, &dest_dir, 0).unwrap();

        assert_eq!(result.file_count, 2); // 1 dir + 1 file
        assert!(dest_dir.join("data.bin").exists());

        let content = fs::read(dest_dir.join("data.bin")).unwrap();
        // The output should be: block0(700) + block1(700) + patch(17) + block2(600)
        // = 2017 bytes total.
        assert_eq!(content.len(), 700 + 700 + patch.len() + 600);
        // Verify the literal patch is present in the output
        assert_eq!(&content[1400..1400 + patch.len()], patch);
        // Verify block copies are correct
        assert_eq!(&content[0..700], &basis_data[0..700]);
        assert_eq!(&content[700..1400], &basis_data[700..1400]);
        assert_eq!(&content[1400 + patch.len()..], &basis_data[1400..2000]);
    }

    /// Verifies replay works when the batch header has `do_compression=true`.
    /// upstream: batch.c - when do_compression is set, delta tokens use
    /// compressed (DEFLATED_DATA) encoding in the batch file body.
    #[test]
    fn test_replay_with_compression_flag() {
        use protocol::flist::{FileEntry, FileListWriter};
        use protocol::wire::CompressedTokenEncoder;

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("replay_compress.batch");
        let dest_dir = temp_dir.path().join("dest");
        fs::create_dir_all(&dest_dir).unwrap();

        let protocol_version = 31;

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(42);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            do_compression: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol);

        // Directory entry
        let mut dir_entry = FileEntry::new_directory(".".into(), 0o755);
        dir_entry.set_mtime(1_700_000_000, 0);
        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &dir_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // File entry
        let file_data = b"compressed batch replay test";
        let mut file_entry = FileEntry::new_file("test.txt".into(), file_data.len() as u64, 0o644);
        file_entry.set_mtime(1_700_000_001, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &file_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // End of flist
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        // NDX-framed delta data for the file (upstream batch format)
        // upstream: receiver.c:recv_files() reads NDX + iflags + sum_head
        // + delta tokens + file checksum per file, with NDX_DONE for phases.
        {
            use protocol::codec::{NdxCodec, NdxCodecEnum};

            let mut ndx_codec = NdxCodecEnum::new(protocol_version as u8);
            let mut ndx_buf = Vec::new();

            // NDX for file entry (index 1 - the file, after the directory at 0)
            ndx_codec.write_ndx(&mut ndx_buf, 1).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // iflags: ITEM_TRANSFER (0x8000) - protocol >= 29
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();

            // sum_head: count=0, blength=0, s2length=16, remainder=0
            writer.write_data(&0i32.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();
            writer.write_data(&16i32.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();

            // Compressed token-format delta: literal data (zlibx mode)
            let mut token_buf = Vec::new();
            let mut encoder = CompressedTokenEncoder::default();
            encoder.set_zlibx(true);
            encoder.send_literal(&mut token_buf, file_data).unwrap();
            encoder.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();

            // File checksum (16 zero bytes, matching s2length=16)
            writer.write_data(&[0u8; 16]).unwrap();

            // NDX_DONE for phase 1 -> phase 2 transition
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // NDX_DONE for phase 2 -> end
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();
        }

        writer.finalize().unwrap();

        // Replay the batch file to destination
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let result = crate::replay::replay(&read_config, &dest_dir, 0).unwrap();

        assert_eq!(result.file_count, 2); // 1 dir + 1 file
        assert!(result.recurse);
        assert!(dest_dir.join("test.txt").exists());

        let content = fs::read(dest_dir.join("test.txt")).unwrap();
        assert_eq!(content, file_data);
    }

    /// Verifies replay of a batch file with zstd-compressed delta tokens.
    ///
    /// This tests the auto-detection path: when a batch file's compressed
    /// payload contains zstd frames (magic 0xFD2FB528), oc-rsync detects
    /// the codec and creates a zstd decoder instead of the default zlib.
    ///
    /// Upstream rsync write-batch always forces zlib (compat.c:413-414),
    /// so this scenario only arises from a patched or future upstream.
    /// oc-rsync handles it correctly via compressed payload auto-detection.
    #[cfg(feature = "zstd")]
    #[test]
    fn test_replay_zstd_compressed_batch() {
        use protocol::flist::{FileEntry, FileListWriter};
        use protocol::wire::CompressedTokenEncoder;

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("replay_zstd.batch");
        let dest_dir = temp_dir.path().join("dest");
        fs::create_dir_all(&dest_dir).unwrap();

        let protocol_version = 32;

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(42);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            do_compression: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol);

        // Directory entry
        let mut dir_entry = FileEntry::new_directory(".".into(), 0o755);
        dir_entry.set_mtime(1_700_000_000, 0);
        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &dir_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // File entry
        let file_data = b"zstd compressed batch replay test data for auto-detection";
        let mut file_entry =
            FileEntry::new_file("zstd_test.txt".into(), file_data.len() as u64, 0o644);
        file_entry.set_mtime(1_700_000_001, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &file_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // End of flist
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        // NDX-framed delta data with zstd-compressed tokens
        {
            use protocol::codec::{NdxCodec, NdxCodecEnum};

            let mut ndx_codec = NdxCodecEnum::new(protocol_version as u8);
            let mut ndx_buf = Vec::new();

            // NDX for file entry (index 1)
            ndx_codec.write_ndx(&mut ndx_buf, 1).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // iflags: ITEM_TRANSFER (0x8000)
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();

            // sum_head: count=0, blength=0, s2length=16, remainder=0
            writer.write_data(&0i32.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();
            writer.write_data(&16i32.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();

            // Use zstd encoder for compressed token-format delta
            let mut token_buf = Vec::new();
            let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();
            encoder.send_literal(&mut token_buf, file_data).unwrap();
            encoder.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();

            // File checksum (16 zero bytes)
            writer.write_data(&[0u8; 16]).unwrap();

            // NDX_DONE for phase 1 -> phase 2
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // NDX_DONE for phase 2 -> end
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();
        }

        writer.finalize().unwrap();

        // Replay the zstd-compressed batch file
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let result = crate::replay::replay(&read_config, &dest_dir, 0).unwrap();

        assert_eq!(result.file_count, 2); // 1 dir + 1 file
        assert!(dest_dir.join("zstd_test.txt").exists());

        let content = fs::read(dest_dir.join("zstd_test.txt")).unwrap();
        assert_eq!(content, file_data);
    }

    /// Verifies replay of a zstd-compressed batch with block matches.
    ///
    /// Unlike CPRES_ZLIB, zstd does not need dictionary synchronization
    /// (see_token is a noop). This test exercises the code path where
    /// the detected codec is zstd and cpres_zlib is false, ensuring
    /// block matches work without dictionary sync.
    #[cfg(feature = "zstd")]
    #[test]
    fn test_replay_zstd_compressed_delta_with_block_matches() {
        use protocol::flist::{FileEntry, FileListWriter};
        use protocol::wire::CompressedTokenEncoder;

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("replay_delta_zstd.batch");
        let dest_dir = temp_dir.path().join("dest");
        fs::create_dir_all(&dest_dir).unwrap();

        let protocol_version = 32;

        // Create basis file at destination
        let basis_data = vec![b'Z'; 2000];
        fs::write(dest_dir.join("data.bin"), &basis_data).unwrap();

        let patch = b"ZSTD_PATCHED_DATA";
        let output_size = 700 + 700 + patch.len() + 600;

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(42);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            do_compression: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol);

        // Directory entry
        let mut dir_entry = FileEntry::new_directory(".".into(), 0o755);
        dir_entry.set_mtime(1_700_000_000, 0);
        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &dir_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // File entry with output size
        let mut file_entry = FileEntry::new_file("data.bin".into(), output_size as u64, 0o644);
        file_entry.set_mtime(1_700_000_001, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &file_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // End of flist
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        // NDX-framed delta with zstd-compressed block matches and literals
        {
            use protocol::codec::{NdxCodec, NdxCodecEnum};

            let mut ndx_codec = NdxCodecEnum::new(protocol_version as u8);
            let mut ndx_buf = Vec::new();

            ndx_codec.write_ndx(&mut ndx_buf, 1).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();

            let block_length: i32 = 700;
            let block_count: i32 = 3;
            let remainder: i32 = 600;
            let s2length: i32 = 16;
            writer.write_data(&block_count.to_le_bytes()).unwrap();
            writer.write_data(&block_length.to_le_bytes()).unwrap();
            writer.write_data(&s2length.to_le_bytes()).unwrap();
            writer.write_data(&remainder.to_le_bytes()).unwrap();

            // Zstd encoder - see_token is noop, no dictionary sync needed
            let mut token_buf = Vec::new();
            let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();

            // Block 0: copy from basis
            encoder.send_block_match(&mut token_buf, 0).unwrap();
            encoder.see_token(&basis_data[0..700]).unwrap(); // noop for zstd

            // Block 1: copy from basis
            encoder.send_block_match(&mut token_buf, 1).unwrap();
            encoder.see_token(&basis_data[700..1400]).unwrap(); // noop for zstd

            // Literal patch
            encoder.send_literal(&mut token_buf, patch).unwrap();

            // Block 2: copy from basis (remainder)
            encoder.send_block_match(&mut token_buf, 2).unwrap();
            encoder.see_token(&basis_data[1400..2000]).unwrap(); // noop for zstd

            encoder.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();

            // File checksum
            writer.write_data(&[0u8; 16]).unwrap();

            // NDX_DONE phase 1 -> phase 2
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // NDX_DONE phase 2 -> end
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();
        }

        writer.finalize().unwrap();

        // Replay the zstd-compressed delta batch
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let result = crate::replay::replay(&read_config, &dest_dir, 0).unwrap();

        assert_eq!(result.file_count, 2);
        assert!(dest_dir.join("data.bin").exists());

        let content = fs::read(dest_dir.join("data.bin")).unwrap();
        assert_eq!(content.len(), output_size);
        assert_eq!(&content[0..700], &basis_data[0..700]);
        assert_eq!(&content[700..1400], &basis_data[700..1400]);
        assert_eq!(&content[1400..1400 + patch.len()], patch);
        assert_eq!(&content[1400 + patch.len()..], &basis_data[1400..2000]);
    }

    /// Verifies that compressed batch replay correctly resets the decoder
    /// between multiple files.
    ///
    /// Upstream rsync's `recv_deflated_token()` reinitializes the inflate
    /// context at the start of each file (r_init flag). If the decoder is
    /// not reset between files, the inflate state from the first file's
    /// compressed stream leaks into the second, corrupting decompression.
    ///
    /// This test exercises the exact code path in `replay.rs` where
    /// `decoder.reset()` is called before each file's token reading loop.
    ///
    /// upstream: token.c:496 - inflateReset per file
    /// upstream: token.c:recv_deflated_token() - r_init resets state
    #[test]
    fn test_replay_compressed_multi_file_resets_decoder() {
        use protocol::flist::{FileEntry, FileListWriter};
        use protocol::wire::CompressedTokenEncoder;

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("replay_multi_z.batch");
        let dest_dir = temp_dir.path().join("dest");
        fs::create_dir_all(&dest_dir).unwrap();

        let protocol_version = 31;
        let file1_data = b"First file content for multi-file batch test";
        let file2_data = b"Second file has entirely different compressed content";

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(42);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            do_compression: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol);

        // Directory entry
        let mut dir_entry = FileEntry::new_directory(".".into(), 0o755);
        dir_entry.set_mtime(1_700_000_000, 0);
        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &dir_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // File 1 entry
        let mut entry1 = FileEntry::new_file("file1.txt".into(), file1_data.len() as u64, 0o644);
        entry1.set_mtime(1_700_000_001, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &entry1).unwrap();
        writer.write_data(&buf).unwrap();

        // File 2 entry
        let mut entry2 = FileEntry::new_file("file2.txt".into(), file2_data.len() as u64, 0o644);
        entry2.set_mtime(1_700_000_002, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &entry2).unwrap();
        writer.write_data(&buf).unwrap();

        // End of flist
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        // NDX-framed delta data for both files
        {
            use protocol::codec::{NdxCodec, NdxCodecEnum};

            let mut ndx_codec = NdxCodecEnum::new(protocol_version as u8);
            let mut ndx_buf = Vec::new();

            // File 1: NDX=1, compressed literal
            ndx_codec.write_ndx(&mut ndx_buf, 1).unwrap();
            writer.write_data(&ndx_buf).unwrap();
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // block_count=0
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // block_length=0
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // s2length=0
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // remainder=0

            let mut token_buf = Vec::new();
            let mut encoder = CompressedTokenEncoder::default();
            encoder.set_zlibx(true);
            encoder.send_literal(&mut token_buf, file1_data).unwrap();
            encoder.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();
            writer.write_data(&[0u8; 16]).unwrap(); // file checksum

            // File 2: NDX=2, compressed literal
            ndx_buf.clear();
            ndx_codec.write_ndx(&mut ndx_buf, 2).unwrap();
            writer.write_data(&ndx_buf).unwrap();
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap();

            token_buf.clear();
            let mut encoder2 = CompressedTokenEncoder::default();
            encoder2.set_zlibx(true);
            encoder2.send_literal(&mut token_buf, file2_data).unwrap();
            encoder2.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();
            writer.write_data(&[0u8; 16]).unwrap(); // file checksum

            // NDX_DONE phase 1 -> phase 2
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // NDX_DONE phase 2 -> end
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();
        }

        writer.finalize().unwrap();

        // Replay
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let result = crate::replay::replay(&read_config, &dest_dir, 0).unwrap();

        assert_eq!(result.file_count, 3); // 1 dir + 2 files

        let content1 = fs::read(dest_dir.join("file1.txt")).unwrap();
        assert_eq!(
            content1, file1_data,
            "first file content should match after multi-file compressed replay"
        );

        let content2 = fs::read(dest_dir.join("file2.txt")).unwrap();
        assert_eq!(
            content2, file2_data,
            "second file content should match after decoder reset"
        );
    }

    /// Verifies compressed batch replay with multiple files where each file
    /// has CPRES_ZLIB block matches requiring dictionary synchronization.
    ///
    /// This is the most demanding compressed batch scenario: each file has
    /// a pre-existing basis at the destination, so the delta contains copy
    /// tokens. The decoder must reset between files AND correctly feed basis
    /// block data via see_token() for each file independently.
    ///
    /// This exercises the upstream bug scenario (token.c:608 inflate -3) for
    /// multiple files in sequence, verifying that oc-rsync handles it correctly.
    ///
    /// upstream: token.c:see_deflate_token() - per-file dictionary sync
    /// upstream: token.c:608 - inflate error -3 when dictionary sync missing
    #[test]
    fn test_replay_compressed_multi_file_with_block_matches() {
        use protocol::flist::{FileEntry, FileListWriter};
        use protocol::wire::CompressedTokenEncoder;

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("replay_multi_delta_z.batch");
        let dest_dir = temp_dir.path().join("dest");
        fs::create_dir_all(&dest_dir).unwrap();

        let protocol_version = 31;

        // Create basis files at destination
        let basis1 = vec![b'A'; 2000];
        let basis2 = vec![b'X'; 2000];
        fs::write(dest_dir.join("alpha.dat"), &basis1).unwrap();
        fs::write(dest_dir.join("beta.dat"), &basis2).unwrap();

        let patch1 = b"PATCH_FOR_ALPHA!";
        let patch2 = b"PATCH_FOR_BETA!!";
        let output1_size = 700 + patch1.len() + 700 + 600; // copy block0 + literal + copy block1 + copy block2
        let output2_size = 700 + 700 + patch2.len() + 600;

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(42);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            do_compression: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol);

        // Directory
        let mut dir_entry = FileEntry::new_directory(".".into(), 0o755);
        dir_entry.set_mtime(1_700_000_000, 0);
        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &dir_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // File 1
        let mut f1 = FileEntry::new_file("alpha.dat".into(), output1_size as u64, 0o644);
        f1.set_mtime(1_700_000_001, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &f1).unwrap();
        writer.write_data(&buf).unwrap();

        // File 2
        let mut f2 = FileEntry::new_file("beta.dat".into(), output2_size as u64, 0o644);
        f2.set_mtime(1_700_000_002, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &f2).unwrap();
        writer.write_data(&buf).unwrap();

        // End flist
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        // Delta data for both files with CPRES_ZLIB dictionary sync
        {
            use protocol::codec::{NdxCodec, NdxCodecEnum};

            let mut ndx_codec = NdxCodecEnum::new(protocol_version as u8);
            let block_length: i32 = 700;
            let block_count: i32 = 3;
            let remainder: i32 = 600;
            let s2length: i32 = 16;

            // --- File 1: copy block0 + literal + copy block1 + copy block2 ---
            let mut ndx_buf = Vec::new();
            ndx_codec.write_ndx(&mut ndx_buf, 1).unwrap();
            writer.write_data(&ndx_buf).unwrap();
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();
            writer.write_data(&block_count.to_le_bytes()).unwrap();
            writer.write_data(&block_length.to_le_bytes()).unwrap();
            writer.write_data(&s2length.to_le_bytes()).unwrap();
            writer.write_data(&remainder.to_le_bytes()).unwrap();

            let mut token_buf = Vec::new();
            let mut enc1 = CompressedTokenEncoder::default();
            enc1.set_zlibx(false); // CPRES_ZLIB

            enc1.send_block_match(&mut token_buf, 0).unwrap();
            enc1.see_token(&basis1[0..700]).unwrap();

            enc1.send_literal(&mut token_buf, patch1).unwrap();

            enc1.send_block_match(&mut token_buf, 1).unwrap();
            enc1.see_token(&basis1[700..1400]).unwrap();

            enc1.send_block_match(&mut token_buf, 2).unwrap();
            enc1.see_token(&basis1[1400..2000]).unwrap();

            enc1.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();
            writer.write_data(&[0u8; 16]).unwrap();

            // --- File 2: copy block0 + copy block1 + literal + copy block2 ---
            ndx_buf.clear();
            ndx_codec.write_ndx(&mut ndx_buf, 2).unwrap();
            writer.write_data(&ndx_buf).unwrap();
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();
            writer.write_data(&block_count.to_le_bytes()).unwrap();
            writer.write_data(&block_length.to_le_bytes()).unwrap();
            writer.write_data(&s2length.to_le_bytes()).unwrap();
            writer.write_data(&remainder.to_le_bytes()).unwrap();

            token_buf.clear();
            let mut enc2 = CompressedTokenEncoder::default();
            enc2.set_zlibx(false); // CPRES_ZLIB

            enc2.send_block_match(&mut token_buf, 0).unwrap();
            enc2.see_token(&basis2[0..700]).unwrap();

            enc2.send_block_match(&mut token_buf, 1).unwrap();
            enc2.see_token(&basis2[700..1400]).unwrap();

            enc2.send_literal(&mut token_buf, patch2).unwrap();

            enc2.send_block_match(&mut token_buf, 2).unwrap();
            enc2.see_token(&basis2[1400..2000]).unwrap();

            enc2.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();
            writer.write_data(&[0u8; 16]).unwrap();

            // NDX_DONE phase 1 -> phase 2
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // NDX_DONE phase 2 -> end
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();
        }

        writer.finalize().unwrap();

        // Replay
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let result = crate::replay::replay(&read_config, &dest_dir, 0).unwrap();

        assert_eq!(result.file_count, 3); // 1 dir + 2 files

        // Verify file 1: block0(700) + patch1(16) + block1(700) + block2(600)
        let content1 = fs::read(dest_dir.join("alpha.dat")).unwrap();
        assert_eq!(content1.len(), output1_size);
        assert_eq!(&content1[0..700], &basis1[0..700]);
        assert_eq!(&content1[700..700 + patch1.len()], &patch1[..]);
        assert_eq!(
            &content1[700 + patch1.len()..1400 + patch1.len()],
            &basis1[700..1400]
        );
        assert_eq!(&content1[1400 + patch1.len()..], &basis1[1400..2000]);

        // Verify file 2: block0(700) + block1(700) + patch2(16) + block2(600)
        let content2 = fs::read(dest_dir.join("beta.dat")).unwrap();
        assert_eq!(content2.len(), output2_size);
        assert_eq!(&content2[0..700], &basis2[0..700]);
        assert_eq!(&content2[700..1400], &basis2[700..1400]);
        assert_eq!(&content2[1400..1400 + patch2.len()], &patch2[..]);
        assert_eq!(&content2[1400 + patch2.len()..], &basis2[1400..2000]);
    }

    /// Verifies that compressed batch replay handles a mix of new files
    /// (no basis) and delta files (with basis) in the same batch.
    ///
    /// When a file has no pre-existing basis at the destination, the replay
    /// path uses `read_compressed_delta_tokens()` (eager mode, no dictionary
    /// sync needed) instead of the streaming CPRES_ZLIB path. This test
    /// verifies the transition between these two paths within a single
    /// batch file.
    ///
    /// upstream: receiver.c:receive_data() - basis_exists determines path
    #[test]
    fn test_replay_compressed_mixed_new_and_delta_files() {
        use protocol::flist::{FileEntry, FileListWriter};
        use protocol::wire::CompressedTokenEncoder;

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("replay_mixed_z.batch");
        let dest_dir = temp_dir.path().join("dest");
        fs::create_dir_all(&dest_dir).unwrap();

        let protocol_version = 31;

        // Only create basis for delta.dat - new.txt has no basis
        let basis_data = vec![b'D'; 2000];
        fs::write(dest_dir.join("delta.dat"), &basis_data).unwrap();

        let new_file_data = b"brand new file without basis";
        let patch_data = b"DELTA_PATCH_DATA";
        let delta_output_size = 700 + 700 + patch_data.len() + 600;

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(42);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            do_compression: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol);

        // Directory
        let mut dir_entry = FileEntry::new_directory(".".into(), 0o755);
        dir_entry.set_mtime(1_700_000_000, 0);
        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &dir_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // File 1: delta.dat (has basis)
        let mut f_delta = FileEntry::new_file("delta.dat".into(), delta_output_size as u64, 0o644);
        f_delta.set_mtime(1_700_000_001, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &f_delta).unwrap();
        writer.write_data(&buf).unwrap();

        // File 2: new.txt (no basis)
        let mut f_new = FileEntry::new_file("new.txt".into(), new_file_data.len() as u64, 0o644);
        f_new.set_mtime(1_700_000_002, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &f_new).unwrap();
        writer.write_data(&buf).unwrap();

        // End flist
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        // Delta data
        {
            use protocol::codec::{NdxCodec, NdxCodecEnum};

            let mut ndx_codec = NdxCodecEnum::new(protocol_version as u8);
            let block_length: i32 = 700;
            let block_count: i32 = 3;
            let remainder: i32 = 600;

            // --- delta.dat: CPRES_ZLIB with block matches ---
            let mut ndx_buf = Vec::new();
            ndx_codec.write_ndx(&mut ndx_buf, 1).unwrap();
            writer.write_data(&ndx_buf).unwrap();
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();
            writer.write_data(&block_count.to_le_bytes()).unwrap();
            writer.write_data(&block_length.to_le_bytes()).unwrap();
            writer.write_data(&16i32.to_le_bytes()).unwrap();
            writer.write_data(&remainder.to_le_bytes()).unwrap();

            let mut token_buf = Vec::new();
            let mut enc = CompressedTokenEncoder::default();
            enc.set_zlibx(false); // CPRES_ZLIB

            enc.send_block_match(&mut token_buf, 0).unwrap();
            enc.see_token(&basis_data[0..700]).unwrap();

            enc.send_block_match(&mut token_buf, 1).unwrap();
            enc.see_token(&basis_data[700..1400]).unwrap();

            enc.send_literal(&mut token_buf, patch_data).unwrap();

            enc.send_block_match(&mut token_buf, 2).unwrap();
            enc.see_token(&basis_data[1400..2000]).unwrap();

            enc.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();
            writer.write_data(&[0u8; 16]).unwrap();

            // --- new.txt: whole-file literal (no basis, uses eager path) ---
            ndx_buf.clear();
            ndx_codec.write_ndx(&mut ndx_buf, 2).unwrap();
            writer.write_data(&ndx_buf).unwrap();
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // block_count=0
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // block_length=0
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // s2length=0
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // remainder=0

            token_buf.clear();
            let mut enc2 = CompressedTokenEncoder::default();
            enc2.set_zlibx(true); // zlibx for whole-file
            enc2.send_literal(&mut token_buf, new_file_data).unwrap();
            enc2.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();
            writer.write_data(&[0u8; 16]).unwrap();

            // NDX_DONE phase 1 -> phase 2
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // NDX_DONE phase 2 -> end
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();
        }

        writer.finalize().unwrap();

        // Replay
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let result = crate::replay::replay(&read_config, &dest_dir, 0).unwrap();

        assert_eq!(result.file_count, 3);

        // Verify delta.dat
        let content_delta = fs::read(dest_dir.join("delta.dat")).unwrap();
        assert_eq!(content_delta.len(), delta_output_size);
        assert_eq!(&content_delta[0..700], &basis_data[0..700]);
        assert_eq!(&content_delta[700..1400], &basis_data[700..1400]);
        assert_eq!(
            &content_delta[1400..1400 + patch_data.len()],
            &patch_data[..]
        );
        assert_eq!(
            &content_delta[1400 + patch_data.len()..],
            &basis_data[1400..2000]
        );

        // Verify new.txt
        let content_new = fs::read(dest_dir.join("new.txt")).unwrap();
        assert_eq!(content_new, new_file_data);
    }

    /// Verifies compressed batch replay with CPRES_ZLIB mode for a whole-file
    /// transfer where no basis file exists at the destination.
    ///
    /// Upstream rsync writes batch tokens in CPRES_ZLIB mode (zlibx=false)
    /// regardless of whether the file has a basis or not. When the basis does
    /// not exist, the delta stream contains only literals (no block matches),
    /// so see_token() is never called. The decoder must still correctly inflate
    /// the literal data even in CPRES_ZLIB mode without any dictionary sync.
    ///
    /// This tests the code path in replay.rs where cpres_zlib=true but
    /// basis_exists=false, which falls through to read_compressed_delta_tokens()
    /// (eager mode). The decoder was created with set_zlibx(false) but works
    /// correctly because literal-only streams don't require dictionary sync.
    ///
    /// upstream: token.c:send_deflated_token() - uses CPRES_ZLIB for all files
    /// upstream: io.c:write_batch_monitor_in - tees compressed bytes to batch_fd
    #[test]
    fn test_replay_cpres_zlib_no_basis_whole_file() {
        use protocol::flist::{FileEntry, FileListWriter};
        use protocol::wire::CompressedTokenEncoder;

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("replay_zlib_no_basis.batch");
        let dest_dir = temp_dir.path().join("dest");
        fs::create_dir_all(&dest_dir).unwrap();

        let protocol_version = 30;
        let file_data = b"whole-file literal data in CPRES_ZLIB mode without basis";

        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        )
        .with_checksum_seed(42);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            do_compression: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        let protocol = protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
        let mut flist_writer = FileListWriter::new(protocol);

        // Directory entry
        let mut dir_entry = FileEntry::new_directory(".".into(), 0o755);
        dir_entry.set_mtime(1_700_000_000, 0);
        let mut buf = Vec::new();
        flist_writer.write_entry(&mut buf, &dir_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // File entry - no basis at destination
        let mut file_entry =
            FileEntry::new_file("newfile.txt".into(), file_data.len() as u64, 0o644);
        file_entry.set_mtime(1_700_000_001, 0);
        buf.clear();
        flist_writer.write_entry(&mut buf, &file_entry).unwrap();
        writer.write_data(&buf).unwrap();

        // End of flist
        let mut end_buf = Vec::new();
        flist_writer.write_end(&mut end_buf, None).unwrap();
        writer.write_data(&end_buf).unwrap();

        // NDX-framed delta: CPRES_ZLIB (zlibx=false) literal-only stream
        {
            use protocol::codec::{NdxCodec, NdxCodecEnum};

            let mut ndx_codec = NdxCodecEnum::new(protocol_version as u8);
            let mut ndx_buf = Vec::new();

            ndx_codec.write_ndx(&mut ndx_buf, 1).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // iflags: ITEM_TRANSFER (0x8000)
            writer.write_data(&0x8000u16.to_le_bytes()).unwrap();

            // sum_head: block_count=0 (whole-file, no basis)
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // block_count
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // block_length
            writer.write_data(&16i32.to_le_bytes()).unwrap(); // s2length
            writer.write_data(&0i32.to_le_bytes()).unwrap(); // remainder

            // Encode with CPRES_ZLIB mode (zlibx=false) - this is what
            // upstream rsync uses for all batch writes regardless of basis.
            let mut token_buf = Vec::new();
            let mut encoder = CompressedTokenEncoder::default();
            encoder.set_zlibx(false); // CPRES_ZLIB, not CPRES_ZLIBX
            encoder.send_literal(&mut token_buf, file_data).unwrap();
            encoder.finish(&mut token_buf).unwrap();
            writer.write_data(&token_buf).unwrap();

            // File checksum (16 zero bytes)
            writer.write_data(&[0u8; 16]).unwrap();

            // NDX_DONE phase 1 -> phase 2
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();

            // NDX_DONE phase 2 -> end
            ndx_buf.clear();
            ndx_codec.write_ndx_done(&mut ndx_buf).unwrap();
            writer.write_data(&ndx_buf).unwrap();
        }

        writer.finalize().unwrap();

        // Replay - no basis file at destination, cpres_zlib=true path
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            protocol_version,
        );

        let result = crate::replay::replay(&read_config, &dest_dir, 0).unwrap();

        assert_eq!(result.file_count, 2); // 1 dir + 1 file
        assert!(dest_dir.join("newfile.txt").exists());

        let content = fs::read(dest_dir.join("newfile.txt")).unwrap();
        assert_eq!(
            content, file_data,
            "CPRES_ZLIB literal-only stream must decompress correctly without basis"
        );
    }
}
