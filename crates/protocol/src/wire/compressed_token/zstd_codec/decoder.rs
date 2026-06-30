//! Zstd token decoder for the compressed token wire format.
//!
//! Implements the zstd-specific decoder used by CPRES_ZSTD mode. Maintains a
//! single persistent decompression context across the entire transfer session,
//! processing one DEFLATED_DATA block at a time.
//!
//! - upstream: token.c:recv_zstd_token() lines 780-870

use std::io::{self, Read};

use zstd::stream::raw::{Decoder as ZstdRawDecoder, Operation};

use super::super::{
    CHUNK_SIZE, CompressedToken, DEFLATED_DATA, END_FLAG, MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL,
    read_deflated_data_length,
};

/// Zstd decoder state for receiving compressed tokens.
///
/// Maintains a single persistent `ZSTD_DCtx` across the **entire transfer
/// session** (all files). Upstream rsync never resets the decompression
/// context between files - the session is one continuous zstd stream.
///
/// Between files, only the token index (rx_token) resets to 0, matching
/// upstream token.c:807-810 (r_init state just resets rx_token).
///
/// The decoder processes one DEFLATED_DATA block at a time, matching
/// upstream's state machine (r_idle -> r_inflating -> r_idle). When all
/// compressed input is consumed and the output buffer is not full, the
/// decoder returns to idle to read the next wire flag.
///
/// upstream: token.c:recv_zstd_token() - DCtx created once (line 789),
/// never reset between files (line 807-810 only resets rx_token)
pub(in crate::wire::compressed_token) struct ZstdTokenDecoder {
    /// Persistent zstd decompression context.
    decoder: ZstdRawDecoder<'static>,
    /// Buffer for decompressed output.
    decompress_buf: Vec<u8>,
    /// Current position in decompress buffer.
    decompress_pos: usize,
    /// Scratch buffer for decompression output.
    /// upstream: out_buffer_size = ZSTD_DStreamOutSize() * 2
    output_buf: Vec<u8>,
    /// Reusable buffer for compressed input data read from the wire.
    compressed_input_buf: Vec<u8>,
    /// Current token index.
    rx_token: i32,
    /// Remaining tokens in current run.
    rx_run: i32,
    pub(in crate::wire::compressed_token) initialized: bool,
}

impl ZstdTokenDecoder {
    pub(in crate::wire::compressed_token) fn new() -> io::Result<Self> {
        let decoder = ZstdRawDecoder::new()?;
        // upstream: token.c line 795 - out_buffer_size = ZSTD_DStreamOutSize() * 2
        let out_size = zstd::zstd_safe::DCtx::out_size() * 2;
        Ok(Self {
            decoder,
            decompress_buf: Vec::new(),
            decompress_pos: 0,
            output_buf: vec![0u8; out_size],
            compressed_input_buf: Vec::with_capacity(MAX_DATA_COUNT),
            rx_token: 0,
            rx_run: 0,
            initialized: false,
        })
    }

    /// Resets decoder state for a new file.
    ///
    /// Only resets the token index and buffering state. The zstd decompression
    /// context is NOT reinitialized - upstream rsync uses a single continuous
    /// stream across all files in the session.
    ///
    /// upstream: token.c:807-810 - r_init only resets rx_token to 0
    pub(in crate::wire::compressed_token) fn reset(&mut self) {
        self.decompress_buf.clear();
        self.decompress_pos = 0;
        self.compressed_input_buf.clear();
        self.rx_token = 0;
        self.rx_run = 0;
        // Keep initialized=true - the DCtx is still valid from the same stream
    }

