use std::io::{self, Write};

use crate::zlib::{CompressionLevel, CountingZlibEncoder};

use super::CompressionDecider;

/// Streaming compression filter that can dynamically skip compression.
///
/// This writer wraps another writer and optionally compresses data based on
/// auto-detection results from the first block. The compressor buffers initial
/// data to make a compression decision, then either passes data through directly
/// or compresses it.
pub struct AdaptiveCompressor<W: Write> {
    inner: W,
    decider: CompressionDecider,
    buffer: Vec<u8>,
    compress_buffer: Vec<u8>,
    decision_made: bool,
    should_compress: bool,
    level: CompressionLevel,
}

impl<W: Write> AdaptiveCompressor<W> {
    /// Creates a new adaptive compressor.
    pub fn new(inner: W, decider: CompressionDecider, level: CompressionLevel) -> Self {
        let sample_size = decider.sample_size();
        Self {
            inner,
            decider,
            buffer: Vec::with_capacity(sample_size),
            compress_buffer: Vec::new(),
            decision_made: false,
            should_compress: true,
            level,
        }
    }

    /// Forces a compression decision without auto-detection.
    pub fn set_decision(&mut self, should_compress: bool) {
        self.decision_made = true;
        self.should_compress = should_compress;
    }

    /// Returns whether compression was decided to be used.
    ///
    /// Returns `None` if the decision hasn't been made yet.
    pub fn compression_enabled(&self) -> Option<bool> {
        if self.decision_made {
            Some(self.should_compress)
        } else {
            None
        }
    }

    /// Makes the compression decision based on buffered sample data.
    fn make_decision(&mut self) -> io::Result<()> {
        if self.decision_made {
            return Ok(());
        }

        self.should_compress = self.decider.auto_detect_compressible(&self.buffer)?;
        self.decision_made = true;

        // Flush buffered data according to decision
        if self.should_compress {
            // Compress buffered data
            let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), self.level);
            encoder.write_all(&self.buffer)?;
            let (compressed, _) = encoder.finish_into_inner()?;
            self.compress_buffer = compressed;
        } else {
            // Write buffered data directly
            self.inner.write_all(&self.buffer)?;
        }

        self.buffer.clear();
        Ok(())
    }

    /// Finishes the compression stream and returns the inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        // Make decision if we haven't yet (for small files)
        if !self.decision_made {
            self.make_decision()?;
        }

        // Write any remaining compressed data
        if !self.compress_buffer.is_empty() {
            self.inner.write_all(&self.compress_buffer)?;
        }

        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for AdaptiveCompressor<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.decision_made {
            // Buffer data until we have enough for auto-detection
            let remaining = self.decider.sample_size().saturating_sub(self.buffer.len());

            if remaining > 0 {
                let to_buffer = buf.len().min(remaining);
                self.buffer.extend_from_slice(&buf[..to_buffer]);

                // If we still don't have enough, report buffered amount
                if self.buffer.len() < self.decider.sample_size() {
                    return Ok(to_buffer);
                }
            }

            // We have enough data, make the decision
            self.make_decision()?;

            // Write any remaining data from this call
            if remaining < buf.len() {
                let written = self.write(&buf[remaining..])?;
                return Ok(written + remaining);
            }

            return Ok(buf.len());
        }

        // Decision already made, write data accordingly
        if self.should_compress {
            // Compress this chunk and add to buffer
            let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), self.level);
            encoder.write_all(buf)?;
            let (compressed, _) = encoder.finish_into_inner()?;
            self.compress_buffer.extend_from_slice(&compressed);
            Ok(buf.len())
        } else {
            self.inner.write(buf)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // Write any pending compressed data
        if !self.compress_buffer.is_empty() {
            self.inner.write_all(&self.compress_buffer)?;
            self.compress_buffer.clear();
        }
        self.inner.flush()
    }
}
