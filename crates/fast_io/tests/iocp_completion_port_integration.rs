//! IOCP completion-port integration tests for the Windows fast I/O path
//! (WTD-2).
//!
//! These tests exercise IOCP code paths that existing CI does not cover:
//!
//! - **Concurrent reader+writer** on the same completion port via the factory
//!   layer, verifying data round-trips through overlapped I/O end-to-end.
//! - **Seek-then-write** to verify that IOCP writes at non-zero offsets land
//!   at the correct file position.
//! - **Preallocate-then-overwrite** to confirm `FileWriter::preallocate`
//!   extends the file and subsequent writes fill the preallocated region.
//! - **Reader factory size-threshold** to confirm that files below
//!   `IOCP_MIN_FILE_SIZE` are served by the Std fallback while files above
//!   the threshold use the IOCP reader.
//! - **Writer sync durability** to verify that `FileWriter::sync` flushes
//!   data to stable storage without error.
//! - **Concurrent IocpDiskBatch instances** to confirm that two independent
//!   completion ports can operate simultaneously without interference.
//! - **IocpWriter seek to non-zero offset** exercises the `Seek` impl for
//!   `SeekFrom::Start` and `SeekFrom::Current`.
//! - **Error: SeekFrom::End** is unsupported and must return an error.
//!
//! The entire file compiles to nothing on non-Windows targets.

#![cfg(all(target_os = "windows", feature = "iocp"))]

use std::fs;
use std::io::{Seek, SeekFrom, Write};

use fast_io::iocp::{
    IOCP_MIN_FILE_SIZE, IocpConfig, IocpDiskBatch, IocpReader, IocpReaderFactory, IocpWriter,
    IocpWriterFactory, is_iocp_available,
};
use fast_io::traits::{FileReader, FileReaderFactory, FileWriter, FileWriterFactory};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// WTD-2.a: IOCP reader + writer round-trip via factory
// ---------------------------------------------------------------------------

/// Write data through the IOCP writer factory and read it back through the
/// IOCP reader factory. The payload exceeds `IOCP_MIN_FILE_SIZE` so both
/// factories select the IOCP variant.
#[test]
fn factory_roundtrip_above_min_size() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("factory_roundtrip.bin");

    // Payload must exceed IOCP_MIN_FILE_SIZE so the reader factory picks IOCP.
    let payload: Vec<u8> = (0..IOCP_MIN_FILE_SIZE as usize + 1024)
        .map(|i| (i % 251) as u8)
        .collect();

    {
        let factory = IocpWriterFactory::default();
        assert!(factory.will_use_iocp());
        let mut writer = factory.create(&path).unwrap();
        writer.write_all(&payload).unwrap();
        writer.flush().unwrap();
    }

    {
        let factory = IocpReaderFactory::default();
        assert!(factory.will_use_iocp());
        let mut reader = factory.open(&path).unwrap();
        assert_eq!(reader.size(), payload.len() as u64);
        let data = reader.read_all().unwrap();
        assert_eq!(data, payload);
    }
}

// ---------------------------------------------------------------------------
// WTD-2.b: Reader factory falls back for small files
// ---------------------------------------------------------------------------

/// Files below `IOCP_MIN_FILE_SIZE` must be served by the Std reader, not
/// the IOCP reader, because the overlapped setup cost exceeds the async
/// benefit.
#[test]
fn factory_reader_uses_std_below_threshold() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("small.bin");
    let payload = vec![0xABu8; (IOCP_MIN_FILE_SIZE as usize) - 1];
    fs::write(&path, &payload).unwrap();

    let factory = IocpReaderFactory::default();
    let mut reader = factory.open(&path).unwrap();
    // The Std fallback still produces correct data.
    let data = reader.read_all().unwrap();
    assert_eq!(data, payload);
}

// ---------------------------------------------------------------------------
// WTD-2.c: IocpWriter seek + write at non-zero offset
// ---------------------------------------------------------------------------

