//! Zero-copy socket-to-disk transfer using `splice`/`vmsplice` syscalls.
//!
//! This module provides high-performance network-to-file transfer using Linux's
//! `splice` syscall when available, with automatic fallback to standard read/write
//! on other platforms or when the syscall fails.
//!
//! # How it works
//!
//! `splice(2)` moves data between a file descriptor and a pipe without copying
//! through userspace. For socket-to-file transfers, the data path is:
//!
//! ```text
//! socket_fd -> pipe (kernel buffer) -> file_fd
//! ```
//!
//! This requires two `splice` calls per chunk but avoids any userspace buffer
//! copies, keeping the data entirely in kernel pages.
//!
//! # Platform Support
//!
//! - **Linux 2.6.17+**: Uses `splice` for zero-copy socket-to-file transfer
//! - **Other platforms**: Returns `Unsupported` error, triggering caller fallback
//!
//! # Performance Characteristics
//!
//! - For transfers < 64KB: callers should use read/write directly (lower overhead)
//! - For transfers >= 64KB: `splice` avoids userspace copies entirely
//! - Pipe buffer size defaults to 64KB on most Linux kernels (tunable via
//!   `/proc/sys/fs/pipe-max-size`)
//! - Uses `SPLICE_F_MOVE | SPLICE_F_MORE` flags for optimal page migration
//!
//! # Example
//!
//! ```no_run
//! # #[cfg(target_os = "linux")]
//! # {
//! use std::fs::File;
//! use std::net::TcpStream;
//! use std::os::fd::AsRawFd;
//! use fast_io::splice::try_splice_to_file;
//!
//! let socket = TcpStream::connect("127.0.0.1:8080").unwrap();
//! let file = File::create("output.bin").unwrap();
//! match try_splice_to_file(socket.as_raw_fd(), file.as_raw_fd(), 1024 * 1024) {
//!     Ok(n) => println!("Spliced {} bytes", n),
//!     Err(_) => println!("Splice unavailable, using fallback"),
//! }
//! # }
//! ```

#[cfg(target_os = "linux")]
use std::sync::OnceLock;

/// Whether `splice` is supported on this kernel. Cached after first probe.
#[cfg(target_os = "linux")]
static SPLICE_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Maximum bytes per splice call. Matches the default pipe buffer capacity
/// on most Linux kernels (16 pages * 4KB = 64KB). Using a larger value is
/// fine - the kernel will transfer up to the pipe capacity per call.
#[cfg(target_os = "linux")]
const SPLICE_CHUNK_SIZE: usize = 64 * 1024;

/// Returns whether `splice(2)` is available on the current system.
///
/// The result is probed once and cached for the lifetime of the process.
/// On non-Linux platforms, always returns `false`.
pub fn is_splice_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        *SPLICE_SUPPORTED.get_or_init(probe_splice_support)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Probes splice support by creating a pipe pair and attempting a zero-length splice.
///
/// This detects kernels or seccomp profiles that block `splice(2)`.
#[cfg(target_os = "linux")]
fn probe_splice_support() -> bool {
    let pipe = match SplicePipe::new() {
        Ok(p) => p,
        Err(_) => return false,
    };

    // Attempt a zero-length splice from the read end to the write end.
    // We use the pipe's own read end as the "input fd" - this is not useful
    // for real I/O but tests that the syscall is not blocked by seccomp.
    // SAFETY: pipe fds are valid and open. Zero-length splice is a no-op.
    let result = unsafe {
        libc::splice(
            pipe.read_fd,
            std::ptr::null_mut(),
            pipe.write_fd,
            std::ptr::null_mut(),
            0,
            0,
        )
    };

    // Result of 0 means the syscall is available (zero bytes transferred).
    // ENOSYS means the syscall does not exist.
    if result < 0 {
        let err = std::io::Error::last_os_error();
        // EAGAIN is acceptable for non-blocking fds with no data - the syscall exists
        err.raw_os_error() == Some(libc::EAGAIN)
    } else {
        true
    }
}

