//! Non-Linux platform tests for the splice module.

use super::super::*;

#[test]
fn test_splice_unavailable_on_non_linux() {
    assert!(!is_splice_available());

    let result = try_splice_to_file(0, 0, 1024);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
}

#[test]
fn test_vmsplice_unavailable_on_non_linux() {
    let data = b"test data";
    let result = try_vmsplice_to_file(data, 0);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
}

#[test]
fn test_splice_pipe_unavailable_on_non_linux() {
    assert!(SplicePipe::new().is_err());
    assert!(SplicePipe::with_capacity(1024).is_err());
}

#[cfg(not(unix))]
#[test]
fn test_recv_fd_to_file_unsupported_on_non_unix() {
    let result = recv_fd_to_file(0, 0, 1024);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
}

#[cfg(all(unix, not(target_os = "linux")))]
mod unix_fallback_tests {
    use super::super::super::syscalls::copy_fd_to_fd;
    use super::super::super::*;
    use std::io::{Read, Seek, SeekFrom};
    use tempfile::NamedTempFile;

    #[test]
    fn test_recv_fd_to_file_uses_fallback() {
        // On non-Linux unix, recv_fd_to_file uses the read/write fallback.
        let content = b"Testing recv_fd_to_file fallback on non-Linux unix";
        let mut dest = NamedTempFile::new().unwrap();

        let mut socket_fds = [0i32; 2];
        // SAFETY: `socket_fds`/`fds` provides the two-int output slot the
        // `socketpair(2)` syscall fills on success.
        let result = unsafe {
            libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
        };
        assert_eq!(result, 0);

        let recv_fd = socket_fds[0];
        let send_fd = socket_fds[1];

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
        // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
        // is closed exactly once here; no further use occurs after this call.
        unsafe { libc::close(send_fd) };

        use std::os::fd::AsRawFd;
        let received =
            recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len() as u64).unwrap();

        // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
        // is closed exactly once here; no further use occurs after this call.
        unsafe { libc::close(recv_fd) };

        assert_eq!(received, content.len() as u64);

        dest.seek(SeekFrom::Start(0)).unwrap();
        let mut file_content = Vec::new();
        dest.read_to_end(&mut file_content).unwrap();
        assert_eq!(file_content, content);
    }

    #[test]
    fn test_copy_fd_to_fd_on_non_linux() {
        // Direct test of the fallback path on macOS/BSD.
        let content = b"Fallback path direct test on non-Linux unix";
        let mut dest = NamedTempFile::new().unwrap();

        let mut socket_fds = [0i32; 2];
        // SAFETY: `socket_fds`/`fds` provides the two-int output slot the
        // `socketpair(2)` syscall fills on success.
        let result = unsafe {
            libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
        };
        assert_eq!(result, 0);

        let recv_fd = socket_fds[0];
        let send_fd = socket_fds[1];

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
        // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
        // is closed exactly once here; no further use occurs after this call.
        unsafe { libc::close(send_fd) };

        use std::os::fd::AsRawFd;
        let received =
            copy_fd_to_fd(recv_fd, dest.as_file().as_raw_fd(), content.len() as u64).unwrap();

        // SAFETY: the fd was opened by `socketpair`/`pipe` earlier in the test and
        // is closed exactly once here; no further use occurs after this call.
        unsafe { libc::close(recv_fd) };

        assert_eq!(received, content.len() as u64);

        dest.seek(SeekFrom::Start(0)).unwrap();
        let mut file_content = Vec::new();
        dest.read_to_end(&mut file_content).unwrap();
        assert_eq!(file_content, content);
    }
}
