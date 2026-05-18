//! Low-level `splice(2)` / `vmsplice(2)` syscall wrappers and transfer helpers.

#[cfg(target_os = "linux")]
use super::probe::is_splice_available;
#[cfg(target_os = "linux")]
use super::{DEFAULT_PIPE_CAPACITY, SPLICE_CHUNK_SIZE, SPLICE_THRESHOLD, SplicePipe};

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

    let pipe = SplicePipe::with_capacity(DEFAULT_PIPE_CAPACITY)?;
    splice_fd_to_file_via_pipe(&pipe, socket_fd, file_fd, len)
}

/// Drives the two-phase splice loop: source fd -> pipe -> dest fd.
///
/// Handles EINTR by retrying the interrupted syscall. Handles short splices
/// by looping until the requested bytes are drained from the pipe.
///
/// This is the shared core used by both [`try_splice_to_file`] and
/// [`SplicePipe::splice_to_file`].
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub(super) fn splice_fd_to_file_via_pipe(
    pipe: &SplicePipe,
    source_fd: std::os::fd::RawFd,
    dest_fd: std::os::fd::RawFd,
    len: usize,
) -> std::io::Result<usize> {
    use std::io;

    let flags = libc::SPLICE_F_MOVE | libc::SPLICE_F_MORE;
    let mut total: usize = 0;
    let mut remaining = len;

    while remaining > 0 {
        let chunk = remaining.min(SPLICE_CHUNK_SIZE);

        // Phase 1: splice from source into the pipe write end.
        // SAFETY: source_fd is assumed valid by the caller (documented precondition).
        // pipe.write_fd is valid because SplicePipe owns it and has not been dropped.
        // Null offset pointers use current file position.
        let spliced_in = loop {
            // SAFETY: all fds passed are valid for the duration of the call; the
            // iovec/buffer references live across the syscall.
            let result = unsafe {
                libc::splice(
                    source_fd,
                    std::ptr::null_mut(),
                    pipe.write_fd(),
                    std::ptr::null_mut(),
                    chunk,
                    flags,
                )
            };
            if result < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue; // Retry on EINTR
                }
                if total == 0 {
                    return Err(err);
                }
                // Partial transfer - return what we have
                return Ok(total);
            }
            break result;
        };

        if spliced_in == 0 {
            // EOF on source
            break;
        }

        let bytes_in_pipe = spliced_in as usize;

        // Phase 2: splice from the pipe read end into the file.
        // Must drain exactly bytes_in_pipe to avoid pipe deadlock.
        drain_pipe_to_fd(pipe, dest_fd, bytes_in_pipe)?;

        total += bytes_in_pipe;
        remaining -= bytes_in_pipe;
    }

    Ok(total)
}

/// Drains `len` bytes from the pipe read end into `dest_fd` via `splice(2)`.
///
/// Handles EINTR by retrying. Returns an error if the pipe produces zero
/// bytes (unexpected EOF on the pipe).
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub(super) fn drain_pipe_to_fd(
    pipe: &SplicePipe,
    dest_fd: std::os::fd::RawFd,
    len: usize,
) -> std::io::Result<()> {
    use std::io;

    let flags = libc::SPLICE_F_MOVE | libc::SPLICE_F_MORE;
    let mut pipe_remaining = len;

    while pipe_remaining > 0 {
        // SAFETY: pipe.read_fd is valid (owned by SplicePipe). dest_fd is assumed
        // valid by the caller. Null offset pointers use current file position.
        let spliced_out = unsafe {
            libc::splice(
                pipe.read_fd(),
                std::ptr::null_mut(),
                dest_fd,
                std::ptr::null_mut(),
                pipe_remaining,
                flags,
            )
        };

        if spliced_out < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue; // Retry on EINTR
            }
            return Err(err);
        }
        if spliced_out == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "splice to file returned 0 bytes",
            ));
        }

        pipe_remaining -= spliced_out as usize;
    }

    Ok(())
}

