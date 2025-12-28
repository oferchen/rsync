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
    pub(crate) const fn new(inner: W) -> Self {
        Self { inner, bytes: 0 }
    }

    pub(crate) const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) const fn inner_ref(&self) -> &W {
        &self.inner
    }

    pub(crate) const fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    pub(crate) fn into_parts(self) -> (W, u64) {
        (self.inner, self.bytes)
    }

    pub(crate) const fn saturating_add_bytes(&mut self, written: usize) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counting_sink_write_returns_full_length() {
        let mut sink = CountingSink;
        let result = sink.write(b"hello world").unwrap();
        assert_eq!(result, 11);
    }

    #[test]
    fn counting_sink_write_empty_returns_zero() {
        let mut sink = CountingSink;
        let result = sink.write(b"").unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn counting_sink_write_vectored_sums_lengths() {
        let mut sink = CountingSink;
        let bufs = [
            IoSlice::new(b"hello"),
            IoSlice::new(b" "),
            IoSlice::new(b"world"),
        ];
        let result = sink.write_vectored(&bufs).unwrap();
        assert_eq!(result, 11);
    }

    #[test]
    fn counting_sink_write_vectored_empty_returns_zero() {
        let mut sink = CountingSink;
        let result = sink.write_vectored(&[]).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn counting_sink_flush_succeeds() {
        let mut sink = CountingSink;
        assert!(sink.flush().is_ok());
    }

    #[test]
    fn counting_sink_is_clone() {
        let sink = CountingSink;
        let cloned = sink;
        assert_eq!(std::mem::size_of_val(&sink), std::mem::size_of_val(&cloned));
    }

    #[test]
    fn counting_sink_is_copy() {
        let sink = CountingSink;
        let copied = sink;
        assert_eq!(std::mem::size_of_val(&sink), std::mem::size_of_val(&copied));
    }

    #[test]
    fn counting_sink_default() {
        let sink = CountingSink;
        let mut w = sink;
        assert!(w.write(b"test").is_ok());
    }

    #[test]
    fn counting_sink_debug() {
        let sink = CountingSink;
        let debug = format!("{sink:?}");
        assert!(debug.contains("CountingSink"));
    }

    #[test]
    fn counting_writer_new_starts_at_zero() {
        let writer = CountingWriter::new(Vec::<u8>::new());
        assert_eq!(writer.bytes(), 0);
    }

    #[test]
    fn counting_writer_write_counts_bytes() {
        let mut writer = CountingWriter::new(Vec::new());
        writer.write_all(b"hello world").unwrap();
        assert_eq!(writer.bytes(), 11);
    }

    #[test]
    fn counting_writer_multiple_writes_accumulate() {
        let mut writer = CountingWriter::new(Vec::new());
        writer.write_all(b"hello").unwrap();
        writer.write_all(b" ").unwrap();
        writer.write_all(b"world").unwrap();
        assert_eq!(writer.bytes(), 11);
    }

    #[test]
    fn counting_writer_write_vectored_counts() {
        let mut writer = CountingWriter::new(Vec::new());
        let bufs = [IoSlice::new(b"hello"), IoSlice::new(b" world")];
        let written = writer.write_vectored(&bufs).unwrap();
        assert_eq!(written, 11);
        assert_eq!(writer.bytes(), 11);
    }

    #[test]
    fn counting_writer_inner_ref_returns_reference() {
        let writer = CountingWriter::new(Vec::new());
        let inner: &Vec<u8> = writer.inner_ref();
        assert!(inner.is_empty());
    }

    #[test]
    fn counting_writer_inner_mut_allows_modification() {
        let mut writer = CountingWriter::new(Vec::new());
        writer.inner_mut().push(42);
        assert_eq!(writer.inner_ref().len(), 1);
    }

    #[test]
    fn counting_writer_into_parts_returns_both() {
        let mut writer = CountingWriter::new(Vec::new());
        writer.write_all(b"test").unwrap();
        let (inner, bytes) = writer.into_parts();
        assert_eq!(inner, b"test");
        assert_eq!(bytes, 4);
    }

    #[test]
    fn counting_writer_flush_flushes_inner() {
        let mut writer = CountingWriter::new(Vec::new());
        writer.write_all(b"test").unwrap();
        assert!(writer.flush().is_ok());
    }

    #[test]
    fn counting_writer_saturating_add_handles_large_values() {
        let mut writer = CountingWriter::new(Vec::<u8>::new());
        writer.saturating_add_bytes(u64::MAX as usize);
        writer.saturating_add_bytes(1);
        assert_eq!(writer.bytes(), u64::MAX);
    }
}
