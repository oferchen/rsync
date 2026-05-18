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

mod probe;
mod syscalls;

pub use probe::{is_splice_available, is_splice_enabled};
pub use syscalls::{recv_fd_to_file, try_splice_to_file, try_vmsplice_to_file};

#[cfg(target_os = "linux")]
use syscalls::{drain_pipe_to_fd, splice_fd_to_file_via_pipe};

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
        #[allow(unsafe_code)]
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
mod tests;
