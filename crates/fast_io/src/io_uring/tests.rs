//! Tests for io_uring file and socket I/O.

use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::path::Path;

use tempfile::tempdir;

use super::config::{IoUringConfig, is_io_uring_available, parse_kernel_version};
use super::file_factory::{
    IoUringOrStdReader, IoUringOrStdWriter, IoUringReaderFactory, IoUringWriterFactory,
};
use super::file_reader::IoUringReader;
use super::file_writer::IoUringWriter;
use super::socket_factory::{
    FdReader, FdWriter, IoUringOrStdSocketReader, IoUringOrStdSocketWriter, socket_reader_from_fd,
    socket_writer_from_fd,
};
use super::{read_file, write_file};
use crate::traits::{FileReader, FileReaderFactory, FileWriter, FileWriterFactory};

#[test]
fn test_kernel_version_parsing() {
    assert_eq!(parse_kernel_version("5.15.0-generic"), Some((5, 15)));
    assert_eq!(parse_kernel_version("6.1.0"), Some((6, 1)));
    assert_eq!(parse_kernel_version("4.19.123-aws"), Some((4, 19)));
    assert_eq!(parse_kernel_version("invalid"), None);
}

#[test]
fn test_io_uring_availability_check() {
    let available = is_io_uring_available();
    println!("io_uring available: {available}");
}

#[test]
fn test_io_uring_config_defaults() {
    let config = IoUringConfig::default();
    assert_eq!(config.sq_entries, 64);
    assert_eq!(config.buffer_size, 64 * 1024);
    assert!(!config.direct_io);
}

#[test]
fn test_io_uring_config_presets() {
    let large = IoUringConfig::for_large_files();
    assert_eq!(large.sq_entries, 256);
    assert_eq!(large.buffer_size, 256 * 1024);

    let small = IoUringConfig::for_small_files();
    assert_eq!(small.sq_entries, 128);
    assert_eq!(small.buffer_size, 16 * 1024);
}

#[test]
fn test_reader_factory_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, b"hello world").unwrap();

    let factory = IoUringReaderFactory::default().force_fallback(true);
    assert!(!factory.will_use_io_uring());

    let mut reader = factory.open(&path).unwrap();
    assert!(matches!(reader, IoUringOrStdReader::Std(_)));

    let data = reader.read_all().unwrap();
    assert_eq!(data, b"hello world");
}

#[test]
fn test_writer_factory_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.txt");

    let factory = IoUringWriterFactory::default().force_fallback(true);
    assert!(!factory.will_use_io_uring());

    let mut writer = factory.create(&path).unwrap();
    assert!(matches!(writer, IoUringOrStdWriter::Std(_)));

    writer.write_all(b"hello world").unwrap();
    writer.flush().unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
}

#[test]
fn test_convenience_functions_with_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.txt");

    write_file(&path, b"test data").unwrap();
    let data = read_file(&path).unwrap();
    assert_eq!(data, b"test data");
}

// ─────────────────────────────────────────────────────────────────────────
// Tests that run only when io_uring is actually available
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_io_uring_reader_if_available() {
    if !is_io_uring_available() {
        println!("Skipping io_uring reader test: not available");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, b"hello from io_uring").unwrap();

    let config = IoUringConfig::default();
    let mut reader = IoUringReader::open(&path, &config).unwrap();

    assert_eq!(reader.size(), 19);
    assert_eq!(reader.position(), 0);

    let data = reader.read_all().unwrap();
    assert_eq!(data, b"hello from io_uring");
}

#[test]
fn test_io_uring_writer_if_available() {
    if !is_io_uring_available() {
        println!("Skipping io_uring writer test: not available");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("test.txt");

    let config = IoUringConfig::default();
    let mut writer = IoUringWriter::create(&path, &config).unwrap();

    writer.write_all(b"hello from io_uring").unwrap();
    writer.sync().unwrap();

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "hello from io_uring"
    );
}

