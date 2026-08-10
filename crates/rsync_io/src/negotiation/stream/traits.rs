use std::fmt;
use std::io::{self, BufRead, IoSlice, IoSliceMut, Read, Write};

use super::base::NegotiatedStream;

impl<R: Read> Read for NegotiatedStream<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let copied = self.buffer_storage_mut().copy_into(buf);
        if copied > 0 {
            return Ok(copied);
        }

        self.inner_mut().read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        if bufs.is_empty() {
            return Ok(0);
        }

        let copied = self.buffer_storage_mut().copy_into_vectored(bufs);
        if copied > 0 {
            return Ok(copied);
        }

        self.inner_mut().read_vectored(bufs)
    }
}

impl<R: BufRead> BufRead for NegotiatedStream<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.buffer_storage().has_remaining() {
            return Ok(self.buffer_storage().remaining_slice());
        }

        self.inner_mut().fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        let remainder = self.consume_buffered(amt);
        if remainder > 0 {
            BufRead::consume(self.inner_mut(), remainder);
        }
    }
}

impl<R: Write> Write for NegotiatedStream<R> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner_mut().write(buf)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.inner_mut().write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner_mut().flush()
    }

    fn write_fmt(&mut self, fmt: fmt::Arguments<'_>) -> io::Result<()> {
        self.inner_mut().write_fmt(fmt)
    }
}