/// Transfers a userspace buffer to a file using `vmsplice(2)` + `splice(2)`.
///
/// The transfer path is: `buffer -> vmsplice -> pipe -> splice -> file_fd`,
/// avoiding a userspace-to-kernel copy for the buffer contents. The kernel
/// references the buffer pages directly via `vmsplice(2)` and then moves
/// them to the destination file via `splice(2)`.
///
/// # Arguments
///
/// * `buf` - Source buffer to transfer. Must remain valid and unmodified until
///   this function returns (the kernel references the pages in-flight).
/// * `file_fd` - Destination file descriptor (must be a regular file or pipe)
///
/// # Returns
///
/// The number of bytes actually transferred.
///
/// # Errors
///
/// Returns an error if:
/// - `splice`/`vmsplice` is not supported on this platform (`ErrorKind::Unsupported`)
/// - Pipe creation fails
/// - An I/O error occurs during transfer
///
/// # Platform Support
///
/// - **Linux 2.6.17+**: Full implementation using `vmsplice(2)` + `splice(2)`
/// - **Other platforms**: Always returns `Unsupported` error
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn try_vmsplice_to_file(buf: &[u8], file_fd: std::os::fd::RawFd) -> std::io::Result<usize> {
    use std::io;

    if buf.is_empty() {
        return Ok(0);
    }

    if !is_splice_available() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "splice/vmsplice not available on this kernel",
        ));
    }

    let pipe = SplicePipe::with_capacity(DEFAULT_PIPE_CAPACITY)?;
    let mut total: usize = 0;
    let mut remaining = buf.len();

    while remaining > 0 {
        let offset = total;
        let chunk = remaining.min(SPLICE_CHUNK_SIZE);

        // Phase 1: vmsplice the buffer slice into the pipe write end.
        // SAFETY: The iovec points to buf[offset..offset+chunk], which is valid
        // for the duration of this call. pipe.write_fd is valid (owned by SplicePipe).
        // SPLICE_F_MORE hints that more data follows within this transfer.
        let iov = libc::iovec {
            iov_base: buf[offset..].as_ptr() as *mut libc::c_void,
            iov_len: chunk,
        };

        let vspliced = loop {
            let result = unsafe { libc::vmsplice(pipe.write_fd(), &iov, 1, libc::SPLICE_F_MORE) };
            if result < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue; // Retry on EINTR
                }
                return Err(err);
            }
            break result as usize;
        };

        if vspliced == 0 {
            break;
        }

        // Phase 2: splice from the pipe read end into the file.
        // Must drain exactly vspliced bytes - the kernel holds a reference to
        // the buffer pages until the pipe consumer reads them.
        drain_pipe_to_fd(&pipe, file_fd, vspliced)?;

        total += vspliced;
        remaining -= vspliced;
    }

    Ok(total)
}

/// Stub for non-Linux platforms - always returns `Unsupported`.
#[cfg(not(target_os = "linux"))]
pub fn try_vmsplice_to_file(_buf: &[u8], _file_fd: i32) -> std::io::Result<usize> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "vmsplice is only available on Linux",
    ))
}

/// Stub for non-Linux platforms - always returns `Unsupported`.
#[cfg(not(target_os = "linux"))]
pub fn try_splice_to_file(_socket_fd: i32, _file_fd: i32, _len: usize) -> std::io::Result<usize> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "splice is only available on Linux",
    ))
}