#[test]
fn test_io_uring_factory_uses_io_uring_when_available() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, b"test").unwrap();

    let factory = IoUringReaderFactory::default();
    let reader = factory.open(&path).unwrap();

    if is_io_uring_available() {
        assert!(matches!(reader, IoUringOrStdReader::IoUring(_)));
    } else {
        assert!(matches!(reader, IoUringOrStdReader::Std(_)));
    }
}

#[test]
fn test_io_uring_read_at() {
    if !is_io_uring_available() {
        println!("Skipping io_uring read_at test: not available");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, b"hello world").unwrap();

    let config = IoUringConfig::default();
    let mut reader = IoUringReader::open(&path, &config).unwrap();

    let mut buf = [0u8; 5];
    let n = reader.read_at(6, &mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"world");

    assert_eq!(reader.position(), 0);
}

#[test]
fn test_io_uring_write_at() {
    if !is_io_uring_available() {
        println!("Skipping io_uring write_at test: not available");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("test.txt");

    let config = IoUringConfig::default();
    let mut writer = IoUringWriter::create(&path, &config).unwrap();

    writer.write_at(0, b"hello").unwrap();
    writer.write_at(6, b"world").unwrap();
    writer.flush().unwrap();

    let content = std::fs::read(&path).unwrap();
    assert_eq!(&content[0..5], b"hello");
    assert_eq!(&content[6..11], b"world");
}

#[test]
fn test_reader_seek() {
    if !is_io_uring_available() {
        println!("Skipping io_uring seek test: not available");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, b"hello world").unwrap();

    let config = IoUringConfig::default();
    let mut reader = IoUringReader::open(&path, &config).unwrap();

    reader.seek_to(6).unwrap();
    assert_eq!(reader.position(), 6);

    let mut buf = [0u8; 5];
    reader.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"world");
}

// ─────────────────────────────────────────────────────────────────────────
// Comprehensive io_uring tests with graceful fallback
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_basic_read_with_io_uring_or_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("read_test.txt");
    let test_data = b"The quick brown fox jumps over the lazy dog";
    std::fs::write(&path, test_data).unwrap();

    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(&path).unwrap();

    let data = reader.read_all().unwrap();
    assert_eq!(data, test_data);
    assert_eq!(reader.size(), test_data.len() as u64);
}

#[test]
fn test_basic_write_with_io_uring_or_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("write_test.txt");
    let test_data = b"Hello, io_uring world!";

    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(&path).unwrap();

    writer.write_all(test_data).unwrap();
    writer.flush().unwrap();

    let written = std::fs::read(&path).unwrap();
    assert_eq!(written, test_data);
    assert_eq!(writer.bytes_written(), test_data.len() as u64);
}

#[test]
fn test_large_file_read_with_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("large_read.bin");

    let chunk_size = 1024;
    let num_chunks = 1024;
    let mut expected_data = Vec::with_capacity(chunk_size * num_chunks);
    for i in 0..num_chunks {
        let pattern = (i % 256) as u8;
        expected_data.extend(std::iter::repeat_n(pattern, chunk_size));
    }
    std::fs::write(&path, &expected_data).unwrap();

    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(&path).unwrap();

    let data = reader.read_all().unwrap();
    assert_eq!(data.len(), expected_data.len());
    assert_eq!(data, expected_data);
}

#[test]
fn test_large_file_write_with_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("large_write.bin");

    let chunk_size = 1024;
    let num_chunks = 512;
    let mut test_data = Vec::with_capacity(chunk_size * num_chunks);
    for i in 0..num_chunks {
        let pattern = (i % 256) as u8;
        test_data.extend(std::iter::repeat_n(pattern, chunk_size));
    }

    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(&path).unwrap();

    for chunk in test_data.chunks(chunk_size) {
        writer.write_all(chunk).unwrap();
    }
    writer.sync().unwrap();

    let written = std::fs::read(&path).unwrap();
    assert_eq!(written.len(), test_data.len());
    assert_eq!(written, test_data);
}

