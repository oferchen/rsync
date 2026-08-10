//! Tests for `try_splice_to_file`, `SplicePipe`, and probe behaviour on Linux.

use super::super::super::*;
use std::io::{Read, Seek, SeekFrom};
use tempfile::NamedTempFile;

#[test]
fn test_splice_pipe_creation() {
    let pipe = SplicePipe::new();
    assert!(pipe.is_ok(), "pipe2 should succeed");
    let pipe = pipe.unwrap();
    assert!(pipe.read_fd() >= 0);
    assert!(pipe.write_fd() >= 0);
    assert_ne!(pipe.read_fd(), pipe.write_fd());
    // Drop closes both fds
}

#[test]
fn test_splice_pipe_multiple_creates() {
    // Verify we can create and drop multiple pipes without fd leaks.
    for _ in 0..100 {
        let pipe = SplicePipe::new().unwrap();
        assert!(pipe.read_fd() >= 0);
        assert!(pipe.write_fd() >= 0);
    }
}

#[test]
fn test_splice_probe() {
    // On Linux, splice should be available (kernel >= 2.6.17).
    let supported = is_splice_available();
    // Modern CI kernels support splice; if not, the test is still valid.
    if !supported {
        eprintln!("splice not available on this kernel - skipping splice tests");
    }
}

#[test]
fn test_splice_socketpair_to_file() {
    if !is_splice_available() {
        return;
    }

    let content = b"Testing splice: socket to file transfer via pipe intermediary";
    let mut dest = NamedTempFile::new().unwrap();

    // Create a socket pair - one end writes, the other is the "socket" for splice.
    let mut socket_fds = [0i32; 2];
    // SAFETY: `socket_fds`/`fds` provides the two-int output slot the
    // `socketpair(2)` syscall fills on success.
    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create socketpair");

    let recv_fd = socket_fds[0]; // splice reads from this end
    let send_fd = socket_fds[1]; // we write test data to this end

    // Write test data into the send end.
    // SAFETY: the fd was opened just above and is still valid; the buffer
    // provides exactly the requested number of readable bytes.
    let written = unsafe {
        libc::write(
            send_fd,
            content.as_ptr().cast::<libc::c_void>(),
            content.len(),
        )
    };
    assert_eq!(written, content.len() as isize);

    // Close send end so splice sees EOF after the data.
    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(send_fd) };

    // Splice from recv_fd into the file.
    use std::os::fd::AsRawFd;
    let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len());

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };

    let spliced = spliced.unwrap();
    assert_eq!(spliced, content.len());

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}

#[test]
fn test_splice_large_transfer() {
    if !is_splice_available() {
        return;
    }

    let size = 512 * 1024; // 512KB - multiple splice chunks
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let mut dest = NamedTempFile::new().unwrap();

    let mut socket_fds = [0i32; 2];
    // SAFETY: `socket_fds`/`fds` provides the two-int output slot the
    // `socketpair(2)` syscall fills on success.
    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr()) };
    assert_eq!(result, 0, "Failed to create socketpair");

    let recv_fd = socket_fds[0];
    let send_fd = socket_fds[1];

    // Spawn writer thread to avoid socket buffer deadlock on large transfers.
    let content_clone = content.clone();
    let writer_thread = std::thread::spawn(move || {
        let mut offset = 0;
        while offset < content_clone.len() {
            // SAFETY: the fd was opened just above and is still valid; the buffer
            // provides exactly the requested number of readable bytes.
            let n = unsafe {
                libc::write(
                    send_fd,
                    content_clone[offset..].as_ptr().cast::<libc::c_void>(),
                    content_clone.len() - offset,
                )
            };
            assert!(n > 0, "write to socket failed");
            offset += n as usize;
        }
        // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
        // is closed exactly once here; no further use occurs after this call.
        unsafe { libc::close(send_fd) };
    });

    use std::os::fd::AsRawFd;
    let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), size).unwrap();

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };
    writer_thread.join().expect("writer thread should succeed");

    assert_eq!(spliced, size);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content.len(), content.len());
    assert_eq!(file_content, content);
}

