//! Zero-copy socket-to-disk transfer using `splice`/`vmsplice` syscalls.
//!
//! This module provides high-performance network-to-file transfer using Linux's
//! `splice` and `vmsplice` syscalls when available, with automatic fallback to
//! standard read/write on other platforms or when the syscalls fail.
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
//! `vmsplice(2)` moves userspace memory pages into a pipe without copying.
//! Combined with `splice`, this enables zero-copy buffer-to-file transfer:
//!
//! ```text
//! userspace buffer -> vmsplice -> pipe (kernel buffer) -> splice -> file_fd
//! ```
//!
//! # API Layers
//!
//! - [`try_splice_to_file`] - Low-level: attempts `splice(2)` only, returns error
//!   on unsupported platforms or syscall failure. Callers must handle fallback.
//! - [`try_vmsplice_to_file`] - Low-level: moves a userspace buffer into a file
//!   via `vmsplice(2)` + `splice(2)`. Returns error on unsupported platforms.
//! - [`recv_fd_to_file`] - High-level: tries `splice(2)` for transfers >= 64KB,
//!   automatically falls back to buffered `read`/`write` on failure or for small
//!   transfers. Analogous to [`crate::sendfile::send_file_to_fd`] for the receive
//!   direction.
//! - [`SplicePipe`] - RAII pipe pair with configurable buffer size, usable as the
//!   intermediary for both `splice` and `vmsplice` operations.
//!
//! # Platform Support
//!
//! - **Linux 2.6.17+**: Uses `splice`/`vmsplice` for zero-copy transfer
//! - **Other platforms**: Falls back to buffered `read`/`write` (via `recv_fd_to_file`)
//!   or returns `Unsupported` error (via `try_splice_to_file`, `try_vmsplice_to_file`)
//!
//! # Performance Characteristics
//!
//! - For transfers < 64KB: `recv_fd_to_file` uses read/write directly (lower overhead)
//! - For transfers >= 64KB: `splice` avoids userspace copies entirely
//! - Pipe buffer size defaults to 1MB (configurable via [`SplicePipe::with_capacity`]),
//!   falling back gracefully if `fcntl(F_SETPIPE_SZ)` is denied
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
//! use fast_io::splice::recv_fd_to_file;
//!
//! let socket = TcpStream::connect("127.0.0.1:8080").unwrap();
//! let file = File::create("output.bin").unwrap();
//! // Automatically tries splice(2) for large transfers, falls back to read/write.
//! let received = recv_fd_to_file(socket.as_raw_fd(), file.as_raw_fd(), 1024 * 1024).unwrap();
//! println!("Received {} bytes", received);
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

/// Minimum transfer size to attempt splice. Below this threshold, the overhead
/// of creating a pipe pair and two splice syscalls per chunk exceeds the benefit
/// of avoiding a userspace copy. Matches the sendfile threshold.
#[cfg(target_os = "linux")]
const SPLICE_THRESHOLD: u64 = 64 * 1024;

/// Default pipe buffer size requested via `fcntl(F_SETPIPE_SZ)`.
///
/// 1MB provides a good balance between throughput (fewer splice loops per
/// large transfer) and memory usage. The kernel may grant less if the
/// process lacks `CAP_SYS_RESOURCE` or the requested size exceeds
/// `/proc/sys/fs/pipe-max-size`.
#[cfg(target_os = "linux")]
pub const DEFAULT_PIPE_CAPACITY: usize = 1024 * 1024;

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

