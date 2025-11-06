//! Common utility types shared by the compression back-ends.

use std::io::{self, IoSlice, Write};

/// Sink used by counting encoders when callers do not provide an explicit writer.
///
/// The sink discards all written bytes while allowing the encoder to keep track of
/// the compressed length. It is exposed so downstream crates can reference the
/// default type parameter used by the encoder constructors.
#[derive(Clone, Copy, Debug, Default)]
pub struct CountingSink;

impl Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        Ok(bufs.iter().map(|slice| slice.len()).sum())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) struct CountingWriter<W> {
    inner: W,
    bytes: u64,
}

impl<W> CountingWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self { inner, bytes: 0 }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn inner_ref(&self) -> &W {
        &self.inner
    }

    pub(crate) fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    pub(crate) fn into_parts(self) -> (W, u64) {
        (self.inner, self.bytes)
    }

    pub(crate) fn saturating_add_bytes(&mut self, written: usize) {
        self.bytes = self.bytes.saturating_add(written as u64);
    }
}

impl<W> Write for CountingWriter<W>
where
    W: Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.saturating_add_bytes(written);
        Ok(written)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let written = self.inner.write_vectored(bufs)?;
        self.saturating_add_bytes(written);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