    /// Receives the next token from a zstd-compressed stream.
    ///
    /// Mirrors upstream's state machine in recv_zstd_token() (token.c lines
    /// 805-877). Processes one DEFLATED_DATA block at a time: reads compressed
    /// data, decompresses via `ZSTD_decompressStream`, and returns available
    /// output. If all input is consumed and the output buffer is not full,
    /// transitions back to idle to read the next wire flag.
    ///
    /// upstream: token.c:recv_zstd_token() lines 805-877
    pub(in crate::wire::compressed_token) fn recv_token<R: Read>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<CompressedToken> {
        if !self.initialized {
            self.initialized = true;
        }

        if self.decompress_pos < self.decompress_buf.len() {
            let remaining = &self.decompress_buf[self.decompress_pos..];
            let chunk_len = remaining.len().min(CHUNK_SIZE);
            let data = remaining[..chunk_len].to_vec();
            self.decompress_pos += chunk_len;
            return Ok(CompressedToken::Literal(data));
        }

        // Emit pending run tokens
        // upstream: token.c lines 871-876 (r_running state)
        // upstream: token.c defence-in-depth (3.4.3) - checked increment
        // prevents rx_token from wrapping past i32::MAX during long runs.
        if self.rx_run > 0 {
            self.rx_run -= 1;
            self.rx_token = self.rx_token.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "token index overflow in compressed stream run",
                )
            })?;
            return Ok(CompressedToken::BlockMatch(self.rx_token as u32));
        }

        // Read next flag byte
        // upstream: token.c lines 812-813 (r_idle state)
        let mut flag_buf = [0u8; 1];
        reader.read_exact(&mut flag_buf)?;
        let flag = flag_buf[0];

        if (flag & 0xC0) == DEFLATED_DATA {
            // upstream: token.c lines 814-822
            let len = read_deflated_data_length(reader, flag)?;
            self.compressed_input_buf.clear();
            self.compressed_input_buf.resize(len, 0);
            reader.read_exact(&mut self.compressed_input_buf)?;

            // Decompress the block
            // upstream: token.c lines 846-863 (r_inflating state)
            self.decompress_buf.clear();
            let mut input_pos = 0;

            while input_pos < self.compressed_input_buf.len() {
                let mut in_buf =
                    zstd::stream::raw::InBuffer::around(&self.compressed_input_buf[input_pos..]);
                let mut out_buf = zstd::stream::raw::OutBuffer::around(&mut self.output_buf);

                self.decoder.run(&mut in_buf, &mut out_buf)?;
                input_pos += in_buf.pos();
                let produced = out_buf.pos();

                if produced > 0 {
                    self.decompress_buf
                        .extend_from_slice(&self.output_buf[..produced]);
                }

                // upstream: token.c lines 862-863
                // If input is fully consumed and output buffer not full,
                // transition back to idle (read next flag).
                if input_pos >= self.compressed_input_buf.len() && produced < self.output_buf.len()
                {
                    break;
                }
            }

            // Drain any remaining buffered output from the zstd decoder.
            // After all compressed input is consumed, the decoder may still
            // hold decompressed data internally when the output buffer was
            // full on the last iteration. Flush by feeding empty input.
            // upstream: token.c lines 846-863 - inflate loop continues until
            // output buffer is not full, indicating decoder is drained.
            loop {
                let mut in_buf = zstd::stream::raw::InBuffer::around(&[]);
                let mut out_buf = zstd::stream::raw::OutBuffer::around(&mut self.output_buf);
                self.decoder.run(&mut in_buf, &mut out_buf)?;
                let produced = out_buf.pos();
                if produced == 0 {
                    break;
                }
                self.decompress_buf
                    .extend_from_slice(&self.output_buf[..produced]);
            }

            self.decompress_pos = 0;

            if !self.decompress_buf.is_empty() {
                let chunk_len = self.decompress_buf.len().min(CHUNK_SIZE);
                let data = self.decompress_buf[..chunk_len].to_vec();
                self.decompress_pos = chunk_len;
                return Ok(CompressedToken::Literal(data));
            }

            // No output produced - read next flag
            return self.recv_token(reader);
        }

        if flag == END_FLAG {
            // upstream: token.c lines 825-828
            return Ok(CompressedToken::End);
        }

        // Token parsing - same encoding for all algorithms
        // upstream: token.c lines 831-841
        if flag & TOKEN_REL != 0 {
            let rel = (flag & 0x3F) as i32;
            // upstream: token.c defence-in-depth (3.4.3) - checked addition
            // prevents rx_token from wrapping past i32::MAX via repeated
            // TOKEN_REL accumulation from a malicious sender.
            self.rx_token = self.rx_token.checked_add(rel).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "token index overflow in compressed stream",
                )
            })?;

            if (flag >> 6) & 1 != 0 {
                let mut run_buf = [0u8; 2];
                reader.read_exact(&mut run_buf)?;
                self.rx_run = u16::from_le_bytes(run_buf) as i32;
            }

            Ok(CompressedToken::BlockMatch(self.rx_token as u32))
        } else if flag & 0xE0 == TOKEN_LONG {
            let mut buf = [0u8; 4];
            reader.read_exact(&mut buf)?;
            self.rx_token = i32::from_le_bytes(buf);
            // upstream: token.c:1013-1016 (3.4.2) - reject negative absolute
            // token to prevent block-index wrap into the valid range.
            if self.rx_token < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid token number in compressed stream",
                ));
            }

            if flag & 1 != 0 {
                let mut run_buf = [0u8; 2];
                reader.read_exact(&mut run_buf)?;
                self.rx_run = u16::from_le_bytes(run_buf) as i32;
            }

            Ok(CompressedToken::BlockMatch(self.rx_token as u32))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid compressed token flag: 0x{flag:02X}"),
            ))
        }
    }

    /// Noop for zstd - no dictionary synchronization needed.
    pub(in crate::wire::compressed_token) fn see_token(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }
}
