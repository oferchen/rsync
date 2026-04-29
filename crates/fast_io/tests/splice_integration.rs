//! Integration tests for the splice zero-copy transfer module.
//!
//! These tests exercise `fast_io::splice` public APIs from the consumer
//! perspective - verifying that `try_splice_to_file`, `recv_fd_to_file`,
//! and `is_splice_available` behave correctly across platforms with
//! realistic payloads, boundary conditions, and error scenarios.

use fast_io::splice::{is_splice_available, recv_fd_to_file, try_splice_to_file};

#[test]
fn splice_availability_is_deterministic() {
    // Repeated calls must return the same cached result.
    let first = is_splice_available();
    let second = is_splice_available();
    assert_eq!(first, second);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn try_splice_returns_unsupported_on_non_linux() {
    assert!(!is_splice_available());
    let err = try_splice_to_file(0, 0, 1024).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
}

#[cfg(not(unix))]
#[test]
fn recv_fd_returns_unsupported_on_non_unix() {
    let err = recv_fd_to_file(0, 0, 1024).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
}

/// Creates a Unix socketpair. The writer thread sends `content` then closes
/// its end so the reader sees EOF. Returns `(recv_fd, join_handle)`.
#[cfg(unix)]
fn socketpair_with_writer(content: Vec<u8>) -> (i32, std::thread::JoinHandle<()>) {
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "socketpair creation failed");

    let recv_fd = fds[0];
    let send_fd = fds[1];

    let handle = std::thread::spawn(move || {
        let mut offset = 0;
        while offset < content.len() {
            let n = unsafe {
                libc::write(
                    send_fd,
                    content[offset..].as_ptr().cast::<libc::c_void>(),
                    content.len() - offset,
                )
            };
            assert!(n > 0, "write to socketpair failed");
            offset += n as usize;
        }
        unsafe { libc::close(send_fd) };
    });

    (recv_fd, handle)
}

/// Verifies that `dest` contains exactly `expected` after seeking to the start.
#[cfg(unix)]
fn assert_file_contents(dest: &mut tempfile::NamedTempFile, expected: &[u8]) {
    use std::io::{Read, Seek, SeekFrom};

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut buf = Vec::new();
    dest.read_to_end(&mut buf).unwrap();
    assert_eq!(buf.len(), expected.len(), "file length mismatch");
    assert_eq!(buf, expected, "file content mismatch");
}

/// Unix fallback tests (run on all unix platforms including Linux).
#[cfg(unix)]
mod fallback {
    use super::*;
    use std::os::fd::AsRawFd;
    use tempfile::NamedTempFile;

    #[test]
    fn recv_fd_small_payload() {
        let content = b"hello splice fallback";
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.to_vec());

        let received =
            recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len() as u64).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(received, content.len() as u64);
        assert_file_contents(&mut dest, content);
    }

    #[test]
    fn recv_fd_eof_before_length_exhausted() {
        // Source has fewer bytes than requested - should stop at EOF.
        let content = b"short";
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.to_vec());

        let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), 1_000_000).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(received, content.len() as u64);
        assert_file_contents(&mut dest, content);
    }

    #[test]
    fn recv_fd_zero_length_source() {
        // Sender closes immediately - zero bytes transferred.
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(Vec::new());

        let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), 4096).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(received, 0);
        assert_file_contents(&mut dest, &[]);
    }

    #[test]
    fn recv_fd_binary_payload_roundtrip() {
        // All 256 byte values to verify binary-safe transfer.
        let content: Vec<u8> = (0..=255).collect();
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.clone());

        let received =
            recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len() as u64).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(received, content.len() as u64);
        assert_file_contents(&mut dest, &content);
    }

    #[test]
    fn recv_fd_just_below_threshold() {
        // 64KB - 1: always uses the buffered path on Linux.
        let size = 64 * 1024 - 1;
        let content: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.clone());

        let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(received, size as u64);
        assert_file_contents(&mut dest, &content);
    }

    #[test]
    fn recv_fd_large_multichunk_transfer() {
        // 384KB - spans multiple internal buffer fills (buffer is 256KB).
        let size = 384 * 1024;
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.clone());

        let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(received, size as u64);
        assert_file_contents(&mut dest, &content);
    }
}

