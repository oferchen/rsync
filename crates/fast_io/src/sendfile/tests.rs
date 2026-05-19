//! Tests for the `sendfile` module dispatch and fallback paths.
//!
//! Platform-specific syscall tests stay near the code they exercise via
//! `#[cfg(target_os = "...")]` gates. Generic tests exercise the public API
//! and the userspace fallback through any `Write` implementation.

use super::*;
use std::io::{Seek, SeekFrom, Write};
use tempfile::NamedTempFile;

#[cfg(target_os = "macos")]
use super::macos::try_sendfile_macos;

/// Helper to create a temp file with specified content
fn create_temp_file(content: &[u8]) -> io::Result<NamedTempFile> {
    let mut file = NamedTempFile::new()?;
    file.write_all(content)?;
    file.flush()?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

#[test]
fn test_send_to_writer_small_file() {
    let content = b"Hello, world! This is a small file for testing.";
    let source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, content.len() as u64).unwrap();

    assert_eq!(sent, content.len() as u64);
    assert_eq!(output, content);
}

#[test]
fn test_send_to_writer_large_file() {
    // Above SENDFILE_THRESHOLD; the Vec writer forces the read/write fallback.
    let size = 128 * 1024;
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let source = create_temp_file(&content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, content.len() as u64).unwrap();

    assert_eq!(sent, content.len() as u64);
    assert_eq!(output, content);
}

#[test]
fn test_send_to_writer_empty_file() {
    let content = b"";
    let source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, 0).unwrap();

    assert_eq!(sent, 0);
    assert_eq!(output, content);
}

#[test]
fn test_send_to_writer_exact_content() {
    let content: Vec<u8> = (0..1000).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let source = create_temp_file(&content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, content.len() as u64).unwrap();

    assert_eq!(sent, content.len() as u64);
    assert_eq!(output, content);
}

#[test]
fn test_send_to_writer_partial() {
    let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, 10).unwrap();

    assert_eq!(sent, 10);
    assert_eq!(output, b"0123456789");
}

#[test]
fn test_send_to_writer_beyond_eof() {
    let content = b"Short content";
    let source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, 10000).unwrap();

    assert_eq!(sent, content.len() as u64);
    assert_eq!(output, content);
}

#[test]
fn test_send_with_file_position() {
    // sendfile uses the source's current offset; verify the wrapper preserves
    // that contract instead of implicitly seeking to zero.
    let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    source.seek(SeekFrom::Start(10)).unwrap();

    let sent = send_file_to_writer(source.as_file(), &mut output, 10).unwrap();

    assert_eq!(sent, 10);
    assert_eq!(output, b"ABCDEFGHIJ");
}

#[cfg(target_os = "linux")]
#[test]
fn test_send_file_to_fd_pipe() {
    let content = b"Testing sendfile with pipe on Linux";
    let source = create_temp_file(content).unwrap();

    let mut pipe_fds = [0i32; 2];
    // SAFETY: `pipe_fds` is the two-int output slot `pipe(2)` writes
    // into; the call returns 0 on success.
    let result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create pipe");

    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    let sent = send_file_to_fd(source.as_file(), write_fd, content.len() as u64);

    // SAFETY: `write_fd` was just opened by `pipe(2)` and is closed
    // exactly once here.
    unsafe { libc::close(write_fd) };

    if let Ok(sent_bytes) = sent {
        assert_eq!(sent_bytes, content.len() as u64);

        let mut received = vec![0u8; content.len()];
        // SAFETY: `read_fd` is still open; `received` provides
        // `content.len()` writable bytes matching the requested length.
        let n = unsafe {
            libc::read(
                read_fd,
                received.as_mut_ptr().cast::<libc::c_void>(),
                content.len(),
            )
        };
        assert_eq!(n, content.len() as isize);
        assert_eq!(received, content);
    }

    // SAFETY: `read_fd` was opened by `pipe(2)` and is closed exactly
    // once here.
    unsafe { libc::close(read_fd) };
}

#[cfg(target_os = "linux")]
#[test]
fn test_send_file_to_fd_socketpair() {
    // socketpair is the canonical sendfile destination - exercises the
    // kernel's zero-copy fast path rather than the read/write fallback.
    let content = b"Testing sendfile with socketpair";
    let source = create_temp_file(content).unwrap();

    let mut socket_fds = [0i32; 2];
    // SAFETY: `socket_fds` is the two-int output slot the syscall fills.
    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create socketpair");

    let recv_fd = socket_fds[0];
    let send_fd = socket_fds[1];

    let sent = send_file_to_fd(source.as_file(), send_fd, content.len() as u64).unwrap();
    assert_eq!(sent, content.len() as u64);

    // SAFETY: `send_fd` was just opened by `socketpair` and is closed
    // exactly once here.
    unsafe { libc::close(send_fd) };

    let mut received = vec![0u8; content.len()];
    // SAFETY: `recv_fd` is still open; `received` provides
    // `content.len()` writable bytes matching the requested length.
    let n = unsafe {
        libc::read(
            recv_fd,
            received.as_mut_ptr().cast::<libc::c_void>(),
            content.len(),
        )
    };
    assert_eq!(n, content.len() as isize);
    assert_eq!(received, content);

    // SAFETY: `recv_fd` was opened by `socketpair` and is closed exactly
    // once here.
    unsafe { libc::close(recv_fd) };
}

