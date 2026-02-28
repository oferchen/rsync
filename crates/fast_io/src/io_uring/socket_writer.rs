//! io_uring-based socket writer using `IORING_OP_SEND`.

use std::io::{self, Write};
use std::os::unix::io::RawFd;

use io_uring::IoUring as RawIoUring;

use super::batching::{sqe_fd, submit_send_batch, try_register_fd};
use super::config::IoUringConfig;

/// io_uring-based socket writer using `IORING_OP_SEND`.
///
/// Replaces direct `write()` calls on `TcpStream` by batching sends through
/// the io_uring ring. Maintains an internal write buffer, flushing via
/// batched send SQEs.
pub struct IoUringSocketWriter {
    ring: RawIoUring,
    fd: RawFd,
    fixed_fd_slot: i32,
    buffer: Vec<u8>,
    buffer_pos: usize,
    buffer_size: usize,
    sq_entries: u32,
}

impl IoUringSocketWriter {
    /// Creates a socket writer from a raw file descriptor.
    ///
    /// The caller must ensure `fd` remains valid for the lifetime of this writer.
    /// The writer does NOT take ownership of the fd — it will not close it on drop.
    pub fn from_raw_fd(fd: RawFd, config: &IoUringConfig) -> io::Result<Self> {
        let ring = config.build_ring()?;
        let fixed_fd_slot = try_register_fd(&ring, fd, config.register_files);

        Ok(Self {
            ring,
            fd,
            fixed_fd_slot,
            buffer: vec![0u8; config.buffer_size],
            buffer_pos: 0,
            buffer_size: config.buffer_size,
            sq_entries: config.sq_entries,
        })
    }

    /// Flushes the internal buffer via batched `IORING_OP_SEND` submissions.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer_pos == 0 {
            return Ok(());
        }

        let fd = sqe_fd(self.fd, self.fixed_fd_slot);
        let len = self.buffer_pos;

        let sent = submit_send_batch(
            &mut self.ring,
            fd,
            &self.buffer[..len],
            self.buffer_size,
            self.sq_entries as usize,
            self.fixed_fd_slot,
        )?;

        if sent != len {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "batched send incomplete",
            ));
        }

        self.buffer_pos = 0;
        Ok(())
    }
}

impl Write for IoUringSocketWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.buffer_pos + buf.len() <= self.buffer_size {
            self.buffer[self.buffer_pos..self.buffer_pos + buf.len()].copy_from_slice(buf);
            self.buffer_pos += buf.len();
            return Ok(buf.len());
        }

        self.flush_buffer()?;

        if buf.len() >= self.buffer_size {
            let fd = sqe_fd(self.fd, self.fixed_fd_slot);
            let sent = submit_send_batch(
                &mut self.ring,
                fd,
                buf,
                self.buffer_size,
                self.sq_entries as usize,
                self.fixed_fd_slot,
            )?;
            return Ok(sent);
        }

        self.buffer[..buf.len()].copy_from_slice(buf);
        self.buffer_pos = buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()
    }
}

impl Drop for IoUringSocketWriter {
    fn drop(&mut self) {
        let _ = self.flush_buffer();
    }
}