/// Receives data from a raw file descriptor to a file, using `splice(2)` when available.
///
/// This is the receive-direction counterpart to [`crate::sendfile::send_file_to_fd`].
/// It selects the best transfer mechanism based on the platform and transfer size:
///
/// - **Linux, length >= 64KB**: Tries `splice(2)` for zero-copy transfer.
///   On failure (EINVAL, unsupported filesystem, etc.), falls back to `read`/`write`.
/// - **Linux, length < 64KB**: Uses buffered `read`/`write` directly.
/// - **Other unix platforms**: Uses buffered `read`/`write` via `libc`.
/// - **Non-unix platforms**: Uses `std::io::copy` fallback.
///
/// # Arguments
///
/// * `source_fd` - Source file descriptor (typically a socket or pipe)
/// * `dest_fd` - Destination file descriptor (typically a regular file)
/// * `length` - Number of bytes to transfer
///
/// # Returns
///
/// The number of bytes actually transferred. May be less than `length` if the
/// source reaches EOF.
///
/// # Errors
///
/// Returns an error if both the optimized path and the fallback fail.
///
/// # Example
///
/// ```no_run
/// # #[cfg(unix)]
/// # {
/// use std::fs::File;
/// use std::net::TcpStream;
/// use std::os::fd::AsRawFd;
/// use fast_io::splice::recv_fd_to_file;
///
/// let socket = TcpStream::connect("127.0.0.1:8080").unwrap();
/// let file = File::create("output.bin").unwrap();
/// let received = recv_fd_to_file(socket.as_raw_fd(), file.as_raw_fd(), 1024 * 1024).unwrap();
/// println!("Received {} bytes", received);
/// # }
/// ```
#[cfg(target_os = "linux")]
pub fn recv_fd_to_file(
    source_fd: std::os::fd::RawFd,
    dest_fd: std::os::fd::RawFd,
    length: u64,
) -> std::io::Result<u64> {
    if length >= SPLICE_THRESHOLD {
        if let Ok(n) = try_splice_to_file(source_fd, dest_fd, length as usize) {
            return Ok(n as u64);
        }
        // Fall through to read/write fallback
    }
    copy_fd_to_fd(source_fd, dest_fd, length)
}

/// Non-Linux unix stub - uses buffered `read`/`write` via `libc`.
#[cfg(all(unix, not(target_os = "linux")))]
pub fn recv_fd_to_file(
    source_fd: std::os::fd::RawFd,
    dest_fd: std::os::fd::RawFd,
    length: u64,
) -> std::io::Result<u64> {
    copy_fd_to_fd(source_fd, dest_fd, length)
}

/// Non-unix stub - always returns `Unsupported`.
#[cfg(not(unix))]
pub fn recv_fd_to_file(_source_fd: i32, _dest_fd: i32, _length: u64) -> std::io::Result<u64> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "recv_fd_to_file requires unix file descriptors",
    ))
}

/// Buffered `read`/`write` fallback for fd-to-fd transfer.
///
/// Reads from `source_fd` and writes to `dest_fd` using a 256KB userspace buffer,
/// handling partial reads and writes. Used when `splice(2)` is unavailable or fails.
///
/// # Arguments
///
/// * `source_fd` - Source file descriptor to read from
/// * `dest_fd` - Destination file descriptor to write to
/// * `length` - Maximum number of bytes to transfer
///
/// # Returns
///
/// The number of bytes actually transferred. Returns early on EOF.
#[cfg(unix)]
#[allow(unsafe_code)]
pub(super) fn copy_fd_to_fd(
    source_fd: std::os::fd::RawFd,
    dest_fd: std::os::fd::RawFd,
    length: u64,
) -> std::io::Result<u64> {
    use std::io;

    const BUF_SIZE: usize = 256 * 1024;
    let mut buf = vec![0u8; BUF_SIZE];
    let mut total: u64 = 0;
    let mut remaining = length;

    while remaining > 0 {
        let to_read = (remaining as usize).min(BUF_SIZE);

        // SAFETY: buf[..to_read] is a valid, aligned, mutable byte slice.
        // source_fd is assumed valid by the caller (documented precondition).
        let n = unsafe {
            libc::read(
                source_fd,
                buf[..to_read].as_mut_ptr().cast::<libc::c_void>(),
                to_read,
            )
        };

        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            break; // EOF
        }

        let bytes_read = n as usize;
        let mut written = 0;
        while written < bytes_read {
            // SAFETY: buf[written..bytes_read] is a valid byte slice.
            // dest_fd is assumed valid by the caller.
            let w = unsafe {
                libc::write(
                    dest_fd,
                    buf[written..bytes_read].as_ptr().cast::<libc::c_void>(),
                    bytes_read - written,
                )
            };
            if w < 0 {
                return Err(io::Error::last_os_error());
            }
            written += w as usize;
        }

        total += bytes_read as u64;
        remaining -= bytes_read as u64;
    }

    Ok(total)
}
