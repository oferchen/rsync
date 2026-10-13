//! Zstd token decoder for the compressed token wire format.
//!
//! Implements the zstd-specific decoder used by CPRES_ZSTD mode. Maintains a
//! single persistent decompression context across the entire transfer session,
//! processing one DEFLATED_DATA block at a time.
//!
//! - upstream: token.c:recv_zstd_token() lines 780-870

use std::io::{self, Read};

use zstd::stream::raw::{Decoder as ZstdRawDecoder, Operation};

#[cfg(feature = "tokio-transfer")]
use super::super::step::drive_async;
use super::super::step::{DeflateSink, TokenDecodeCore, drive_sync};
use super::super::{CompressedToken, MAX_DATA_COUNT};

/// Zstd decoder state for receiving compressed tokens.
///
/// Maintains a single persistent `ZSTD_DCtx` across the **entire transfer
/// session** (all files). Upstream rsync never resets the decompression
/// context between files - the session is one continuous zstd stream.
///
/// Between files, only the token index (rx_token) resets to 0, matching
/// upstream token.c:862-865 (r_init state just resets rx_token).
///
/// The decoder processes one DEFLATED_DATA block at a time, matching
/// upstream's state machine (r_idle -> r_inflating -> r_idle). Each step
/// decompresses into a fixed `out_buffer_size` buffer and that step's output is
/// emitted as one literal before the next step runs, so memory never depends
/// on the compression ratio. When all compressed input is consumed and the
/// output buffer is not full, the decoder returns to idle to read the next
/// wire flag.
///
/// upstream: token.c:recv_zstd_token() - DCtx created once (line 844),
/// never reset between files (line 862-865 only resets rx_token)
pub(in crate::wire::compressed_token) struct ZstdTokenDecoder {
    /// Shared sans-io decode state machine (framing, run index, output chunking).
    core: TokenDecodeCore,
    /// Algorithm-specific decompression half.
    deflate: ZstdDeflate,
}

/// Zstd decompression sink: the algorithm-specific half of the sans-io decoder.
///
/// Owns the persistent `ZSTD_DCtx` (never reset between files), the scratch
/// output buffer, and the single-block compressed input. The shared
/// [`TokenDecodeCore`] drives all wire framing and delegates one DEFLATED_DATA
/// block at a time here.
///
/// upstream: token.c:recv_zstd_token() - DCtx created once (line 844),
/// never reset between files (line 862-865 only resets rx_token)
struct ZstdDeflate {
    /// Persistent zstd decompression context.
    decoder: ZstdRawDecoder<'static>,
    /// Scratch buffer for decompression output.
    /// upstream: out_buffer_size = ZSTD_DStreamOutSize() * 2
    output_buf: Vec<u8>,
    /// Reusable buffer for compressed input data read from the wire.
    compressed_input_buf: Vec<u8>,
    /// Bytes of `compressed_input_buf` already fed to the decoder; carried
    /// across steps like upstream's `zstd_in_buff.pos`.
    input_pos: usize,
}

impl DeflateSink for ZstdDeflate {
    fn accumulates(&self) -> bool {
        false
    }

    fn begin_block(&mut self, payload: &[u8]) {
        // upstream: token.c:875-878 - read_buf into cbuf, zstd_in_buff.pos = 0
        self.compressed_input_buf.clear();
        self.compressed_input_buf.extend_from_slice(payload);
        self.input_pos = 0;
    }

    fn push_block(&mut self, payload: &[u8]) -> io::Result<()> {
        // Never called: zstd does not accumulate consecutive blocks.
        self.compressed_input_buf.extend_from_slice(payload);
        Ok(())
    }

    fn decompress_step(&mut self, output: &mut Vec<u8>) -> io::Result<bool> {
        // upstream: token.c:895-919 r_inflating - one ZSTD_decompressStream
        // into the fixed out_buffer_size dbuf; its output is returned before
        // any more input is decompressed.
        let mut in_buf =
            zstd::stream::raw::InBuffer::around(&self.compressed_input_buf[self.input_pos..]);
        let mut out_buf = zstd::stream::raw::OutBuffer::around(self.output_buf.as_mut_slice());
        self.decoder.run(&mut in_buf, &mut out_buf)?;
        self.input_pos += in_buf.pos();
        let produced = out_buf.pos();
        output.extend_from_slice(&self.output_buf[..produced]);
        // upstream: token.c:911-912 - input consumed and output buffer not full
        // means the block is finished; read the next flag.
        Ok(self.input_pos == self.compressed_input_buf.len() && produced < self.output_buf.len())
    }
}
impl ZstdTokenDecoder {
    /// Creates a zstd decoder with a fresh persistent decompression context.
    ///
    /// The `ZSTD_DCtx` lives for the whole transfer session and is never reset
    /// between files; see the type-level documentation.
    pub(in crate::wire::compressed_token) fn new() -> io::Result<Self> {
        let decoder = ZstdRawDecoder::new()?;
        // upstream: token.c line 851 - out_buffer_size = ZSTD_DStreamOutSize() * 2
        let out_size = zstd::zstd_safe::DCtx::out_size() * 2;
        Ok(Self {
            core: TokenDecodeCore::new(false),
            deflate: ZstdDeflate {
                decoder,
                output_buf: vec![0u8; out_size],
                compressed_input_buf: Vec::with_capacity(MAX_DATA_COUNT),
                input_pos: 0,
            },
        })
    }

    /// Returns whether the decoder has received its first token.
    pub(in crate::wire::compressed_token) fn initialized(&self) -> bool {
        self.core.initialized
    }

    /// Resets decoder state for a new file.
    ///
    /// Only resets the token index and buffering state. The zstd decompression
    /// context is NOT reinitialized - upstream rsync uses a single continuous
    /// stream across all files in the session.
    ///
    /// upstream: token.c:862-865 - r_init only resets rx_token to 0
    pub(in crate::wire::compressed_token) fn reset(&mut self) {
        self.core.reset();
        self.deflate.compressed_input_buf.clear();
        self.deflate.input_pos = 0;
        // Keep initialized=true - the DCtx is still valid from the same stream
    }

    /// Receives the next token from a zstd-compressed stream.
    ///
    /// A thin blocking driver over the shared sans-io decode state machine.
    ///
    /// upstream: token.c:recv_zstd_token() lines 861-920
    pub(in crate::wire::compressed_token) fn recv_token<R: Read>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<CompressedToken> {
        drive_sync(&mut self.core, &mut self.deflate, reader)
    }

    /// Async counterpart to [`recv_token`](Self::recv_token), backed by the same
    /// sans-io state machine.
    #[cfg(feature = "tokio-transfer")]
    pub(in crate::wire::compressed_token) async fn recv_token_async<R>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<CompressedToken>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        drive_async(&mut self.core, &mut self.deflate, reader).await
    }

    /// Noop for zstd - no dictionary synchronization needed.
    pub(in crate::wire::compressed_token) fn see_token(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }
}