/// Returns whether `splice(2)` is available and not disabled by `policy`.
///
/// Combines [`is_splice_available`] with the
/// [`ZeroCopyPolicy`](crate::ZeroCopyPolicy) gate. Returns `false` when
/// the policy is [`ZeroCopyPolicy::Disabled`](crate::ZeroCopyPolicy::Disabled)
/// regardless of kernel support, so callers can opt out of zero-copy
/// transfer paths via `--no-zero-copy`.
#[must_use]
pub fn is_splice_enabled(policy: crate::ZeroCopyPolicy) -> bool {
    !matches!(policy, crate::ZeroCopyPolicy::Disabled) && is_splice_available()
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
fn splice_fd_to_file_via_pipe(
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
            let result = unsafe {
                libc::splice(
                    source_fd,
                    std::ptr::null_mut(),
                    pipe.write_fd,
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
fn drain_pipe_to_fd(
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
                pipe.read_fd,
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
            let result = unsafe { libc::vmsplice(pipe.write_fd, &iov, 1, libc::SPLICE_F_MORE) };
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
fn copy_fd_to_fd(
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

/// RAII wrapper around a pipe pair created for splice/vmsplice intermediary use.
///
/// The pipe is created with `O_CLOEXEC` to prevent fd leaks across `exec`.
/// Both ends are closed on drop. The pipe buffer size can be enlarged via
/// [`SplicePipe::with_capacity`] to reduce the number of splice loop iterations
/// for large transfers.
///
/// # Usage
///
/// ```no_run
/// # #[cfg(target_os = "linux")]
/// # {
/// use fast_io::splice::SplicePipe;
///
/// let pipe = SplicePipe::with_capacity(1024 * 1024).unwrap();
/// println!("pipe capacity: {} bytes", pipe.capacity());
/// # }
/// ```
#[cfg(target_os = "linux")]
pub struct SplicePipe {
    /// Read end of the pipe (data flows out to the destination file).
    read_fd: i32,
    /// Write end of the pipe (data flows in from the source socket).
    write_fd: i32,
    /// Actual pipe buffer capacity after `fcntl(F_SETPIPE_SZ)`.
    capacity: usize,
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
impl SplicePipe {
    /// Creates a new pipe pair with `O_CLOEXEC` set and the default kernel
    /// pipe buffer size (typically 64KB on most Linux kernels).
    ///
    /// # Errors
    ///
    /// Returns an error if `pipe2(2)` fails (e.g., fd limit reached).
    pub fn new() -> std::io::Result<Self> {
        let mut fds = [0i32; 2];
        // SAFETY: fds is a valid [i32; 2] array. pipe2 writes two valid file
        // descriptors on success. O_CLOEXEC prevents leaking fds across exec.
        let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }

        // Query the actual pipe capacity the kernel assigned.
        // SAFETY: fds[1] is a valid pipe fd. F_GETPIPE_SZ returns the buffer
        // size in bytes as a positive integer.
        let capacity = unsafe { libc::fcntl(fds[1], libc::F_GETPIPE_SZ) };
        let capacity = if capacity > 0 {
            capacity as usize
        } else {
            // Fallback: assume the common default of 64KB.
            64 * 1024
        };

        Ok(Self {
            read_fd: fds[0],
            write_fd: fds[1],
            capacity,
        })
    }

    /// Creates a new pipe pair and attempts to enlarge the buffer to
    /// `requested_capacity` bytes via `fcntl(F_SETPIPE_SZ)`.
    ///
    /// The kernel may grant a smaller buffer if the process lacks
    /// `CAP_SYS_RESOURCE` or the requested size exceeds
    /// `/proc/sys/fs/pipe-max-size`. The actual capacity is always
    /// queryable via [`SplicePipe::capacity`].
    ///
    /// # Errors
    ///
    /// Returns an error if `pipe2(2)` fails. A failed `F_SETPIPE_SZ` is
    /// silently ignored - the pipe remains usable at its default capacity.
    pub fn with_capacity(requested_capacity: usize) -> std::io::Result<Self> {
        let mut pipe = Self::new()?;

        if requested_capacity > pipe.capacity {
            // SAFETY: pipe.write_fd is a valid pipe fd. F_SETPIPE_SZ sets the
            // pipe buffer to at least `requested_capacity` bytes. The kernel
            // rounds up to the nearest page boundary and may cap at pipe-max-size.
            let actual = unsafe {
                libc::fcntl(pipe.write_fd, libc::F_SETPIPE_SZ, requested_capacity as i32)
            };
            if actual > 0 {
                pipe.capacity = actual as usize;
            }
            // If fcntl failed, pipe.capacity stays at the default - still usable.
        }

        Ok(pipe)
    }

    /// Returns the actual pipe buffer capacity in bytes.
    ///
    /// This reflects what the kernel granted, which may differ from the
    /// requested capacity if `fcntl(F_SETPIPE_SZ)` was capped or failed.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the read end file descriptor.
    ///
    /// The returned fd is valid for the lifetime of this `SplicePipe`.
    #[must_use]
    pub fn read_fd(&self) -> i32 {
        self.read_fd
    }

    /// Returns the write end file descriptor.
    ///
    /// The returned fd is valid for the lifetime of this `SplicePipe`.
    #[must_use]
    pub fn write_fd(&self) -> i32 {
        self.write_fd
    }

    /// Splices data from `source_fd` to `dest_fd` using this pipe as the
    /// intermediary.
    ///
    /// This is equivalent to [`try_splice_to_file`] but reuses an existing
    /// pipe, avoiding the overhead of creating and destroying a pipe pair
    /// per transfer. Useful when performing many small splice operations.
    ///
    /// # Arguments
    ///
    /// * `source_fd` - Source file descriptor (must be a socket or pipe)
    /// * `dest_fd` - Destination file descriptor (regular file or pipe)
    /// * `len` - Number of bytes to transfer
    ///
    /// # Returns
    ///
    /// The number of bytes actually transferred.
    pub fn splice_to_file(
        &self,
        source_fd: std::os::fd::RawFd,
        dest_fd: std::os::fd::RawFd,
        len: usize,
    ) -> std::io::Result<usize> {
        splice_fd_to_file_via_pipe(self, source_fd, dest_fd, len)
    }

    /// Transfers a userspace buffer to a file via `vmsplice(2)` + `splice(2)`
    /// using this pipe as the intermediary.
    ///
    /// The buffer pages are referenced by the kernel in-flight. The caller
    /// must not modify `buf` until this function returns.
    ///
    /// # Arguments
    ///
    /// * `buf` - Source buffer to transfer
    /// * `dest_fd` - Destination file descriptor (regular file or pipe)
    ///
    /// # Returns
    ///
    /// The number of bytes actually transferred.
    pub fn vmsplice_to_file(
        &self,
        buf: &[u8],
        dest_fd: std::os::fd::RawFd,
    ) -> std::io::Result<usize> {
        use std::io;

        if buf.is_empty() {
            return Ok(0);
        }

        let mut total: usize = 0;
        let mut remaining = buf.len();

        while remaining > 0 {
            let offset = total;
            let chunk = remaining.min(SPLICE_CHUNK_SIZE);

            let iov = libc::iovec {
                iov_base: buf[offset..].as_ptr() as *mut libc::c_void,
                iov_len: chunk,
            };

            // SAFETY: The iovec points to buf[offset..offset+chunk], which is
            // valid for the duration of this call. self.write_fd is a valid pipe
            // fd owned by this SplicePipe.
            let vspliced = loop {
                let result = unsafe { libc::vmsplice(self.write_fd, &iov, 1, libc::SPLICE_F_MORE) };
                if result < 0 {
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(err);
                }
                break result as usize;
            };

            if vspliced == 0 {
                break;
            }

            drain_pipe_to_fd(self, dest_fd, vspliced)?;

            total += vspliced;
            remaining -= vspliced;
        }

        Ok(total)
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

/// Stub type for non-Linux platforms.
///
/// All methods return `Unsupported` errors. This allows consumers to write
/// platform-independent code that compiles everywhere.
#[cfg(not(target_os = "linux"))]
pub struct SplicePipe {
    _private: (),
}

#[cfg(not(target_os = "linux"))]
impl SplicePipe {
    /// Stub: always returns `Unsupported` on non-Linux.
    pub fn new() -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "SplicePipe is only available on Linux",
        ))
    }

    /// Stub: always returns `Unsupported` on non-Linux.
    pub fn with_capacity(_requested_capacity: usize) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "SplicePipe is only available on Linux",
        ))
    }

    /// Stub: returns 0.
    #[must_use]
    pub fn capacity(&self) -> usize {
        0
    }

    /// Stub: always returns `Unsupported` on non-Linux.
    pub fn splice_to_file(
        &self,
        _source_fd: i32,
        _dest_fd: i32,
        _len: usize,
    ) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "splice is only available on Linux",
        ))
    }

    /// Stub: always returns `Unsupported` on non-Linux.
    pub fn vmsplice_to_file(&self, _buf: &[u8], _dest_fd: i32) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "vmsplice is only available on Linux",
        ))
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

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_vmsplice_unavailable_on_non_linux() {
        let data = b"test data";
        let result = try_vmsplice_to_file(data, 0);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }

    #[cfg(not(target_os = "linux"))]
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
        use super::*;
        use std::io::{Read, Seek, SeekFrom};
        use tempfile::NamedTempFile;

        #[test]
        fn test_recv_fd_to_file_uses_fallback() {
            // On non-Linux unix, recv_fd_to_file uses the read/write fallback.
            let content = b"Testing recv_fd_to_file fallback on non-Linux unix";
            let mut dest = NamedTempFile::new().unwrap();

            let mut socket_fds = [0i32; 2];
            let result = unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
            };
            assert_eq!(result, 0);

            let recv_fd = socket_fds[0];
            let send_fd = socket_fds[1];

            let written = unsafe {
                libc::write(
                    send_fd,
                    content.as_ptr().cast::<libc::c_void>(),
                    content.len(),
                )
            };
            assert_eq!(written, content.len() as isize);
            unsafe { libc::close(send_fd) };

            use std::os::fd::AsRawFd;
            let received =
                recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len() as u64).unwrap();

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
            let result = unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
            };
            assert_eq!(result, 0);

            let recv_fd = socket_fds[0];
            let send_fd = socket_fds[1];

            let written = unsafe {
                libc::write(
                    send_fd,
                    content.as_ptr().cast::<libc::c_void>(),
                    content.len(),
                )
            };
            assert_eq!(written, content.len() as isize);
            unsafe { libc::close(send_fd) };

            use std::os::fd::AsRawFd;
            let received =
                copy_fd_to_fd(recv_fd, dest.as_file().as_raw_fd(), content.len() as u64).unwrap();

            unsafe { libc::close(recv_fd) };

            assert_eq!(received, content.len() as u64);

            dest.seek(SeekFrom::Start(0)).unwrap();
            let mut file_content = Vec::new();
            dest.read_to_end(&mut file_content).unwrap();
            assert_eq!(file_content, content);
        }
    }

    #[cfg(target_os = "linux")]
    mod linux_tests {
        use super::*;
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

        /// Helper: creates a socketpair with a writer thread that sends `content`,
        /// then closes the send end. Returns the recv fd.
        fn socketpair_with_writer(content: Vec<u8>) -> (i32, std::thread::JoinHandle<()>) {
            let mut socket_fds = [0i32; 2];
            let result = unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
            };
            assert_eq!(result, 0, "Failed to create socketpair");

            let recv_fd = socket_fds[0];
            let send_fd = socket_fds[1];

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
                    assert!(n > 0, "write to socket failed");
                    offset += n as usize;
                }
                unsafe { libc::close(send_fd) };
            });

            (recv_fd, handle)
        }

        #[test]
        fn test_recv_fd_to_file_small_transfer() {
            // Below SPLICE_THRESHOLD - should use read/write fallback directly.
            let content = b"Small transfer below splice threshold";
            let mut dest = NamedTempFile::new().unwrap();

            let (recv_fd, writer) = socketpair_with_writer(content.to_vec());

            use std::os::fd::AsRawFd;
            let received =
                recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len() as u64).unwrap();

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

            let (recv_fd, writer) = socketpair_with_writer(content.clone());

            use std::os::fd::AsRawFd;
            let received =
                recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

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
            let result = unsafe {
                libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
            };
            assert_eq!(result, 0);

            let recv_fd = socket_fds[0];
            let send_fd = socket_fds[1];
            unsafe { libc::close(send_fd) };

            use std::os::fd::AsRawFd;
            let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), 1024).unwrap();

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
            let size = SPLICE_THRESHOLD as usize;
            let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let mut dest = NamedTempFile::new().unwrap();

            let (recv_fd, writer) = socketpair_with_writer(content.clone());

            use std::os::fd::AsRawFd;
            let received =
                recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

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
            // Test the fallback path directly.
            let content = b"Testing copy_fd_to_fd fallback path directly";
            let mut dest = NamedTempFile::new().unwrap();

            let (recv_fd, writer) = socketpair_with_writer(content.to_vec());

            use std::os::fd::AsRawFd;
            let received =
                copy_fd_to_fd(recv_fd, dest.as_file().as_raw_fd(), content.len() as u64).unwrap();

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

            let (recv_fd, writer) = socketpair_with_writer(content.clone());

            use std::os::fd::AsRawFd;
            let received = copy_fd_to_fd(recv_fd, dest.as_file().as_raw_fd(), size as u64).unwrap();

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

            let (recv_fd, writer) = socketpair_with_writer(content.to_vec());

            use std::os::fd::AsRawFd;
            let received = recv_fd_to_file(recv_fd, dest.as_file().as_raw_fd(), 100_000).unwrap();

            unsafe { libc::close(recv_fd) };
            writer.join().expect("writer thread should succeed");

            assert_eq!(received, content.len() as u64);

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
                let content: Vec<u8> = (0..128).map(|j| j.wrapping_add(i * 64)).collect();
                let mut dest = NamedTempFile::new().unwrap();

                let (recv_fd, writer) = socketpair_with_writer(content.clone());

                use std::os::fd::AsRawFd;
                let spliced = pipe
                    .splice_to_file(recv_fd, dest.as_file().as_raw_fd(), content.len())
                    .unwrap();

                unsafe { libc::close(recv_fd) };
                writer.join().expect("writer thread should succeed");

                assert_eq!(spliced, content.len());

                dest.seek(SeekFrom::Start(0)).unwrap();
                let mut file_content = Vec::new();
                dest.read_to_end(&mut file_content).unwrap();
                assert_eq!(file_content, content);
            }
        }

        #[test]
        fn test_vmsplice_small_buffer() {
            if !is_splice_available() {
                return;
            }

            let content = b"Testing vmsplice: buffer to file via pipe intermediary";
            let mut dest = NamedTempFile::new().unwrap();

            use std::os::fd::AsRawFd;
            let transferred = try_vmsplice_to_file(content, dest.as_file().as_raw_fd()).unwrap();

            assert_eq!(transferred, content.len());

            dest.seek(SeekFrom::Start(0)).unwrap();
            let mut file_content = Vec::new();
            dest.read_to_end(&mut file_content).unwrap();
            assert_eq!(file_content, content);
        }

        #[test]
        fn test_vmsplice_empty_buffer() {
            if !is_splice_available() {
                return;
            }

            let mut dest = NamedTempFile::new().unwrap();

            use std::os::fd::AsRawFd;
            let transferred = try_vmsplice_to_file(&[], dest.as_file().as_raw_fd()).unwrap();

            assert_eq!(transferred, 0);

            dest.seek(SeekFrom::Start(0)).unwrap();
            let mut file_content = Vec::new();
            dest.read_to_end(&mut file_content).unwrap();
            assert!(file_content.is_empty());
        }

        #[test]
        fn test_vmsplice_large_buffer() {
            if !is_splice_available() {
                return;
            }

            // 512KB - multiple splice chunks worth of data.
            let size = 512 * 1024;
            let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let mut dest = NamedTempFile::new().unwrap();

            use std::os::fd::AsRawFd;
            let transferred = try_vmsplice_to_file(&content, dest.as_file().as_raw_fd()).unwrap();

            assert_eq!(transferred, size);

            dest.seek(SeekFrom::Start(0)).unwrap();
            let mut file_content = Vec::new();
            dest.read_to_end(&mut file_content).unwrap();
            assert_eq!(file_content.len(), content.len());
            assert_eq!(file_content, content);
        }

        #[test]
        fn test_vmsplice_exact_chunk_boundary() {
            if !is_splice_available() {
                return;
            }

            // Exactly SPLICE_CHUNK_SIZE bytes.
            let size = SPLICE_CHUNK_SIZE;
            let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let mut dest = NamedTempFile::new().unwrap();

            use std::os::fd::AsRawFd;
            let transferred = try_vmsplice_to_file(&content, dest.as_file().as_raw_fd()).unwrap();

            assert_eq!(transferred, size);

            dest.seek(SeekFrom::Start(0)).unwrap();
            let mut file_content = Vec::new();
            dest.read_to_end(&mut file_content).unwrap();
            assert_eq!(file_content, content);
        }

        #[test]
        fn test_vmsplice_via_splice_pipe() {
            if !is_splice_available() {
                return;
            }

            let content = b"Testing vmsplice through SplicePipe method";
            let pipe = SplicePipe::with_capacity(DEFAULT_PIPE_CAPACITY).unwrap();
            let mut dest = NamedTempFile::new().unwrap();

            use std::os::fd::AsRawFd;
            let transferred = pipe
                .vmsplice_to_file(content, dest.as_file().as_raw_fd())
                .unwrap();

            assert_eq!(transferred, content.len());

            dest.seek(SeekFrom::Start(0)).unwrap();
            let mut file_content = Vec::new();
            dest.read_to_end(&mut file_content).unwrap();
            assert_eq!(file_content, content);
        }

        #[test]
        fn test_vmsplice_invalid_fd_returns_error() {
            if !is_splice_available() {
                return;
            }

            let content = b"test data for invalid fd";
            let result = try_vmsplice_to_file(content, -1);
            assert!(result.is_err());
        }
    }
}
