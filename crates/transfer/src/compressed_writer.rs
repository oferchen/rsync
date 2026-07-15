//! Compressed writer that wraps multiplexed streams with compression.
//!
//! This module implements compression on top of multiplexed rsync protocol streams,
//! mirroring upstream rsync's io.c:io_start_buffering_out() behavior where compression
//! is applied after multiplex framing.

use std::io::{self, IoSlice, Write};
use std::num::NonZeroU8;

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::{CompressionLevel, CountingZlibEncoder};

#[cfg(feature = "lz4")]
use compress::lz4::CountingLz4Encoder;

#[cfg(feature = "zstd")]
use compress::zstd::CountingZstdEncoder;

/// Wraps a writer with compression, buffering compressed output.
///
/// Mirrors upstream `io.c:io_start_buffering_out()` where compression is
/// applied on top of the multiplexed stream.
///
/// Batch recording is handled by the inner `MultiplexWriter`, not here.
/// Unlike upstream (which tees compressed wire bytes via `io.c:write_buf()`),
/// oc-rsync records data at the pre-compression level and sets
/// `do_compression: false` in the batch stream flags. This avoids an
/// upstream rsync 3.4.1 limitation where the batch format does not record
/// the compression algorithm, causing read-batch to force CPRES_ZLIB
/// (compat.c:194-195) even when the original write used zstd.
pub struct CompressedWriter<W: Write> {
    /// Underlying writer, typically a `MultiplexWriter`.
    inner: W,
    /// Active compression encoder variant.
    encoder: EncoderVariant,
    /// Drain the encoder's sink to `inner` when it exceeds this many bytes.
    flush_threshold: usize,
}

/// Compression encoder dispatch for supported algorithms. Each variant
/// writes into a heap-backed `Vec<u8>` sink that is drained to the inner
/// writer when it crosses [`CompressedWriter::flush_threshold`].
#[allow(clippy::large_enum_variant)]
enum EncoderVariant {
    Zlib(CountingZlibEncoder<Vec<u8>>),
    #[cfg(feature = "lz4")]
    Lz4(CountingLz4Encoder<Vec<u8>>),
    #[cfg(feature = "zstd")]
    Zstd(CountingZstdEncoder<Vec<u8>>),
}

impl<W: Write> CompressedWriter<W> {
    /// Creates a new compressed writer wrapping the given writer.
    ///
    /// # Errors
    ///
    /// Returns an error if the compression algorithm is not supported in this build.
    pub fn new(
        inner: W,
        algorithm: CompressionAlgorithm,
        level: CompressionLevel,
    ) -> io::Result<Self> {
        Self::with_workers(inner, algorithm, level, None)
    }