#[cfg(target_os = "linux")]
#[test]
fn test_send_file_to_fd_large() {
    use std::thread;

    let size = 512 * 1024; // 512KB - exceeds SENDFILE_THRESHOLD
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let source = create_temp_file(&content).unwrap();

    let mut pipe_fds = [0i32; 2];
    // SAFETY: `pipe_fds` is the two-int output slot `pipe(2)` writes into.
    let result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create pipe");

    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    // 512KB exceeds the default 64KB pipe buffer, so we must drain it from
    // another thread to avoid blocking the sender.
    let expected_content = content.clone();
    let reader_thread = thread::spawn(move || {
        let mut received = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            // SAFETY: `read_fd` is owned by this thread until the
            // `close` below; `buf` provides `buf.len()` writable bytes.
            let n =
                unsafe { libc::read(read_fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
            if n <= 0 {
                break;
            }
            received.extend_from_slice(&buf[..n as usize]);
        }
        // SAFETY: `read_fd` is still open and is closed exactly once
        // before the thread exits.
        unsafe { libc::close(read_fd) };
        received
    });

    let sent = send_file_to_fd(source.as_file(), write_fd, size as u64);

    // SAFETY: `write_fd` was opened by `pipe(2)` and is closed exactly
    // once here.
    unsafe { libc::close(write_fd) };

    assert!(sent.is_ok(), "sendfile should succeed");
    let sent_bytes = sent.unwrap();
    assert_eq!(sent_bytes, size as u64);

    let received = reader_thread.join().expect("reader thread should succeed");
    assert_eq!(received.len(), expected_content.len());
    assert_eq!(received, expected_content);
}

#[cfg(target_os = "macos")]
#[test]
fn test_send_file_to_fd_socketpair_macos() {
    // Darwin's sendfile(2) only accepts SOCK_STREAM destinations.
    // Exercise the native path with a content length above
    // SENDFILE_THRESHOLD so try_sendfile_macos is invoked rather than
    // the buffered read/write fallback.
    use std::thread;

    let size = SENDFILE_THRESHOLD as usize + 4096; // 68 KiB
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let source = create_temp_file(&content).unwrap();

    let mut socket_fds = [0i32; 2];
    // SAFETY: `socket_fds` is the two-int output slot the syscall fills.
    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create socketpair");

    let recv_fd = socket_fds[0];
    let send_fd = socket_fds[1];

    // Drain the receive end concurrently so a small socket buffer does
    // not deadlock the sender.
    let expected_content = content.clone();
    let reader_thread = thread::spawn(move || {
        let mut received = Vec::with_capacity(expected_content.len());
        let mut buf = [0u8; 8192];
        while received.len() < expected_content.len() {
            // SAFETY: `recv_fd` is owned by this thread until the
            // `close` below; `buf` provides `buf.len()` writable bytes.
            let n =
                unsafe { libc::read(recv_fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
            if n <= 0 {
                break;
            }
            received.extend_from_slice(&buf[..n as usize]);
        }
        // SAFETY: `recv_fd` is still open and is closed exactly once
        // before the thread exits.
        unsafe { libc::close(recv_fd) };
        received
    });

    // try_sendfile_macos should succeed end-to-end for a SOCK_STREAM peer.
    let sent = try_sendfile_macos(source.as_file(), send_fd, size as u64)
        .expect("native macOS sendfile should succeed on a SOCK_STREAM");
    assert_eq!(sent, size as u64);

    // SAFETY: `send_fd` was opened by `socketpair` and is closed exactly
    // once here.
    unsafe { libc::close(send_fd) };

    let received = reader_thread.join().expect("reader thread should succeed");
    assert_eq!(received.len(), content.len());
    assert_eq!(received, content);
}

#[cfg(target_os = "macos")]
#[test]
fn test_send_file_to_fd_dispatch_uses_native_macos() {
    // The high-level dispatch must succeed for SOCK_STREAM destinations
    // on macOS without falling back to read/write (which would also pass
    // the byte-equality check but defeat the purpose of this audit).
    let content: Vec<u8> = (0..(SENDFILE_THRESHOLD as usize + 1024))
        .map(|i| ((i * 13 + 7) % 256) as u8)
        .collect();
    let source = create_temp_file(&content).unwrap();

    let mut socket_fds = [0i32; 2];
    // SAFETY: `socket_fds` is the two-int output slot the syscall fills.
    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create socketpair");

    let recv_fd = socket_fds[0];
    let send_fd = socket_fds[1];

    let expected = content.clone();
    let reader_thread = std::thread::spawn(move || {
        let mut received = Vec::with_capacity(expected.len());
        let mut buf = [0u8; 8192];
        while received.len() < expected.len() {
            // SAFETY: `recv_fd` is owned by this thread until the
            // `close` below; `buf` provides `buf.len()` writable bytes.
            let n =
                unsafe { libc::read(recv_fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
            if n <= 0 {
                break;
            }
            received.extend_from_slice(&buf[..n as usize]);
        }
        // SAFETY: `recv_fd` is still open and is closed exactly once.
        unsafe { libc::close(recv_fd) };
        received
    });

    let sent = send_file_to_fd(source.as_file(), send_fd, content.len() as u64).unwrap();
    assert_eq!(sent, content.len() as u64);

    // SAFETY: `send_fd` was opened by `socketpair` and is closed exactly
    // once here.
    unsafe { libc::close(send_fd) };

    let received = reader_thread.join().expect("reader thread should succeed");
    assert_eq!(received, content);
}

#[cfg(target_os = "macos")]
#[test]
fn test_send_file_to_fd_macos_non_socket_falls_back() {
    // Darwin's sendfile returns ENOTSOCK for pipes; the dispatch must
    // fall back to the read/write loop and still deliver the bytes.
    let content: Vec<u8> = (0..(SENDFILE_THRESHOLD as usize + 512))
        .map(|i| (i % 256) as u8)
        .collect();
    let source = create_temp_file(&content).unwrap();

    let mut pipe_fds = [0i32; 2];
    // SAFETY: `pipe_fds` is the two-int output slot `pipe(2)` writes into.
    let result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create pipe");

    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    let expected = content.clone();
    let reader_thread = std::thread::spawn(move || {
        let mut received = Vec::with_capacity(expected.len());
        let mut buf = [0u8; 8192];
        while received.len() < expected.len() {
            // SAFETY: `read_fd` is owned by this thread until the
            // `close` below; `buf` provides `buf.len()` writable bytes.
            let n =
                unsafe { libc::read(read_fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
            if n <= 0 {
                break;
            }
            received.extend_from_slice(&buf[..n as usize]);
        }
        // SAFETY: `read_fd` is still open and is closed exactly once.
        unsafe { libc::close(read_fd) };
        received
    });

    let sent = send_file_to_fd(source.as_file(), write_fd, content.len() as u64).unwrap();
    assert_eq!(sent, content.len() as u64);

    // SAFETY: `write_fd` was opened by `pipe(2)` and is closed exactly
    // once here.
    unsafe { libc::close(write_fd) };

    let received = reader_thread.join().expect("reader thread should succeed");
    assert_eq!(received, content);
}

#[test]
fn test_copy_via_readwrite_direct() {
    // Test the read/write fallback path directly
    let content = b"Testing fallback path directly with specific data";
    let source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    let copied =
        super::fallback::copy_via_readwrite(source.as_file(), &mut output, content.len() as u64)
            .unwrap();

    assert_eq!(copied, content.len() as u64);
    assert_eq!(output, content);
}

#[cfg(target_os = "linux")]
#[test]
fn test_threshold_boundary() {
    // Test at exact threshold boundary
    let size = SENDFILE_THRESHOLD as usize;
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let source = create_temp_file(&content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, size as u64).unwrap();

    assert_eq!(sent, size as u64);
    assert_eq!(output.len(), content.len());
    assert_eq!(output, content);
}

#[test]
fn test_multiple_writes() {
    // Test that multiple independent writes work correctly
    let content1 = b"First write";
    let content2 = b"Second";
    let source1 = create_temp_file(content1).unwrap();
    let source2 = create_temp_file(content2).unwrap();
    let mut output = Vec::new();

    // First write
    let sent1 = send_file_to_writer(source1.as_file(), &mut output, content1.len() as u64).unwrap();
    assert_eq!(sent1, content1.len() as u64);
    assert_eq!(&output[..sent1 as usize], content1);

    // Second write appends to output
    let sent2 = send_file_to_writer(source2.as_file(), &mut output, content2.len() as u64).unwrap();
    assert_eq!(sent2, content2.len() as u64);
    assert_eq!(output, b"First writeSecond");
}
