//! Integration tests for batch mode.

#[cfg(test)]
mod integration {
    use crate::batch::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
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
        let config = BatchConfig::new(BatchMode::Write, "test".to_string(), 30);
        assert!(config.is_write_mode());
        assert!(!config.is_read_mode());
        assert!(config.should_transfer());

        let config2 = BatchConfig::new(BatchMode::OnlyWrite, "test".to_string(), 30);
        assert!(config2.is_write_mode());
        assert!(!config2.is_read_mode());
        assert!(!config2.should_transfer());

        let config3 = BatchConfig::new(BatchMode::Read, "test".to_string(), 30);
        assert!(!config3.is_write_mode());
        assert!(config3.is_read_mode());
        assert!(config3.should_transfer());
    }

    #[test]
    fn test_batch_script_path() {
        let config = BatchConfig::new(BatchMode::Write, "mybatch".to_string(), 30);
        assert_eq!(config.script_file_path(), "mybatch.sh");

        let config2 = BatchConfig::new(
            BatchMode::Write,
            "/tmp/test.batch".to_string(),
            30,
        );
        assert_eq!(config2.script_file_path(), "/tmp/test.batch.sh");
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
}