#[test]
fn test_forced_fallback_to_standard_io() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("fallback_test.txt");
    let test_data = b"Testing forced fallback";
    std::fs::write(&path, test_data).unwrap();

    let factory = IoUringReaderFactory::default().force_fallback(true);
    assert!(!factory.will_use_io_uring());

    let mut reader = factory.open(&path).unwrap();
    assert!(matches!(reader, IoUringOrStdReader::Std(_)));

    let data = reader.read_all().unwrap();
    assert_eq!(data, test_data);
}

#[test]
fn test_writer_forced_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("fallback_write.txt");
    let test_data = b"Forced fallback write";

    let factory = IoUringWriterFactory::default().force_fallback(true);
    assert!(!factory.will_use_io_uring());

    let mut writer = factory.create(&path).unwrap();
    assert!(matches!(writer, IoUringOrStdWriter::Std(_)));

    writer.write_all(test_data).unwrap();
    writer.flush().unwrap();

    let written = std::fs::read(&path).unwrap();
    assert_eq!(written, test_data);
}

#[test]
fn test_reader_partial_reads() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("partial_read.txt");
    std::fs::write(&path, b"0123456789ABCDEF").unwrap();

    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(&path).unwrap();

    let mut buf = [0u8; 3];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(&buf, b"012");

    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(&buf, b"345");

    reader.seek_to(10).unwrap();
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(&buf, b"ABC");
}

#[test]
fn test_writer_buffering() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("buffering_test.txt");

    let _config = IoUringConfig {
        sq_entries: 32,
        buffer_size: 128,
        direct_io: false,
        register_files: true,
        sqpoll: false,
        sqpoll_idle_ms: 1000,
    };

    let factory = IoUringWriterFactory::default().force_fallback(true);
    let mut writer = factory.create(&path).unwrap();

    let data = b"x".repeat(256);
    writer.write_all(&data).unwrap();

    assert_eq!(writer.bytes_written(), 256);

    writer.flush().unwrap();

    let written = std::fs::read(&path).unwrap();
    assert_eq!(written.len(), 256);
}

#[test]
fn test_writer_sync() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("sync_test.txt");

    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(&path).unwrap();

    writer.write_all(b"sync test").unwrap();
    writer.sync().unwrap();

    let written = std::fs::read(&path).unwrap();
    assert_eq!(written, b"sync test");
}

#[test]
fn test_writer_preallocate() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("preallocate_test.txt");

    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create_with_size(&path, 1024).unwrap();

    writer.write_all(b"prealloc").unwrap();
    writer.flush().unwrap();

    let metadata = std::fs::metadata(&path).unwrap();
    assert_eq!(metadata.len(), 1024);
}

#[test]
fn test_read_empty_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("empty.txt");
    std::fs::write(&path, b"").unwrap();

    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(&path).unwrap();

    assert_eq!(reader.size(), 0);
    let data = reader.read_all().unwrap();
    assert_eq!(data.len(), 0);
}

#[test]
fn test_read_at_eof() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("eof_test.txt");
    std::fs::write(&path, b"short").unwrap();

    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(&path).unwrap();

    reader.seek_to(5).unwrap();
    assert_eq!(reader.position(), 5);

    let mut buf = [0u8; 10];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn test_seek_beyond_eof_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("seek_error.txt");
    std::fs::write(&path, b"data").unwrap();

    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(&path).unwrap();

    let result = reader.seek_to(100);
    assert!(result.is_err());
}