#[test]
fn test_splice_empty_transfer() {
    if !is_splice_available() {
        return;
    }

    let mut dest = NamedTempFile::new().unwrap();

    let mut socket_fds = [0i32; 2];
    // SAFETY: `socket_fds`/`fds` provides the two-int output slot the
    // `socketpair(2)` syscall fills on success.
    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr()) };
    assert_eq!(result, 0);

    let recv_fd = socket_fds[0];
    let send_fd = socket_fds[1];

    // Close send end immediately - splice should return 0 (EOF).
    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(send_fd) };

    use std::os::fd::AsRawFd;
    let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), 1024).unwrap();

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };

    assert_eq!(spliced, 0);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert!(file_content.is_empty());
}

#[test]
fn test_splice_invalid_fd_returns_error() {
    if !is_splice_available() {
        return;
    }

    // Using -1 as fd should produce an error, not a panic.
    let result = try_splice_to_file(-1, -1, 1024);
    assert!(result.is_err());
}

#[test]
fn test_splice_exact_chunk_boundary() {
    if !is_splice_available() {
        return;
    }

    // Transfer exactly SPLICE_CHUNK_SIZE bytes to test boundary handling.
    let size = super::super::super::SPLICE_CHUNK_SIZE;
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let mut dest = NamedTempFile::new().unwrap();

    let mut socket_fds = [0i32; 2];
    // SAFETY: `socket_fds`/`fds` provides the two-int output slot the
    // `socketpair(2)` syscall fills on success.
    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr()) };
    assert_eq!(result, 0);

    let recv_fd = socket_fds[0];
    let send_fd = socket_fds[1];

    let content_clone = content.clone();
    let writer_thread = std::thread::spawn(move || {
        let mut offset = 0;
        while offset < content_clone.len() {
            // SAFETY: the fd was opened just above and is still valid; the buffer
            // provides exactly the requested number of readable bytes.
            let n = unsafe {
                libc::write(
                    send_fd,
                    content_clone[offset..].as_ptr().cast::<libc::c_void>(),
                    content_clone.len() - offset,
                )
            };
            assert!(n > 0, "write to socket failed");
            offset += n as usize;
        }
        // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
        // is closed exactly once here; no further use occurs after this call.
        unsafe { libc::close(send_fd) };
    });

    use std::os::fd::AsRawFd;
    let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), size).unwrap();

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };
    writer_thread.join().expect("writer thread should succeed");

    assert_eq!(spliced, size);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}

#[test]
fn test_splice_pipe_with_capacity() {
    let pipe = SplicePipe::with_capacity(1024 * 1024).unwrap();
    // The kernel may round up or cap the value, but capacity should
    // be at least the default (64KB on most kernels).
    assert!(pipe.capacity() >= 64 * 1024);
    assert!(pipe.read_fd() >= 0);
    assert!(pipe.write_fd() >= 0);
    assert_ne!(pipe.read_fd(), pipe.write_fd());
}

#[test]
fn test_splice_pipe_default_capacity() {
    let pipe = SplicePipe::new().unwrap();
    // Default pipe capacity is 64KB on most Linux kernels.
    assert!(pipe.capacity() > 0);
}

#[test]
fn test_splice_pipe_reuse() {
    if !is_splice_available() {
        return;
    }

    let pipe = SplicePipe::with_capacity(DEFAULT_PIPE_CAPACITY).unwrap();

    // Perform two sequential transfers through the same pipe.
    for i in 0u8..2 {
        let content: Vec<u8> = (0..128u8).map(|j| j.wrapping_add(i * 64)).collect();
        let mut dest = NamedTempFile::new().unwrap();

        let (recv_fd, writer) = super::socketpair_with_writer(content.clone());

        use std::os::fd::AsRawFd;
        let spliced = pipe
            .splice_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len())
            .unwrap();

        // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
        // is closed exactly once here; no further use occurs after this call.
        unsafe { libc::close(recv_fd) };
        writer.join().expect("writer thread should succeed");

        assert_eq!(spliced, content.len());

        dest.seek(SeekFrom::Start(0)).unwrap();
        let mut file_content = Vec::new();
        dest.read_to_end(&mut file_content).unwrap();
        assert_eq!(file_content, content);
    }
}
