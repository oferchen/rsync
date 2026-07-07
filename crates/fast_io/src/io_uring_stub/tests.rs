//! Cross-cutting tests for the io_uring stub backend.
//!
//! Mirrors `crate::io_uring::tests`: validates that every stub entry point
//! reports unavailability and that the standard-I/O fallback paths still
//! produce byte-identical output across the supported policies.

use super::*;
use crate::IoUringPolicy;
use crate::io_uring_common::IoBackend;
use crate::traits::{FileReader, FileReaderFactory, FileWriter, FileWriterFactory};
use std::io::{self, Read, Write};
use tempfile::{NamedTempFile, tempdir};

#[test]
fn io_uring_unavailable_on_stub_platform() {
    assert!(!is_io_uring_available());
}

#[test]
fn buffer_ring_is_not_supported_on_stub() {
    assert!(!buffer_ring::is_supported());
}

#[test]
fn buffer_ring_try_new_returns_none_on_stub() {
    let config = BufferRingConfig::default();
    assert!(BufferRing::try_new(&(), config).is_none());
}

#[test]
fn buffer_ring_new_returns_error_on_stub() {
    let config = BufferRingConfig::default();
    let err: io::Error = BufferRing::new(&(), config).unwrap_err().into();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn buffer_ring_new_with_allocator_returns_error_on_stub() {
    let config = BufferRingConfig::default();
    let err: io::Error = BufferRing::new_with_allocator(&(), config)
        .unwrap_err()
        .into();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn bgid_allocator_reports_exhausted_on_stub() {
    let err = BgidAllocator::allocate().unwrap_err();
    assert!(matches!(err, BgidAllocError::Exhausted { .. }));
    let io_err: io::Error = err.into();
    assert_eq!(io_err.kind(), io::ErrorKind::OutOfMemory);
    assert_eq!(BgidAllocator::remaining(), 0);
    BgidAllocator::deallocate(0);
    BgidAllocator::deallocate(u16::MAX);
    assert_eq!(BgidAllocator::remaining(), 0);
}

#[test]
fn bgid_exhausted_count_is_zero_on_stub() {
    // The stub never advances a fresh-bgid counter, so the exposed
    // exhaustion counter must always read zero.
    assert_eq!(bgid_exhausted_count(), 0);
}

#[test]
fn buffer_id_from_cqe_flags_extracts_id_when_flag_set() {
    // Common helper returns `Some(buf_id)` when IORING_CQE_F_BUFFER is set.
    let flags = (1234u32 << 16) | 1;
    assert_eq!(buffer_id_from_cqe_flags(flags), Some(1234));
}

#[test]
fn buffer_id_from_cqe_flags_returns_none_when_flag_clear() {
    let no_flag = 1234u32 << 16;
    assert_eq!(buffer_id_from_cqe_flags(no_flag), None);
}

#[test]
fn buffer_ring_config_default_has_valid_values() {
    let config = BufferRingConfig::default();
    assert!(config.ring_size > 0);
    assert!(config.buffer_size > 0);
    assert_eq!(config.bgid, 0);
}

#[test]
fn registered_buffer_group_try_new_returns_none() {
    let result = RegisteredBufferGroup::try_new(&(), 4096, 4);
    assert!(result.is_none());
}

#[test]
fn registered_buffer_group_new_returns_unsupported() {
    let result = RegisteredBufferGroup::new(&(), 4096, 4);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
}

#[test]
fn disk_batch_try_new_returns_none() {
    let config = IoUringConfig::default();
    assert!(IoUringDiskBatch::try_new(&config).is_none());
}

#[test]
fn disk_batch_new_returns_unsupported() {
    let config = IoUringConfig::default();
    let result = IoUringDiskBatch::new(&config);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
}

#[test]
fn config_has_register_buffers_fields() {
    let config = IoUringConfig::default();
    assert!(config.register_buffers);
    assert_eq!(config.registered_buffer_count, 8);

    let large = IoUringConfig::for_large_files();
    assert!(large.register_buffers);
    assert_eq!(large.registered_buffer_count, 16);

    let small = IoUringConfig::for_small_files();
    assert!(small.register_buffers);
    assert_eq!(small.registered_buffer_count, 8);
}

#[test]
fn policy_disabled_writer_uses_std() {
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(b"").unwrap();
    let file = tmp.reopen().unwrap();

    let writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();
    assert!(matches!(writer, IoUringOrStdWriter::Std(_)));
}

#[test]
fn policy_disabled_reader_uses_std() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disabled_reader.txt");
    std::fs::write(&path, b"hello").unwrap();

    let reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
    assert!(matches!(reader, IoUringOrStdReader::Std(_)));
}

#[test]
fn policy_auto_falls_back_to_std_writer() {
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(b"").unwrap();
    let file = tmp.reopen().unwrap();

    let writer = writer_from_file(file, 8192, IoUringPolicy::Auto).unwrap();
    assert!(matches!(writer, IoUringOrStdWriter::Std(_)));
}

#[test]
fn policy_auto_falls_back_to_std_reader() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("auto_reader.txt");
    std::fs::write(&path, b"world").unwrap();

    let reader = reader_from_path(&path, IoUringPolicy::Auto).unwrap();
    assert!(matches!(reader, IoUringOrStdReader::Std(_)));
}