/// Transfers data from a socket to a file using `splice(2)` with a pipe intermediary.
///
/// The transfer path is: `socket_fd -> pipe -> file_fd`, keeping data in kernel
/// pages without copying through userspace.
///
/// # Arguments
///
/// * `socket_fd` - Source file descriptor (must be a socket or pipe)
/// * `file_fd` - Destination file descriptor (must support `splice` as output,
///   i.e., a regular file or pipe)
/// * `len` - Number of bytes to transfer
///
/// # Returns
///
/// The number of bytes actually transferred. May be less than `len` if the
/// source reaches EOF or the socket is closed.
///
/// # Errors
///
/// Returns an error if:
/// - `splice` is not supported on this platform (`ErrorKind::Unsupported`)
/// - `splice` is blocked by seccomp or returns `ENOSYS`/`EINVAL`
/// - Pipe creation fails
/// - An I/O error occurs during transfer
///
/// Callers should fall back to standard read/write on any error.
///
/// # Platform Support
///
/// - **Linux**: Full implementation using `splice(2)` with pipe intermediary
/// - **Other platforms**: Always returns `Unsupported` error
#[cfg(target_os = "linux")]
pub fn try_splice_to_file(
    socket_fd: std::os::fd::RawFd,
    file_fd: std::os::fd::RawFd,
    len: usize,
) -> std::io::Result<usize> {
    use std::io;

    if !is_splice_available() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "splice not available on this kernel",
        ));
    }

    let pipe = SplicePipe::new()?;
    let flags = libc::SPLICE_F_MOVE | libc::SPLICE_F_MORE;
    let mut total: usize = 0;
    let mut remaining = len;

    while remaining > 0 {
        let chunk = remaining.min(SPLICE_CHUNK_SIZE);

        // Phase 1: splice from socket into the pipe write end.
        // SAFETY: socket_fd is assumed valid by the caller (documented precondition).
        // pipe.write_fd is valid because SplicePipe owns it and has not been dropped.
        // Null offset pointers use current file position.
        let spliced_in = unsafe {
            libc::splice(
                socket_fd,
                std::ptr::null_mut(),
                pipe.write_fd,
                std::ptr::null_mut(),
                chunk,
                flags,
            )
        };

        if spliced_in < 0 {
            let err = io::Error::last_os_error();
            if total == 0 {
                return Err(err);
            }
            // Partial transfer - return what we have
            return Ok(total);
        }
        if spliced_in == 0 {
            // EOF on socket
            break;
        }

        let bytes_in_pipe = spliced_in as usize;

        // Phase 2: splice from the pipe read end into the file.
        // Must drain exactly bytes_in_pipe to avoid pipe deadlock.
        let mut pipe_remaining = bytes_in_pipe;
        while pipe_remaining > 0 {
            // SAFETY: pipe.read_fd is valid (owned by SplicePipe). file_fd is assumed
            // valid by the caller. Null offset pointers use current file position.
            let spliced_out = unsafe {
                libc::splice(
                    pipe.read_fd,
                    std::ptr::null_mut(),
                    file_fd,
                    std::ptr::null_mut(),
                    pipe_remaining,
                    flags,
                )
            };

            if spliced_out < 0 {
                return Err(io::Error::last_os_error());
            }
            if spliced_out == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "splice to file returned 0 bytes",
                ));
            }

            pipe_remaining -= spliced_out as usize;
        }

        total += bytes_in_pipe;
        remaining -= bytes_in_pipe;
    }

    Ok(total)
}

/// Stub for non-Linux platforms - always returns `Unsupported`.
#[cfg(not(target_os = "linux"))]
pub fn try_splice_to_file(_socket_fd: i32, _file_fd: i32, _len: usize) -> std::io::Result<usize> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "splice is only available on Linux",
    ))
}

/// RAII wrapper around a pipe pair created for splice intermediary use.
///
/// The pipe is created with `O_CLOEXEC` to prevent fd leaks across `exec`.
/// Both ends are closed on drop.
#[cfg(target_os = "linux")]
struct SplicePipe {
    /// Read end of the pipe (data flows out to the destination file).
    read_fd: i32,
    /// Write end of the pipe (data flows in from the source socket).
    write_fd: i32,
}

#[cfg(target_os = "linux")]
impl SplicePipe {
    /// Creates a new pipe pair with `O_CLOEXEC` set.
    ///
    /// # Errors
    ///
    /// Returns an error if `pipe2(2)` fails (e.g., fd limit reached).
    fn new() -> std::io::Result<Self> {
        let mut fds = [0i32; 2];
        // SAFETY: fds is a valid [i32; 2] array. pipe2 writes two valid file
        // descriptors on success. O_CLOEXEC prevents leaking fds across exec.
        let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            read_fd: fds[0],
            write_fd: fds[1],
        })
    }
}