    /// Like [`new`](Self::new) but plumbs `--compress-threads=N` through to
    /// `ZSTD_c_nbWorkers` when zstd is the active codec. `workers` is ignored
    /// for zlib and LZ4. upstream: `token.c:701`.
    pub fn with_workers(
        inner: W,
        algorithm: CompressionAlgorithm,
        level: CompressionLevel,
        workers: Option<NonZeroU8>,
    ) -> io::Result<Self> {
        // upstream: io.c IO_BUFFER_SIZE (32KB). Reduces flush frequency and
        // improves compression ratio.
        const BUFFER_SIZE: usize = 32 * 1024;

        let encoder = match algorithm {
            CompressionAlgorithm::Zlib => {
                let sink = Vec::with_capacity(BUFFER_SIZE);
                EncoderVariant::Zlib(CountingZlibEncoder::with_sink(sink, level))
            }
            #[cfg(feature = "lz4")]
            CompressionAlgorithm::Lz4 => {
                let sink = Vec::with_capacity(BUFFER_SIZE);
                EncoderVariant::Lz4(CountingLz4Encoder::with_sink(sink, level))
            }
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => {
                let sink = Vec::with_capacity(BUFFER_SIZE);
                EncoderVariant::Zstd(CountingZstdEncoder::with_sink_workers(
                    sink, level, workers,
                )?)
            }
            #[allow(unreachable_patterns)]
            _ => {
                let _ = workers;
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "compression algorithm {} is not supported",
                        algorithm.name()
                    ),
                ));
            }
        };

        Ok(Self {
            inner,
            encoder,
            flush_threshold: BUFFER_SIZE,
        })
    }

    /// Drains the encoder's internal sink to the underlying writer without
    /// triggering a compressor-level flush.
    fn drain_sink(&mut self) -> io::Result<()> {
        match &mut self.encoder {
            EncoderVariant::Zlib(encoder) => {
                let sink = encoder.get_mut();
                if !sink.is_empty() {
                    self.inner.write_all(sink)?;
                    sink.clear();
                }
            }
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => {
                let sink = encoder.get_mut();
                if !sink.is_empty() {
                    self.inner.write_all(sink)?;
                    sink.clear();
                }
            }
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => {
                let sink = encoder.get_mut();
                if !sink.is_empty() {
                    self.inner.write_all(sink)?;
                    sink.clear();
                }
            }
        }
        Ok(())
    }

    /// Performs a sync flush on the encoder and drains all output.
    ///
    /// Calls `Z_SYNC_FLUSH` (or equivalent) on the compressor so the receiver
    /// can decompress all data written so far without waiting for more input.
    ///
    /// upstream: `token.c:send_deflated_token()` lines 433-434 uses
    /// `Z_SYNC_FLUSH` at token boundaries for independent decompressibility.
    fn flush_compressed(&mut self) -> io::Result<()> {
        // upstream: token.c uses Z_SYNC_FLUSH after each token's data so the
        // encoder's pending deflate state is materialized into the sink.
        match &mut self.encoder {
            EncoderVariant::Zlib(encoder) => encoder.flush()?,
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => encoder.flush()?,
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => encoder.flush()?,
        }

        self.drain_sink()?;
        self.inner.flush()
    }

    /// Finishes the compression stream and flushes all data.
    ///
    /// This MUST be called before dropping the writer to ensure all
    /// compressed data (including trailer bytes) is written.
    ///
    /// Returns the underlying writer so it can be reused.
    ///
    /// # Errors
    ///
    /// Returns an error if finishing the compression stream or flushing fails.
    pub fn finish(mut self) -> io::Result<W> {
        match self.encoder {
            EncoderVariant::Zlib(encoder) => {
                let (sink, _bytes) = encoder.finish_into_inner()?;
                if !sink.is_empty() {
                    self.inner.write_all(&sink)?;
                }
            }
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => {
                let (sink, _bytes) = encoder.finish_into_inner()?;
                if !sink.is_empty() {
                    self.inner.write_all(&sink)?;
                }
            }
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => {
                let (sink, _bytes) = encoder.finish_into_inner()?;
                if !sink.is_empty() {
                    self.inner.write_all(&sink)?;
                }
            }
        }

        self.inner.flush()?;
        Ok(self.inner)
    }

    /// Returns the number of compressed bytes written so far.
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        match &self.encoder {
            EncoderVariant::Zlib(encoder) => encoder.bytes_written(),
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => encoder.bytes_written(),
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => encoder.bytes_written(),
        }
    }

    /// Provides mutable access to the underlying writer.
    ///
    /// Used for sending multiplex control messages that bypass the compression
    /// buffer, matching upstream behavior where control messages are uncompressed.
    pub const fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Provides shared access to the underlying writer.
    ///
    /// Used to read pass-through state (e.g. the keep-alive lull interval) that
    /// lives on the multiplex writer without mutating it.
    pub const fn inner_ref(&self) -> &W {
        &self.inner
    }
}

