//! io_uring-based socket reader using `IORING_OP_RECV`.

use std::io::{self, Read};
use std::os::unix::io::RawFd;

use io_uring::{IoUring as RawIoUring, opcode};

use super::batching::{maybe_fixed_file, sqe_fd, try_register_fd};
use super::config::IoUringConfig;

/// io_uring-based socket reader using `IORING_OP_RECV`.
///
/// Replaces `BufReader<TcpStream>` on Linux 5.6+ by submitting recv
/// operations through the io_uring ring instead of blocking `read()` syscalls.
/// Maintains an internal buffer identical in purpose to `BufReader`.
pub struct IoUringSocketReader {
    ring: RawIoUring,
    fd: RawFd,
    fixed_fd_slot: i32,
    buffer: Vec<u8>,
    pos: usize,
    len: usize,
    buffer_size: usize,
}

impl IoUringSocketReader {
    /// Creates a socket reader from a raw file descriptor.
    ///
    /// The caller must ensure `fd` remains valid for the lifetime of this reader.
    /// The reader does NOT take ownership of the fd - it will not close it on drop.
    ///
    /// # Errors
    ///
    /// Returns the io_uring ring construction error.
    pub fn from_raw_fd(fd: RawFd, config: &IoUringConfig) -> io::Result<Self> {
        let ring = config.build_ring()?;
        let fixed_fd_slot = try_register_fd(&ring, fd, config.register_files);

        Ok(Self {
            ring,
            fd,
            fixed_fd_slot,
            buffer: vec![0u8; config.buffer_size],
            pos: 0,
            len: 0,
            buffer_size: config.buffer_size,
        })
    }

    /// Receives up to `len` bytes into `ptr` with one `IORING_OP_RECV` SQE.
    ///
    /// The SQE is submitted exactly once. A signal that interrupts the wait
    /// resumes the wait rather than resubmitting, so a second RECV never
    /// races the first into the same buffer; `io_uring_enter(2)` may also
    /// return after submitting without a CQE, which the loop absorbs too.
    fn recv(&mut self, ptr: *mut u8, len: usize) -> io::Result<usize> {
        let fd = sqe_fd(self.fd, self.fixed_fd_slot);
        let entry = opcode::Recv::new(fd, ptr, len as u32).build().user_data(0);
        let entry = maybe_fixed_file(entry, self.fixed_fd_slot);

        // SAFETY: `entry` references `len` writable bytes at `ptr` (the
        // caller's slice or `self.buffer`) and the socket fd; both remain
        // valid until the CQE is reaped below, so the kernel can write into
        // the buffer before we observe completion.
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| io::Error::other("submission queue full"))?;
        }

        let result = loop {
            if let Some(cqe) = self.ring.completion().next() {
                break cqe.result();
            }
            match self.ring.submit_and_wait(1) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        };
        if result < 0 {
            return Err(io::Error::from_raw_os_error(-result));
        }
        Ok(result as usize)
    }

    /// Refills the internal buffer from the socket.
    fn fill_buffer(&mut self) -> io::Result<usize> {
        let ptr = self.buffer.as_mut_ptr();
        let n = self.recv(ptr, self.buffer_size)?;
        self.pos = 0;
        self.len = n;
        Ok(n)
    }
}

impl Read for IoUringSocketReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len {
            if buf.len() >= self.buffer_size {
                // Large read: bypass internal buffer, recv directly into caller's buffer.
                return self.recv(buf.as_mut_ptr(), buf.len());
            }

            let n = self.fill_buffer()?;
            if n == 0 {
                return Ok(0);
            }
        }

        let available = self.len - self.pos;
        let to_copy = available.min(buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[self.pos..self.pos + to_copy]);
        self.pos += to_copy;
        Ok(to_copy)
    }
}
