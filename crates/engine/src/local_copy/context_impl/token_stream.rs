// The one compressed-token encoder a local copy runs.

/// The `-z` token stream of a local copy: the only compressor a run builds.
///
/// The batch file, the wire tally's "Total bytes sent", the bandwidth limiter
/// and the per-file compressed count all take the byte counts this stream
/// reports for the tokens it writes, so a literal byte is compressed once
/// however many of those consumers are active.
///
/// upstream: token.c:370 `send_deflated_token()` (and `send_zstd_token()`,
/// `send_compressed_token()`) is the sender's single encoder; the batch file is
/// a tee of what it writes (io.c:2511-2513 `write_buf()`), and the same bytes
/// are counted in `stats.total_written` and charged to the bandwidth limit
/// (io.c:977-980), so nothing compresses the data a second time.
pub(crate) struct TokenStream {
    encoder: protocol::wire::CompressedTokenEncoder,
    /// Bytes written for the current file so far.
    file_bytes: u64,
}

/// Wraps the batch buffer (or nothing) so every token write is counted.
struct CountedSink<'a> {
    inner: Option<&'a mut std::io::Cursor<Vec<u8>>>,
    written: u64,
}

impl std::io::Write for CountedSink<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = match self.inner.as_mut() {
            Some(inner) => inner.write(buf)?,
            None => buf.len(),
        };
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.inner.as_mut() {
            Some(inner) => inner.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Streams built on this thread; the seam the one-stream tests read.
    static TOKEN_STREAMS_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl TokenStream {
    /// Builds the run's stream for the session's codec, or `None` without `-z`.
    ///
    /// This is the engine's only constructor of a compressed-token encoder.
    /// `protocol` is the version the tokens are framed for: zlib below 31
    /// repeats upstream's history-feed offset bug (token.c:483-484).
    fn from_options(
        options: &LocalCopyOptions,
        algorithm: CompressionAlgorithm,
        protocol: u32,
    ) -> Option<Self> {
        if !options.compress_enabled() {
            return None;
        }
        let encoder = match algorithm {
            CompressionAlgorithm::Zlib => {
                protocol::wire::CompressedTokenEncoder::new(options.compression_level(), protocol)
            }
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => protocol::wire::CompressedTokenEncoder::new_zstd(
                zstd_level(options.compression_level()),
                options.compression_threads(),
            )
            .ok()?,
            #[cfg(feature = "lz4")]
            CompressionAlgorithm::Lz4 => protocol::wire::CompressedTokenEncoder::new_lz4(),
            // A codec this build lacks (feature unification through another
            // dependent) has no stream; the CLI refuses it before the copy.
            #[allow(unreachable_patterns)]
            _ => return None,
        };
        #[cfg(test)]
        TOKEN_STREAMS_BUILT.with(|built| built.set(built.get() + 1));
        Some(Self {
            encoder,
            file_bytes: 0,
        })
    }

    /// Streams built on the calling thread so far.
    #[cfg(test)]
    pub(crate) fn built_on_this_thread() -> usize {
        TOKEN_STREAMS_BUILT.with(std::cell::Cell::get)
    }

    /// Starts a file's token stream.
    ///
    /// upstream: token.c:390 `deflateReset()` at the first token of each file.
    fn reset(&mut self) {
        self.encoder.reset();
        self.file_bytes = 0;
    }

    /// Bytes written for the current file so far.
    const fn file_bytes(&self) -> u64 {
        self.file_bytes
    }

    /// Writes `data` as literal tokens and returns the bytes produced.
    fn literal(
        &mut self,
        sink: Option<&mut std::io::Cursor<Vec<u8>>>,
        data: &[u8],
    ) -> std::io::Result<u64> {
        let mut out = CountedSink { inner: sink, written: 0 };
        self.encoder.send_literal(&mut out, data)?;
        Ok(self.count(out.written))
    }

    /// Writes a block-match token and returns the bytes produced. `matched`
    /// primes the zlib history the way token.c:475-510 does.
    fn block_match(
        &mut self,
        sink: Option<&mut std::io::Cursor<Vec<u8>>>,
        block_index: u32,
        matched: Option<&[u8]>,
    ) -> std::io::Result<u64> {
        let mut out = CountedSink { inner: sink, written: 0 };
        self.encoder.send_block_match(&mut out, block_index)?;
        if let Some(data) = matched {
            self.encoder.see_token(data)?;
        }
        Ok(self.count(out.written))
    }

    /// Ends the file's stream and returns the bytes produced.
    ///
    /// upstream: token.c:468-474 drains the stream and writes `END_FLAG`.
    fn finish(&mut self, sink: Option<&mut std::io::Cursor<Vec<u8>>>) -> std::io::Result<u64> {
        let mut out = CountedSink { inner: sink, written: 0 };
        self.encoder.finish(&mut out)?;
        Ok(self.count(out.written))
    }

    fn count(&mut self, written: u64) -> u64 {
        self.file_bytes += written;
        written
    }
}

/// The zstd level a `CompressionLevel` stands for.
#[cfg(feature = "zstd")]
fn zstd_level(level: CompressionLevel) -> i32 {
    match level {
        CompressionLevel::None | CompressionLevel::Default => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Best => 19,
        CompressionLevel::Precise(v) => i32::from(v.get()),
        CompressionLevel::PreciseSigned(v) => v,
    }
}