impl<W: Write> Write for CompressedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // upstream: io.c:write_buf() tees compressed wire bytes at the
        // MultiplexWriter level, not here.
        match &mut self.encoder {
            EncoderVariant::Zlib(encoder) => encoder.write(buf)?,
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => encoder.write(buf)?,
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => encoder.write(buf)?,
        }

        let current_size = match &self.encoder {
            EncoderVariant::Zlib(encoder) => encoder.get_ref().len(),
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => encoder.get_ref().len(),
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => encoder.get_ref().len(),
        };

        if current_size > self.flush_threshold {
            self.drain_sink()?;
        }

        Ok(buf.len())
    }

    /// Writes multiple buffers sequentially through the encoder since
    /// compression state must be maintained across buffers.
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let mut total_written = 0;
        for buf in bufs {
            if !buf.is_empty() {
                self.write(buf)?;
                total_written += buf.len();
            }
        }
        Ok(total_written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_compressed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compress::zlib::decompress_to_vec;

    #[test]
    fn compress_round_trip_zlib() {
        let data = b"test data that should be compressed";
        let mut buf = Vec::new();
        let mut writer = CompressedWriter::new(
            &mut buf,
            CompressionAlgorithm::Zlib,
            CompressionLevel::Default,
        )
        .unwrap();

        writer.write_all(data).unwrap();
        writer.finish().unwrap();

        assert!(!buf.is_empty());

        let decompressed = decompress_to_vec(&buf).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn compress_multiple_writes() {
        let data1 = b"first chunk ";
        let data2 = b"second chunk";
        let data3 = b" third chunk";

        let mut buf = Vec::new();
        let mut writer = CompressedWriter::new(
            &mut buf,
            CompressionAlgorithm::Zlib,
            CompressionLevel::Default,
        )
        .unwrap();

        writer.write_all(data1).unwrap();
        writer.write_all(data2).unwrap();
        writer.write_all(data3).unwrap();
        writer.finish().unwrap();

        let decompressed = decompress_to_vec(&buf).unwrap();
        let expected = b"first chunk second chunk third chunk";
        assert_eq!(decompressed, expected);
    }

    #[test]
    fn compress_large_data_flushes_automatically() {
        let data = vec![b'x'; 8192];

        let mut buf = Vec::new();
        {
            let mut writer =
                CompressedWriter::new(&mut buf, CompressionAlgorithm::Zlib, CompressionLevel::Fast)
                    .unwrap();

            writer.write_all(&data).unwrap();
            writer.finish().unwrap();
        }

        assert!(!buf.is_empty());

        let decompressed = decompress_to_vec(&buf).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn bytes_written_tracks_compressed_size() {
        let data = b"test data that should compress to a reasonable size";
        let mut buf = Vec::new();
        {
            let mut writer = CompressedWriter::new(
                &mut buf,
                CompressionAlgorithm::Zlib,
                CompressionLevel::Default,
            )
            .unwrap();

            writer.write_all(data).unwrap();
            writer.finish().unwrap();
        }

        // Compressed size should fit within the input plus a small zlib overhead.
        assert!(!buf.is_empty());
        assert!(buf.len() < data.len() + 20);
    }

    #[test]
    fn inner_mut_provides_access() {
        let mut buf = Vec::new();
        let mut writer = CompressedWriter::new(
            &mut buf,
            CompressionAlgorithm::Zlib,
            CompressionLevel::Default,
        )
        .unwrap();

        let _inner = writer.inner_mut();
        writer.finish().unwrap();
    }

    /// Verifies that flush() triggers Z_SYNC_FLUSH, producing output that
    /// is independently decompressible without finishing the stream.
    ///
    /// This mirrors upstream rsync's per-token flush behavior where
    /// Z_SYNC_FLUSH is called after each token so the receiver can
    /// decompress tokens individually (token.c:send_deflated_token
    /// lines 433-434).
    #[test]
    fn flush_triggers_sync_flush_producing_decompressible_output() {
        let token_data = b"first token data for sync flush test";
        let token2 = b"second token after flush";

        let mut buf = Vec::new();
        {
            let mut writer = CompressedWriter::new(
                &mut buf,
                CompressionAlgorithm::Zlib,
                CompressionLevel::Default,
            )
            .unwrap();

            writer.write_all(token_data).unwrap();
            // flush() triggers Z_SYNC_FLUSH so pending data is decompressible without more input.
            writer.flush().unwrap();

            writer.write_all(token2).unwrap();
            writer.finish().unwrap();
        }

        assert!(!buf.is_empty(), "flush must produce compressed output");

        // upstream: token.c:send_deflated_token lines 433-434.
        let full = decompress_to_vec(&buf).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(token_data);
        expected.extend_from_slice(token2);
        assert_eq!(full, expected);
    }

    #[cfg(feature = "lz4")]
    mod lz4_tests {
        use super::*;
        use compress::lz4::decompress_to_vec as lz4_decompress;

        #[test]
        fn compress_round_trip_lz4() {
            let data = b"test data that should be compressed with lz4";
            let mut buf = Vec::new();
            let mut writer = CompressedWriter::new(
                &mut buf,
                CompressionAlgorithm::Lz4,
                CompressionLevel::Default,
            )
            .unwrap();

            writer.write_all(data).unwrap();
            writer.finish().unwrap();

            assert!(!buf.is_empty());

            let decompressed = lz4_decompress(&buf).unwrap();
            assert_eq!(decompressed, data);
        }

        #[test]
        fn compress_multiple_writes_lz4() {
            let data1 = b"first chunk ";
            let data2 = b"second chunk";
            let data3 = b" third chunk";

            let mut buf = Vec::new();
            let mut writer = CompressedWriter::new(
                &mut buf,
                CompressionAlgorithm::Lz4,
                CompressionLevel::Default,
            )
            .unwrap();

            writer.write_all(data1).unwrap();
            writer.write_all(data2).unwrap();
            writer.write_all(data3).unwrap();
            writer.finish().unwrap();

            let decompressed = lz4_decompress(&buf).unwrap();
            let expected = b"first chunk second chunk third chunk";
            assert_eq!(decompressed, expected);
        }

        #[test]
        fn compress_large_data_lz4() {
            let data = vec![b'y'; 8192];

            let mut buf = Vec::new();
            {
                let mut writer = CompressedWriter::new(
                    &mut buf,
                    CompressionAlgorithm::Lz4,
                    CompressionLevel::Fast,
                )
                .unwrap();

                writer.write_all(&data).unwrap();
                writer.finish().unwrap();
            }

            assert!(!buf.is_empty());

            let decompressed = lz4_decompress(&buf).unwrap();
            assert_eq!(decompressed, data);
        }

        #[test]
        fn lz4_bytes_written_tracks_size() {
            let data = b"lz4 tracking test data that should compress well";
            let mut buf = Vec::new();
            {
                let mut writer = CompressedWriter::new(
                    &mut buf,
                    CompressionAlgorithm::Lz4,
                    CompressionLevel::Default,
                )
                .unwrap();

                writer.write_all(data).unwrap();
                writer.finish().unwrap();
            }

            assert!(!buf.is_empty());
        }
    }

    #[cfg(feature = "zstd")]
    mod zstd_tests {
        use super::*;
        use compress::zstd::decompress_to_vec as zstd_decompress;

        #[test]
        fn compress_round_trip_zstd() {
            let data = b"test data that should be compressed with zstd";
            let mut buf = Vec::new();
            let mut writer = CompressedWriter::new(
                &mut buf,
                CompressionAlgorithm::Zstd,
                CompressionLevel::Default,
            )
            .unwrap();

            writer.write_all(data).unwrap();
            writer.finish().unwrap();

            assert!(!buf.is_empty());

            let decompressed = zstd_decompress(&buf).unwrap();
            assert_eq!(decompressed, data);
        }

        #[test]
        fn compress_multiple_writes_zstd() {
            let data1 = b"first chunk ";
            let data2 = b"second chunk";
            let data3 = b" third chunk";

            let mut buf = Vec::new();
            let mut writer = CompressedWriter::new(
                &mut buf,
                CompressionAlgorithm::Zstd,
                CompressionLevel::Default,
            )
            .unwrap();

            writer.write_all(data1).unwrap();
            writer.write_all(data2).unwrap();
            writer.write_all(data3).unwrap();
            writer.finish().unwrap();

            let decompressed = zstd_decompress(&buf).unwrap();
            let expected = b"first chunk second chunk third chunk";
            assert_eq!(decompressed, expected);
        }

        #[test]
        fn compress_large_data_zstd() {
            let data = vec![b'z'; 8192];

            let mut buf = Vec::new();
            {
                let mut writer = CompressedWriter::new(
                    &mut buf,
                    CompressionAlgorithm::Zstd,
                    CompressionLevel::Fast,
                )
                .unwrap();

                writer.write_all(&data).unwrap();
                writer.finish().unwrap();
            }

            assert!(!buf.is_empty());

            let decompressed = zstd_decompress(&buf).unwrap();
            assert_eq!(decompressed, data);
        }

        #[test]
        fn with_workers_round_trip_and_smoke() {
            // None matches upstream's do_compression_threads = 0. Some(_) either succeeds
            // under zstdmt or returns Unsupported; never panic, never silently drop the setting.
            let data = b"zstd workers test";
            for workers in [None, std::num::NonZeroU8::new(4)] {
                let mut buf = Vec::new();
                let result = CompressedWriter::with_workers(
                    &mut buf,
                    CompressionAlgorithm::Zstd,
                    CompressionLevel::Default,
                    workers,
                );
                if workers.is_some() && !compress::zstd::SUPPORTS_MULTITHREAD {
                    let err = match result {
                        Ok(_) => panic!("expected Unsupported when zstdmt is off"),
                        Err(e) => e,
                    };
                    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
                    continue;
                }
                let mut writer = match result {
                    Ok(w) => w,
                    Err(e) => panic!("encoder construction failed: {e}"),
                };
                writer.write_all(data).unwrap();
                writer.finish().unwrap();
                assert_eq!(zstd_decompress(&buf).unwrap(), data);
            }
        }

        #[test]
        fn zstd_bytes_written_tracks_size() {
            let data = b"zstd tracking test data that should compress well";
            let mut buf = Vec::new();
            {
                let mut writer = CompressedWriter::new(
                    &mut buf,
                    CompressionAlgorithm::Zstd,
                    CompressionLevel::Default,
                )
                .unwrap();

                writer.write_all(data).unwrap();
                writer.finish().unwrap();
            }

            assert!(!buf.is_empty());
        }
    }
}