#[test]
fn test_concurrent_operations_with_fallback() {
    use std::sync::Arc;
    use std::thread;

    let dir = Arc::new(tempdir().unwrap());
    let test_data = b"concurrent test data";

    let handles: Vec<_> = (0..4)
        .map(|i| {
            let dir = Arc::clone(&dir);
            let data = test_data.to_vec();
            thread::spawn(move || {
                let path = dir.path().join(format!("thread_{i}.txt"));

                let factory = IoUringWriterFactory::default();
                let mut writer = factory.create(&path).unwrap();
                writer.write_all(&data).unwrap();
                writer.sync().unwrap();

                let factory = IoUringReaderFactory::default();
                let mut reader = factory.open(&path).unwrap();
                let read_data = reader.read_all().unwrap();

                assert_eq!(read_data, data);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_convenience_functions() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("convenience.txt");
    let test_data = b"convenience function test";

    write_file(&path, test_data).unwrap();

    let data = read_file(&path).unwrap();
    assert_eq!(data, test_data);
}

#[test]
fn test_multiple_sequential_operations() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("sequential.txt");

    let factory = IoUringWriterFactory::default();

    {
        let mut writer = factory.create(&path).unwrap();
        writer.write_all(b"first").unwrap();
        writer.flush().unwrap();
    }

    let factory_read = IoUringReaderFactory::default();
    {
        let mut reader = factory_read.open(&path).unwrap();
        let data = reader.read_all().unwrap();
        assert_eq!(data, b"first");
    }

    {
        let mut writer = factory.create(&path).unwrap();
        writer.write_all(b"second write").unwrap();
        writer.flush().unwrap();
    }

    {
        let mut reader = factory_read.open(&path).unwrap();
        let data = reader.read_all().unwrap();
        assert_eq!(data, b"second write");
    }
}

#[test]
fn test_config_presets() {
    let large = IoUringConfig::for_large_files();
    assert!(large.sq_entries >= 128);
    assert!(large.buffer_size >= 128 * 1024);

    let small = IoUringConfig::for_small_files();
    assert!(small.buffer_size <= 32 * 1024);
}

#[test]
fn test_factory_with_custom_config() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("custom_config.txt");
    std::fs::write(&path, b"custom").unwrap();

    let config = IoUringConfig {
        sq_entries: 32,
        buffer_size: 4096,
        direct_io: false,
        register_files: true,
        sqpoll: false,
        sqpoll_idle_ms: 1000,
    };

    let factory = IoUringReaderFactory::with_config(config);
    let mut reader = factory.open(&path).unwrap();
    let data = reader.read_all().unwrap();
    assert_eq!(data, b"custom");
}

#[test]
fn test_error_handling_nonexistent_file() {
    let factory = IoUringReaderFactory::default();
    let result = factory.open(Path::new("/nonexistent/path/file.txt"));
    assert!(result.is_err());
}

#[test]
fn test_error_handling_permission_denied() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let path = dir.path().join("readonly.txt");
    std::fs::write(&path, b"data").unwrap();

    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o200);
    fs::set_permissions(&path, perms).unwrap();

    let factory = IoUringReaderFactory::default();
    let result = factory.open(&path);
    assert!(result.is_err());

    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&path, perms).unwrap();
}

#[test]
fn test_queue_depth_limits() {
    if !is_io_uring_available() {
        println!("Skipping queue depth test: io_uring not available");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("queue_test.txt");

    let config = IoUringConfig {
        sq_entries: 4,
        buffer_size: 1024,
        direct_io: false,
        register_files: true,
        sqpoll: false,
        sqpoll_idle_ms: 1000,
    };

    let mut writer = IoUringWriter::create(&path, &config).unwrap();
    let data = b"x".repeat(8192);
    writer.write_all(&data).unwrap();
    writer.flush().unwrap();

    let written = std::fs::read(&path).unwrap();
    assert_eq!(written.len(), data.len());
}

#[test]
fn test_reader_remaining() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("remaining.txt");
    std::fs::write(&path, b"0123456789").unwrap();

    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(&path).unwrap();

    assert_eq!(reader.remaining(), 10);

    let mut buf = [0u8; 3];
    reader.read_exact(&mut buf).unwrap();
    assert_eq!(reader.remaining(), 7);

    reader.seek_to(8).unwrap();
    assert_eq!(reader.remaining(), 2);
}

#[test]
fn test_write_zero_bytes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("zero_write.txt");

    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(&path).unwrap();

    let n = writer.write(b"").unwrap();
    assert_eq!(n, 0);
    assert_eq!(writer.bytes_written(), 0);

    writer.flush().unwrap();
    let written = std::fs::read(&path).unwrap();
    assert_eq!(written.len(), 0);
}