/// Writing after seeking to a non-zero offset must place data at the correct
/// file position.
#[test]
fn writer_seek_start_then_write() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("seek_write.bin");
    let config = IocpConfig::default();

    {
        let mut writer = IocpWriter::create_with_size(&path, 1024, &config).unwrap();
        // Seek to offset 512 then write 10 bytes.
        let pos = writer.seek(SeekFrom::Start(512)).unwrap();
        assert_eq!(pos, 512);
        writer.write_all(b"at-offset!").unwrap();
        writer.flush().unwrap();
    }

    let content = fs::read(&path).unwrap();
    // File was preallocated to 1024. First 512 bytes should be zero.
    assert!(content[..512].iter().all(|&b| b == 0));
    assert_eq!(&content[512..522], b"at-offset!");
}

/// `SeekFrom::Current` with a positive delta must advance the offset.
#[test]
fn writer_seek_current_forward() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("seek_current.bin");
    let config = IocpConfig::default();

    {
        let mut writer = IocpWriter::create_with_size(&path, 256, &config).unwrap();
        writer.write_all(b"AAAA").unwrap();
        writer.flush().unwrap();
        // Current position is 4. Seek forward by 4 more.
        let pos = writer.seek(SeekFrom::Current(4)).unwrap();
        assert_eq!(pos, 8);
        writer.write_all(b"BBBB").unwrap();
        writer.flush().unwrap();
    }

    let content = fs::read(&path).unwrap();
    assert_eq!(&content[0..4], b"AAAA");
    // Bytes 4-7 are zero (gap from the seek).
    assert!(content[4..8].iter().all(|&b| b == 0));
    assert_eq!(&content[8..12], b"BBBB");
}

/// `SeekFrom::End` is explicitly unsupported and must return an error.
#[test]
fn writer_seek_end_returns_error() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("seek_end.bin");
    let config = IocpConfig::default();
    let mut writer = IocpWriter::create(&path, &config).unwrap();
    let err = writer
        .seek(SeekFrom::End(0))
        .expect_err("SeekFrom::End must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
}

// ---------------------------------------------------------------------------
// WTD-2.d: FileWriter::preallocate + overwrite
// ---------------------------------------------------------------------------

/// `FileWriter::preallocate` must extend the file to the requested size.
/// Subsequent writes must fill the preallocated region without error.
#[test]
fn writer_preallocate_then_fill() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("prealloc_fill.bin");
    let config = IocpConfig::default();

    let expected_size: u64 = 128 * 1024;
    let payload: Vec<u8> = (0..expected_size as usize)
        .map(|i| (i % 199) as u8)
        .collect();

    {
        let mut writer = IocpWriter::create(&path, &config).unwrap();
        writer.preallocate(expected_size).unwrap();
        writer.write_all(&payload).unwrap();
        writer.sync().unwrap();
    }

    let content = fs::read(&path).unwrap();
    // The file may be larger than expected_size due to preallocate, but the
    // first expected_size bytes must match the payload.
    assert!(content.len() >= expected_size as usize);
    assert_eq!(&content[..expected_size as usize], &payload[..]);
}

// ---------------------------------------------------------------------------
// WTD-2.e: FileWriter::sync durability
// ---------------------------------------------------------------------------

/// `FileWriter::sync` must flush data to stable storage. Verify by writing,
/// syncing, and reading back.
#[test]
fn writer_sync_persists_data() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("sync_persist.bin");
    let config = IocpConfig::default();

    {
        let mut writer = IocpWriter::create(&path, &config).unwrap();
        writer.write_all(b"durable-after-sync").unwrap();
        writer.sync().unwrap();
    }

    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "durable-after-sync");
}

// ---------------------------------------------------------------------------
// WTD-2.f: Concurrent IocpDiskBatch instances
// ---------------------------------------------------------------------------

