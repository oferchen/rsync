//! Tests for sendfile and fallback transfer paths.

use super::*;
use std::io::{Seek, SeekFrom, Write};
use tempfile::NamedTempFile;

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
    // Small file (< 64KB) should use read/write path directly
    let content = b"Hello, world! This is a small file for testing.";
    let source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, content.len() as u64).unwrap();

    assert_eq!(sent, content.len() as u64);
    assert_eq!(output, content);
}

#[test]
fn test_send_to_writer_large_file() {
    // Large file (>= 64KB) should trigger sendfile attempt (but falls back for Vec)
    let size = 128 * 1024; // 128KB
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let source = create_temp_file(&content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, content.len() as u64).unwrap();

    assert_eq!(sent, content.len() as u64);
    assert_eq!(output, content);
}

#[test]
fn test_send_to_writer_empty_file() {
    // Empty file should work without errors
    let content = b"";
    let source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, 0).unwrap();

    assert_eq!(sent, 0);
    assert_eq!(output, content);
}

#[test]
fn test_send_to_writer_exact_content() {
    // Verify data integrity with specific pattern
    let content: Vec<u8> = (0..1000).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let source = create_temp_file(&content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, content.len() as u64).unwrap();

    assert_eq!(sent, content.len() as u64);
    assert_eq!(output, content);
}

#[test]
fn test_send_to_writer_partial() {
    // Request fewer bytes than available
    let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, 10).unwrap();

    assert_eq!(sent, 10);
    assert_eq!(output, b"0123456789");
}

#[test]
fn test_send_to_writer_beyond_eof() {
    // Request more bytes than available - should stop at EOF
    let content = b"Short content";
    let source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    let sent = send_file_to_writer(source.as_file(), &mut output, 10000).unwrap();

    assert_eq!(sent, content.len() as u64);
    assert_eq!(output, content);
}

#[test]
fn test_send_with_file_position() {
    // Test that file position is respected
    let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    // Seek source to position 10
    source.seek(SeekFrom::Start(10)).unwrap();

    // Send 10 bytes from position 10
    let sent = send_file_to_writer(source.as_file(), &mut output, 10).unwrap();

    assert_eq!(sent, 10);
    assert_eq!(output, b"ABCDEFGHIJ");
}

#[cfg(target_os = "linux")]
#[test]
fn test_send_file_to_fd_pipe() {
    // Test sendfile to a pipe (should work on Linux)
    let content = b"Testing sendfile with pipe on Linux";
    let source = create_temp_file(content).unwrap();

    // Create a pipe for testing
    let mut pipe_fds = [0i32; 2];
    let result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create pipe");

    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    // Send data through sendfile to pipe
    let sent = send_file_to_fd(source.as_file(), write_fd, content.len() as u64);

    // Close write end
    unsafe { libc::close(write_fd) };

    if let Ok(sent_bytes) = sent {
        assert_eq!(sent_bytes, content.len() as u64);

        // Read from pipe to verify content
        let mut received = vec![0u8; content.len()];
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

    // Close read end
    unsafe { libc::close(read_fd) };
}

#[cfg(target_os = "linux")]
#[test]
fn test_send_file_to_fd_socketpair() {
    // Test sendfile to a socket (ideal use case)
    let content = b"Testing sendfile with socketpair";
    let source = create_temp_file(content).unwrap();

    // Create a socket pair for testing
    let mut socket_fds = [0i32; 2];
    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create socketpair");

    let recv_fd = socket_fds[0];
    let send_fd = socket_fds[1];

    // Send data through sendfile to socket
    let sent = send_file_to_fd(source.as_file(), send_fd, content.len() as u64).unwrap();

    assert_eq!(sent, content.len() as u64);

    // Close send end to signal EOF
    unsafe { libc::close(send_fd) };

    // Read from socket to verify content
    let mut received = vec![0u8; content.len()];
    let n = unsafe {
        libc::read(
            recv_fd,
            received.as_mut_ptr().cast::<libc::c_void>(),
            content.len(),
        )
    };
    assert_eq!(n, content.len() as isize);
    assert_eq!(received, content);

    // Close receive end
    unsafe { libc::close(recv_fd) };
}

#[cfg(target_os = "linux")]
#[test]
fn test_send_file_to_fd_large() {
    use std::thread;

    // Test large file transfer via sendfile
    let size = 512 * 1024; // 512KB - well above threshold
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let source = create_temp_file(&content).unwrap();

    // Create a pipe for testing
    let mut pipe_fds = [0i32; 2];
    let result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create pipe");

    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    // Spawn reader thread to avoid pipe buffer deadlock
    let expected_content = content.clone();
    let reader_thread = thread::spawn(move || {
        let mut received = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n =
                unsafe { libc::read(read_fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
            if n <= 0 {
                break;
            }
            received.extend_from_slice(&buf[..n as usize]);
        }
        unsafe { libc::close(read_fd) };
        received
    });

    // Send data through sendfile (main thread)
    let sent = send_file_to_fd(source.as_file(), write_fd, size as u64);

    // Close write end to signal EOF to reader
    unsafe { libc::close(write_fd) };

    // Verify send succeeded
    assert!(sent.is_ok(), "sendfile should succeed");
    let sent_bytes = sent.unwrap();
    assert_eq!(sent_bytes, size as u64);

    // Wait for reader and verify content
    let received = reader_thread.join().expect("reader thread should succeed");
    assert_eq!(received.len(), expected_content.len());
    assert_eq!(received, expected_content);
}

#[test]
fn test_copy_via_readwrite_direct() {
    use fallback::copy_via_readwrite;

    // Test the read/write fallback path directly
    let content = b"Testing fallback path directly with specific data";
    let source = create_temp_file(content).unwrap();
    let mut output = Vec::new();

    let copied = copy_via_readwrite(source.as_file(), &mut output, content.len() as u64).unwrap();

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