#[test]
fn policy_enabled_writer_returns_error() {
    let tmp = NamedTempFile::new().unwrap();
    let file = tmp.reopen().unwrap();

    let result = writer_from_file(file, 8192, IoUringPolicy::Enabled);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert!(err.to_string().contains("io_uring"));
}

#[test]
fn policy_enabled_reader_returns_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("enabled_reader.txt");
    std::fs::write(&path, b"data").unwrap();

    let result = reader_from_path(&path, IoUringPolicy::Enabled);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert!(err.to_string().contains("io_uring"));
}

#[test]
fn writer_parity_disabled_vs_auto() {
    let test_data: Vec<u8> = (0..4096).map(|i| ((i * 7 + 13) % 256) as u8).collect();

    let dir = tempdir().unwrap();
    let path_disabled = dir.path().join("parity_disabled.bin");
    {
        let file = std::fs::File::create(&path_disabled).unwrap();
        let mut writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();
        writer.write_all(&test_data).unwrap();
        writer.flush().unwrap();
    }

    let path_auto = dir.path().join("parity_auto.bin");
    {
        let file = std::fs::File::create(&path_auto).unwrap();
        let mut writer = writer_from_file(file, 8192, IoUringPolicy::Auto).unwrap();
        writer.write_all(&test_data).unwrap();
        writer.flush().unwrap();
    }

    let content_disabled = std::fs::read(&path_disabled).unwrap();
    let content_auto = std::fs::read(&path_auto).unwrap();

    assert_eq!(content_disabled.len(), test_data.len());
    assert_eq!(content_disabled, content_auto);
    assert_eq!(content_disabled, test_data);
}

#[test]
fn reader_parity_disabled_vs_auto() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("parity_read.bin");
    let test_data: Vec<u8> = (0..8192).map(|i| ((i * 11 + 3) % 256) as u8).collect();
    std::fs::write(&path, &test_data).unwrap();

    let mut reader_disabled = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
    let data_disabled = reader_disabled.read_all().unwrap();

    let mut reader_auto = reader_from_path(&path, IoUringPolicy::Auto).unwrap();
    let data_auto = reader_auto.read_all().unwrap();

    assert_eq!(data_disabled.len(), test_data.len());
    assert_eq!(data_disabled, data_auto);
    assert_eq!(data_disabled, test_data);
}

#[test]
fn writer_bytes_written_tracking() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bytes_tracking.bin");
    let file = std::fs::File::create(&path).unwrap();
    let mut writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();

    assert_eq!(writer.bytes_written(), 0);
    writer.write_all(b"hello").unwrap();
    assert_eq!(writer.bytes_written(), 5);
    writer.write_all(b" world").unwrap();
    assert_eq!(writer.bytes_written(), 11);
    writer.flush().unwrap();
    assert_eq!(writer.bytes_written(), 11);
}

#[test]
fn reader_size_and_position_tracking() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("position_tracking.bin");
    let data = b"abcdefghijklmnop";
    std::fs::write(&path, data).unwrap();

    let mut reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
    assert_eq!(reader.size(), 16);
    assert_eq!(reader.position(), 0);

    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf).unwrap();
    assert_eq!(reader.position(), 4);
    assert_eq!(reader.remaining(), 12);
}

#[test]
fn write_then_read_roundtrip_via_policy() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("roundtrip.bin");
    let test_data: Vec<u8> = (0..65536).map(|i| ((i * 17 + 5) % 256) as u8).collect();

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = writer_from_file(file, 16384, IoUringPolicy::Disabled).unwrap();
        writer.write_all(&test_data).unwrap();
        writer.flush().unwrap();
    }

    let mut reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
    let read_back = reader.read_all().unwrap();

    assert_eq!(read_back.len(), test_data.len());
    assert_eq!(read_back, test_data);
}

#[test]
fn factory_reader_forced_fallback_produces_std() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("factory_fallback.txt");
    std::fs::write(&path, b"factory test").unwrap();

    let factory = IoUringReaderFactory::default().force_fallback(true);
    assert!(!factory.will_use_io_uring());

    let reader = factory.open(&path).unwrap();
    assert!(matches!(reader, IoUringOrStdReader::Std(_)));
}