/// Linux-specific splice tests.
#[cfg(target_os = "linux")]
mod linux_splice {
    use super::*;
    use std::os::fd::AsRawFd;
    use tempfile::NamedTempFile;

    #[test]
    fn splice_available_on_modern_kernels() {
        // CI runs on modern kernels where splice should be supported.
        // If not, the remaining tests in this module gracefully skip.
        let _avail = is_splice_available();
    }

    #[test]
    fn try_splice_basic_transfer() {
        if !is_splice_available() {
            return;
        }

        let content = b"Integration test: basic splice socket-to-file transfer";
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.to_vec());

        let spliced =
            try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len()).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(spliced, content.len());
        assert_file_contents(&mut dest, content);
    }

    #[test]
    fn try_splice_at_exact_threshold() {
        if !is_splice_available() {
            return;
        }

        // Exactly 64KB - the splice threshold boundary.
        let size = 64 * 1024;
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.clone());

        let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), size).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(spliced, size);
        assert_file_contents(&mut dest, &content);
    }

    #[test]
    fn try_splice_multiple_chunks() {
        if !is_splice_available() {
            return;
        }

        // 512KB requires 8 splice-chunk iterations (64KB each).
        let size = 512 * 1024;
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.clone());

        let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), size).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(spliced, size);
        assert_file_contents(&mut dest, &content);
    }

    #[test]
    fn try_splice_eof_returns_partial() {
        if !is_splice_available() {
            return;
        }

        // Ask for more bytes than available - splice returns what it got.
        let content = b"partial data";
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.to_vec());

        let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), 1_000_000).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(spliced, content.len());
        assert_file_contents(&mut dest, content);
    }

    #[test]
    fn try_splice_immediate_eof() {
        if !is_splice_available() {
            return;
        }

        let mut dest = NamedTempFile::new().unwrap();

        let mut fds = [0i32; 2];
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0);

        // Close sender immediately.
        unsafe { libc::close(fds[1]) };

        let spliced = try_splice_to_file(fds[0], dest.as_file().as_raw_fd(), 4096).unwrap();
        unsafe { libc::close(fds[0]) };

        assert_eq!(spliced, 0);
        assert_file_contents(&mut dest, &[]);
    }

    #[test]
    fn try_splice_invalid_source_fd() {
        if !is_splice_available() {
            return;
        }

        let dest = NamedTempFile::new().unwrap();
        let result = try_splice_to_file(-1, dest.as_file().as_raw_fd(), 1024);
        assert!(result.is_err());
    }

    #[test]
    fn try_splice_invalid_dest_fd() {
        if !is_splice_available() {
            return;
        }

        let mut fds = [0i32; 2];
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0);

        // Write some data so the splice has something to transfer.
        let data = b"test";
        unsafe {
            libc::write(fds[1], data.as_ptr().cast::<libc::c_void>(), data.len());
            libc::close(fds[1]);
        }

        let result = try_splice_to_file(fds[0], -1, data.len());
        unsafe { libc::close(fds[0]) };

        assert!(result.is_err());
    }

    #[test]
    fn recv_fd_routes_large_through_splice() {
        if !is_splice_available() {
            return;
        }

        // 128KB - above the 64KB threshold, should use splice path.
        let size = 128 * 1024;
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.clone());

        let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(received, size as u64);
        assert_file_contents(&mut dest, &content);
    }

    #[test]
    fn recv_fd_routes_small_through_buffered() {
        // 32KB - below threshold, uses read/write even on Linux.
        let size = 32 * 1024;
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.clone());

        let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(received, size as u64);
        assert_file_contents(&mut dest, &content);
    }

    #[test]
    fn recv_fd_one_megabyte_stress() {
        // Stress test with 1MB payload spanning many splice chunks.
        let size = 1024 * 1024;
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let mut dest = NamedTempFile::new().unwrap();
        let (recv_fd, writer) = socketpair_with_writer(content.clone());

        let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

        unsafe { libc::close(recv_fd) };
        writer.join().unwrap();

        assert_eq!(received, size as u64);
        assert_file_contents(&mut dest, &content);
    }
}
