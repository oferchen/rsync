//! Tests for `recv_fd_to_file` and the `copy_fd_to_fd` fallback on Linux.

use super::super::super::syscalls::copy_fd_to_fd;
use super::super::super::*;
use std::io::{Read, Seek, SeekFrom};
use tempfile::NamedTempFile;

#[test]
fn test_recv_fd_to_file_small_transfer() {
    // Below SPLICE_THRESHOLD - should use read/write fallback directly.
    let content = b"Small transfer below splice threshold";
    let mut dest = NamedTempFile::new().unwrap();

    let (recv_fd, writer) = super::socketpair_with_writer(content.to_vec());

    use std::os::fd::AsRawFd;
    let received =
        recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len() as u64).unwrap();

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };
    writer.join().expect("writer thread should succeed");

    assert_eq!(received, content.len() as u64);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}

#[test]
fn test_recv_fd_to_file_large_transfer() {
    // Above SPLICE_THRESHOLD - should attempt splice, fall back if unavailable.
    let size: usize = 256 * 1024;
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let mut dest = NamedTempFile::new().unwrap();

    let (recv_fd, writer) = super::socketpair_with_writer(content.clone());

    use std::os::fd::AsRawFd;
    let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };
    writer.join().expect("writer thread should succeed");

    assert_eq!(received, size as u64);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}

#[test]
fn test_recv_fd_to_file_empty() {
    // Zero-length transfer should succeed immediately.
    let mut dest = NamedTempFile::new().unwrap();

    let mut socket_fds = [0i32; 2];
    // SAFETY: `socket_fds`/`fds` provides the two-int output slot the
    // `socketpair(2)` syscall fills on success.
    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr()) };
    assert_eq!(result, 0);

    let recv_fd = socket_fds[0];
    let send_fd = socket_fds[1];
    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(send_fd) };

    use std::os::fd::AsRawFd;
    let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), 1024).unwrap();

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };

    assert_eq!(received, 0);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert!(file_content.is_empty());
}

#[test]
fn test_recv_fd_to_file_exact_threshold() {
    // Exactly at SPLICE_THRESHOLD boundary - should attempt splice path.
    let size = super::super::super::SPLICE_THRESHOLD as usize;
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let mut dest = NamedTempFile::new().unwrap();

    let (recv_fd, writer) = super::socketpair_with_writer(content.clone());

    use std::os::fd::AsRawFd;
    let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };
    writer.join().expect("writer thread should succeed");

    assert_eq!(received, size as u64);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}

#[test]
fn test_copy_fd_to_fd_fallback() {
    let content = b"Testing copy_fd_to_fd fallback path directly";
    let mut dest = NamedTempFile::new().unwrap();

    let (recv_fd, writer) = super::socketpair_with_writer(content.to_vec());

    use std::os::fd::AsRawFd;
    let received =
        copy_fd_to_fd(recv_fd, dest.as_file().as_raw_fd(), content.len() as u64).unwrap();

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };
    writer.join().expect("writer thread should succeed");

    assert_eq!(received, content.len() as u64);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}

#[test]
fn test_copy_fd_to_fd_large() {
    // Test fallback with data spanning multiple buffer fills (> 256KB).
    let size: usize = 512 * 1024;
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let mut dest = NamedTempFile::new().unwrap();

    let (recv_fd, writer) = super::socketpair_with_writer(content.clone());

    use std::os::fd::AsRawFd;
    let received = copy_fd_to_fd(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };
    writer.join().expect("writer thread should succeed");

    assert_eq!(received, size as u64);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}

#[test]
fn test_recv_fd_to_file_partial_read() {
    // Request more bytes than available - should stop at EOF.
    let content = b"Short content for EOF test";
    let mut dest = NamedTempFile::new().unwrap();

    let (recv_fd, writer) = super::socketpair_with_writer(content.to_vec());

    use std::os::fd::AsRawFd;
    let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), 100_000).unwrap();

    // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
    // is closed exactly once here; no further use occurs after this call.
    unsafe { libc::close(recv_fd) };
    writer.join().expect("writer thread should succeed");

    assert_eq!(received, content.len() as u64);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}