#[test]
fn test_io_uring_reader_read_all_batched() {
    if !is_io_uring_available() {
        println!("Skipping batched read test: io_uring not available");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("batched.txt");

    let size = 256 * 1024;
    let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    std::fs::write(&path, &data).unwrap();

    let config = IoUringConfig {
        sq_entries: 64,
        buffer_size: 64 * 1024,
        direct_io: false,
        register_files: true,
        sqpoll: false,
        sqpoll_idle_ms: 1000,
    };

    let mut reader = IoUringReader::open(&path, &config).unwrap();
    let read_data = reader.read_all_batched().unwrap();

    assert_eq!(read_data.len(), data.len());
    assert_eq!(read_data, data);
}

#[test]
fn test_io_uring_batched_read_small_sq() {
    if !is_io_uring_available() {
        println!("Skipping batched read small-sq test: io_uring not available");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("batched_small_sq.bin");

    // 128 KB file with 4 SQ entries and 8 KB buffers = 4 batches of 4 reads
    let size = 128 * 1024;
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &data).unwrap();

    let config = IoUringConfig {
        sq_entries: 4,
        buffer_size: 8 * 1024,
        direct_io: false,
        register_files: true,
        sqpoll: false,
        sqpoll_idle_ms: 1000,
    };

    let mut reader = IoUringReader::open(&path, &config).unwrap();
    let read_data = reader.read_all_batched().unwrap();

    assert_eq!(read_data.len(), data.len());
    assert_eq!(read_data, data);
}

#[test]
fn test_io_uring_batched_write() {
    if !is_io_uring_available() {
        println!("Skipping batched write test: io_uring not available");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("batched_write.bin");

    // Write 512 KB in one shot via write_all_batched
    let size = 512 * 1024;
    let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

    let config = IoUringConfig {
        sq_entries: 32,
        buffer_size: 64 * 1024,
        direct_io: false,
        register_files: true,
        sqpoll: false,
        sqpoll_idle_ms: 1000,
    };

    let mut writer = IoUringWriter::create(&path, &config).unwrap();
    writer.write_all_batched(&data, 0).unwrap();
    writer.flush().unwrap();

    let written = std::fs::read(&path).unwrap();
    assert_eq!(written.len(), data.len());
    assert_eq!(written, data);
}

#[test]
fn test_io_uring_large_file_batched_roundtrip() {
    if !is_io_uring_available() {
        println!("Skipping large batched roundtrip: io_uring not available");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("roundtrip.bin");

    // 2 MB file
    let size = 2 * 1024 * 1024;
    let data: Vec<u8> = (0..size).map(|i| ((i * 7 + 3) % 256) as u8).collect();

    let config = IoUringConfig {
        sq_entries: 64,
        buffer_size: 64 * 1024,
        direct_io: false,
        register_files: true,
        sqpoll: false,
        sqpoll_idle_ms: 1000,
    };

    {
        let mut writer = IoUringWriter::create(&path, &config).unwrap();
        writer.write_all(&data).unwrap();
        writer.sync().unwrap();
    }

    {
        let mut reader = IoUringReader::open(&path, &config).unwrap();
        let read_data = reader.read_all_batched().unwrap();
        assert_eq!(read_data.len(), data.len());
        assert_eq!(read_data, data);
    }
}

#[test]
fn test_binary_data_integrity() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("binary.bin");

    let data: Vec<u8> = (0..=255).cycle().take(4096).collect();

    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(&path).unwrap();
    writer.write_all(&data).unwrap();
    writer.flush().unwrap();
    drop(writer);

    let factory_read = IoUringReaderFactory::default();
    let mut reader = factory_read.open(&path).unwrap();
    let read_data = reader.read_all().unwrap();

    assert_eq!(read_data.len(), data.len());
    assert_eq!(read_data, data);
}

#[test]
fn test_drop_flushes_writer() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("drop_flush.txt");

    {
        let factory = IoUringWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();
        writer.write_all(b"data to flush on drop").unwrap();
    }

    let written = std::fs::read(&path).unwrap();
    assert_eq!(written, b"data to flush on drop");
}

// ─────────────────────────────────────────────────────────────────────────
// Socket I/O tests (exercises io_uring path on Linux, fallback elsewhere)
// ─────────────────────────────────────────────────────────────────────────

/// Creates a Unix socket pair suitable for testing socket read/write.
fn make_socket_pair() -> (RawFd, RawFd) {
    let mut fds = [0i32; 2];
    let ret = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    assert_eq!(ret, 0, "socketpair failed");
    (fds[0], fds[1])
}

/// Closes a raw file descriptor.
fn close_fd(fd: RawFd) {
    unsafe {
        libc::close(fd);
    }
}

#[test]
fn test_socket_reader_writer_roundtrip() {
    let (fd_a, fd_b) = make_socket_pair();
    let policy = crate::IoUringPolicy::Auto;

    let mut writer = socket_writer_from_fd(fd_a, 64 * 1024, policy).unwrap();
    let mut reader = socket_reader_from_fd(fd_b, 64 * 1024, policy).unwrap();

    let payload = b"hello from io_uring socket writer";
    writer.write_all(payload).unwrap();
    writer.flush().unwrap();

    let mut buf = vec![0u8; payload.len()];
    reader.read_exact(&mut buf).unwrap();
    assert_eq!(buf, payload);

    drop(writer);
    close_fd(fd_a);
    close_fd(fd_b);
}

#[test]
fn test_socket_large_payload_roundtrip() {
    let (fd_a, fd_b) = make_socket_pair();
    let policy = crate::IoUringPolicy::Auto;

    let mut writer = socket_writer_from_fd(fd_a, 8 * 1024, policy).unwrap();
    let mut reader = socket_reader_from_fd(fd_b, 8 * 1024, policy).unwrap();

    // 128KB payload — larger than internal buffer, forces multiple batches.
    let payload: Vec<u8> = (0..128 * 1024).map(|i| (i % 251) as u8).collect();

    // Write in a separate thread to avoid deadlock on blocking socket pair.
    let payload_clone = payload.clone();
    let write_thread = std::thread::spawn(move || {
        writer.write_all(&payload_clone).unwrap();
        writer.flush().unwrap();
        drop(writer);
        close_fd(fd_a);
    });

    let mut received = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => received.extend_from_slice(&buf[..n]),
            Err(e) => panic!("read error: {e}"),
        }
    }
    close_fd(fd_b);

    write_thread.join().unwrap();
    assert_eq!(received.len(), payload.len());
    assert_eq!(received, payload);
}

