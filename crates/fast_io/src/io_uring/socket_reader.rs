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
    /// The reader does NOT take ownership of the fd — it will not close it on drop.
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

    /// Fills the internal buffer by submitting a single `IORING_OP_RECV` SQE.
    fn fill_buffer(&mut self) -> io::Result<usize> {
        let fd = sqe_fd(self.fd, self.fixed_fd_slot);
        let entry = opcode::Recv::new(fd, self.buffer.as_mut_ptr(), self.buffer_size as u32)
            .build()
            .user_data(0);
        let entry = maybe_fixed_file(entry, self.fixed_fd_slot);

        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| io::Error::other("submission queue full"))?;
        }

        self.ring.submit_and_wait(1)?;

        let cqe = self
            .ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::other("missing CQE"))?;

        let result = cqe.result();
        if result < 0 {
            return Err(io::Error::from_raw_os_error(-result));
        }

        let n = result as usize;
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
                let fd = sqe_fd(self.fd, self.fixed_fd_slot);
                let entry = opcode::Recv::new(fd, buf.as_mut_ptr(), buf.len() as u32)
                    .build()
                    .user_data(0);
                let entry = maybe_fixed_file(entry, self.fixed_fd_slot);

                unsafe {
                    self.ring
                        .submission()
                        .push(&entry)
                        .map_err(|_| io::Error::other("submission queue full"))?;
                }
                self.ring.submit_and_wait(1)?;

                let cqe = self
                    .ring
                    .completion()
                    .next()
                    .ok_or_else(|| io::Error::other("missing CQE"))?;

                let result = cqe.result();
                if result < 0 {
                    return Err(io::Error::from_raw_os_error(-result));
                }
                return Ok(result as usize);
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