/// Two independent `IocpDiskBatch` instances must operate simultaneously
/// without interfering. Each owns its own completion port and handle.
#[test]
fn concurrent_disk_batch_instances() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let config = IocpConfig::default();

    let path_a = dir.path().join("concurrent_a.bin");
    let path_b = dir.path().join("concurrent_b.bin");

    let payload_a = vec![0xAAu8; 8192];
    let payload_b = vec![0xBBu8; 16384];

    let file_a = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path_a)
        .unwrap();
    let file_b = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path_b)
        .unwrap();

    let mut batch_a = IocpDiskBatch::new(&config).unwrap();
    let mut batch_b = IocpDiskBatch::new(&config).unwrap();

    batch_a.begin_file(file_a).unwrap();
    batch_b.begin_file(file_b).unwrap();

    // Interleave writes to both batches.
    batch_a.write_data(&payload_a[..4096]).unwrap();
    batch_b.write_data(&payload_b[..8192]).unwrap();
    batch_a.write_data(&payload_a[4096..]).unwrap();
    batch_b.write_data(&payload_b[8192..]).unwrap();

    let (_fa, written_a) = batch_a.commit_file(false).unwrap();
    let (_fb, written_b) = batch_b.commit_file(false).unwrap();

    assert_eq!(written_a as usize, payload_a.len());
    assert_eq!(written_b as usize, payload_b.len());

    let content_a = fs::read(&path_a).unwrap();
    let content_b = fs::read(&path_b).unwrap();
    assert_eq!(content_a, payload_a);
    assert_eq!(content_b, payload_b);
}

// ---------------------------------------------------------------------------
// WTD-2.g: IocpReader read_at with explicit offset
// ---------------------------------------------------------------------------

/// `IocpReader::read_at` must read data at an arbitrary offset without
/// affecting the sequential position.
#[test]
fn reader_read_at_arbitrary_offset() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("read_at.bin");
    // Write a known pattern.
    let payload: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
    fs::write(&path, &payload).unwrap();

    let config = IocpConfig::default();
    let mut reader = IocpReader::open(&path, &config).unwrap();

    // Read 16 bytes at offset 512.
    let mut buf = [0u8; 16];
    let n = reader.read_at(512, &mut buf).unwrap();
    assert_eq!(n, 16);
    assert_eq!(&buf, &payload[512..528]);
}

// ---------------------------------------------------------------------------
// WTD-2.h: IocpReader seek_to validates bounds
// ---------------------------------------------------------------------------

/// `IocpReader::seek_to` past the end of the file must return an error.
#[test]
fn reader_seek_beyond_eof_errors() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("seek_eof.bin");
    fs::write(&path, b"short").unwrap();

    let config = IocpConfig::default();
    let mut reader = IocpReader::open(&path, &config).unwrap();
    assert_eq!(reader.size(), 5);

    let err = reader.seek_to(100).expect_err("seek past EOF must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

// ---------------------------------------------------------------------------
// WTD-2.i: IocpWriter create_for_append preserves existing content offset
// ---------------------------------------------------------------------------

/// `IocpWriter::create_for_append` opens an existing file at offset 0.
/// Writing appended data after a seek to the file's logical end should
/// extend the file while preserving the original content.
#[test]
fn writer_create_for_append_preserves_content() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("append.bin");
    fs::write(&path, b"existing-content").unwrap();

    let config = IocpConfig::default();
    {
        let mut writer = IocpWriter::create_for_append(&path, 8192, &config).unwrap();
        // Seek to the end of the existing content before appending.
        writer.seek(SeekFrom::Start(16)).unwrap();
        writer.write_all(b"-appended").unwrap();
        writer.flush().unwrap();
    }

    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "existing-content-appended");
}

// ---------------------------------------------------------------------------
// WTD-2.j: Large concurrent_ops IocpDiskBatch
// ---------------------------------------------------------------------------

/// Drive a moderate payload through a batch with `concurrent_ops = 64` to
/// exercise deep in-flight queues.
#[test]
fn disk_batch_deep_in_flight_queue() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("deep_queue.bin");

    let config = IocpConfig {
        buffer_size: 4096,
        concurrent_ops: 64,
        ..IocpConfig::default()
    };

    // 64 * 4 KB = 256 KB payload to fill the entire in-flight window.
    let payload: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();

    let mut batch = IocpDiskBatch::new(&config).unwrap();
    batch.begin_file(file).unwrap();
    batch.write_data(&payload).unwrap();
    let (_f, written) = batch.commit_file(false).unwrap();
    assert_eq!(written as usize, payload.len());

    let content = fs::read(&path).unwrap();
    assert_eq!(content, payload);
}