#[test]
fn factory_writer_forced_fallback_produces_std() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("factory_fallback_write.txt");

    let factory = IoUringWriterFactory::default().force_fallback(true);
    assert!(!factory.will_use_io_uring());

    let writer = factory.create(&path).unwrap();
    assert!(matches!(writer, IoUringOrStdWriter::Std(_)));
}

#[test]
fn policy_default_is_auto() {
    assert_eq!(IoUringPolicy::default(), IoUringPolicy::Auto);
}

#[cfg(unix)]
#[test]
fn socket_reader_disabled_policy_uses_std() {
    let (fd_a, fd_b) = {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is the two-int output slot `socketpair(2)` requires;
        // the call returns 0 and writes both fds on success.
        let ret =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(ret, 0);
        (fds[0], fds[1])
    };

    let reader = socket_reader_from_fd(fd_b, 8192, IoUringPolicy::Disabled).unwrap();
    assert!(matches!(reader, IoUringOrStdSocketReader::Std(_)));

    // SAFETY: `fd_a` and `fd_b` were just opened by `socketpair`; we close
    // each exactly once and do not reuse them afterwards.
    unsafe {
        libc::close(fd_a);
        libc::close(fd_b);
    }
}

#[cfg(unix)]
#[test]
fn socket_writer_disabled_policy_uses_std() {
    let (fd_a, fd_b) = {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is the two-int output slot `socketpair(2)` requires;
        // the call returns 0 and writes both fds on success.
        let ret =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(ret, 0);
        (fds[0], fds[1])
    };

    let writer = socket_writer_from_fd(fd_a, 8192, IoUringPolicy::Disabled).unwrap();
    assert!(matches!(writer, IoUringOrStdSocketWriter::Std(_)));

    // SAFETY: `fd_a` and `fd_b` were just opened by `socketpair`; we close
    // each exactly once and do not reuse them afterwards.
    unsafe {
        libc::close(fd_a);
        libc::close(fd_b);
    }
}

#[cfg(unix)]
#[test]
fn socket_enabled_policy_returns_error() {
    let (fd_a, fd_b) = {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is the two-int output slot `socketpair(2)` requires;
        // the call returns 0 and writes both fds on success.
        let ret =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(ret, 0);
        (fds[0], fds[1])
    };

    let reader_result = socket_reader_from_fd(fd_b, 8192, IoUringPolicy::Enabled);
    match reader_result {
        Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
        Ok(_) => panic!("expected Unsupported error for reader"),
    }

    let writer_result = socket_writer_from_fd(fd_a, 8192, IoUringPolicy::Enabled);
    match writer_result {
        Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
        Ok(_) => panic!("expected Unsupported error for writer"),
    }

    // SAFETY: `fd_a` and `fd_b` were just opened by `socketpair`; we close
    // each exactly once and do not reuse them afterwards.
    unsafe {
        libc::close(fd_a);
        libc::close(fd_b);
    }
}

// Unlike the IoUring-policy factory, the zero-copy factory must NEVER error on
// this build: SEND_ZC is unavailable (non-Linux or the `io_uring` feature is
// off), so `--zero-copy` degrades gracefully to the plain fd writer. The
// daemon-sender relies on this so an opted-in transfer still runs with
// byte-identical framing when the zero-copy transport is absent.
#[cfg(unix)]
#[test]
fn zero_copy_factory_degrades_to_std_for_every_policy() {
    use crate::ZeroCopyPolicy;

    let (fd_a, fd_b) = {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is the two-int output slot `socketpair(2)` requires;
        // the call returns 0 and writes both fds on success.
        let ret =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(ret, 0);
        (fds[0], fds[1])
    };

    for policy in [
        ZeroCopyPolicy::Auto,
        ZeroCopyPolicy::Disabled,
        ZeroCopyPolicy::Enabled,
    ] {
        let writer = socket_writer_from_fd_zero_copy(fd_a, 8192, policy)
            .expect("zero-copy factory never errors on this build");
        assert!(
            matches!(writer, IoUringOrStdSocketWriter::Std(_)),
            "SEND_ZC unavailable here: {policy:?} must yield the Std writer"
        );
    }

    // SAFETY: `fd_a` and `fd_b` were just opened by `socketpair`; we close
    // each exactly once and do not reuse them afterwards.
    unsafe {
        libc::close(fd_a);
        libc::close(fd_b);
    }
}

#[test]
fn stub_backend_reports_unavailable() {
    assert!(!StubIoUringBackend::is_available());
    assert!(!StubIoUringBackend::sqpoll_fell_back());
    assert!(StubIoUringBackend::availability_reason().contains("disabled"));
}