#[cfg(target_os = "linux")]
impl Drop for SplicePipe {
    fn drop(&mut self) {
        // SAFETY: fds were created by pipe2 and are still valid (not closed elsewhere).
        // close() on an already-closed fd is harmless in practice but we ensure
        // single-ownership by making SplicePipe the sole owner.
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_splice_available_returns_bool() {
        // On any platform, this should return a boolean without panicking.
        let _available = is_splice_available();
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_splice_unavailable_on_non_linux() {
        assert!(!is_splice_available());

        let result = try_splice_to_file(0, 0, 1024);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }

    #[cfg(target_os = "linux")]
    mod linux_tests {
        use super::*;
        use std::io::{Read, Seek, SeekFrom, Write};
        use tempfile::NamedTempFile;

        #[test]
        fn test_splice_pipe_creation() {
            let pipe = SplicePipe::new();
            assert!(pipe.is_ok(), "pipe2 should succeed");
            let pipe = pipe.unwrap();
            assert!(pipe.read_fd >= 0);
            assert!(pipe.write_fd >= 0);
            assert_ne!(pipe.read_fd, pipe.write_fd);
            // Drop closes both fds
        }

        #[test]
        fn test_splice_pipe_multiple_creates() {
            // Verify we can create and drop multiple pipes without fd leaks.
            for _ in 0..100 {
                let pipe = SplicePipe::new().unwrap();
                assert!(pipe.read_fd >= 0);
                assert!(pipe.write_fd >= 0);
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
            let result = unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
            };
            assert_eq!(result, 0, "Failed to create socketpair");

            let recv_fd = socket_fds[0]; // splice reads from this end
            let send_fd = socket_fds[1]; // we write test data to this end

            // Write test data into the send end.
            let written = unsafe {
                libc::write(
                    send_fd,
                    content.as_ptr().cast::<libc::c_void>(),
                    content.len(),
                )
            };
            assert_eq!(written, content.len() as isize);

            // Close send end so splice sees EOF after the data.
            unsafe { libc::close(send_fd) };

            // Splice from recv_fd into the file.
            use std::os::fd::AsRawFd;
            let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len());

            unsafe { libc::close(recv_fd) };

            let spliced = spliced.unwrap();
            assert_eq!(spliced, content.len());

            // Verify file contents.
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
            let result = unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
            };
            assert_eq!(result, 0, "Failed to create socketpair");

            let recv_fd = socket_fds[0];
            let send_fd = socket_fds[1];

            // Spawn writer thread to avoid socket buffer deadlock on large transfers.
            let content_clone = content.clone();
            let writer_thread = std::thread::spawn(move || {
                let mut offset = 0;
                while offset < content_clone.len() {
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
                unsafe { libc::close(send_fd) };
            });

            use std::os::fd::AsRawFd;
            let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), size).unwrap();

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
            let result = unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
            };
            assert_eq!(result, 0);

            let recv_fd = socket_fds[0];
            let send_fd = socket_fds[1];

            // Close send end immediately - splice should return 0 (EOF).
            unsafe { libc::close(send_fd) };

            use std::os::fd::AsRawFd;
            let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), 1024).unwrap();

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
            let size = SPLICE_CHUNK_SIZE;
            let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let mut dest = NamedTempFile::new().unwrap();

            let mut socket_fds = [0i32; 2];
            let result = unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
            };
            assert_eq!(result, 0);

            let recv_fd = socket_fds[0];
            let send_fd = socket_fds[1];

            let content_clone = content.clone();
            let writer_thread = std::thread::spawn(move || {
                let mut offset = 0;
                while offset < content_clone.len() {
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
                unsafe { libc::close(send_fd) };
            });

            use std::os::fd::AsRawFd;
            let spliced = try_splice_to_file(recv_fd, dest.as_file().as_raw_fd(), size).unwrap();

            unsafe { libc::close(recv_fd) };
            writer_thread.join().expect("writer thread should succeed");

            assert_eq!(spliced, size);

            dest.seek(SeekFrom::Start(0)).unwrap();
            let mut file_content = Vec::new();
            dest.read_to_end(&mut file_content).unwrap();
            assert_eq!(file_content, content);
        }
    }
}