#[test]
fn test_socket_reader_disabled_policy() {
    let (fd_a, fd_b) = make_socket_pair();
    let reader = socket_reader_from_fd(fd_b, 64 * 1024, crate::IoUringPolicy::Disabled).unwrap();
    assert!(matches!(reader, IoUringOrStdSocketReader::Std(_)));
    close_fd(fd_a);
    close_fd(fd_b);
}

#[test]
fn test_socket_writer_disabled_policy() {
    let (fd_a, fd_b) = make_socket_pair();
    let writer = socket_writer_from_fd(fd_a, 64 * 1024, crate::IoUringPolicy::Disabled).unwrap();
    assert!(matches!(writer, IoUringOrStdSocketWriter::Std(_)));
    close_fd(fd_a);
    close_fd(fd_b);
}

#[test]
fn test_fd_reader_writer_basic() {
    let (fd_a, fd_b) = make_socket_pair();

    let mut writer = FdWriter(fd_a);
    let mut reader = FdReader(fd_b);

    writer.write_all(b"fd adapter test").unwrap();
    writer.flush().unwrap();

    let mut buf = vec![0u8; 15];
    reader.read_exact(&mut buf).unwrap();
    assert_eq!(buf, b"fd adapter test");

    close_fd(fd_a);
    close_fd(fd_b);
}
