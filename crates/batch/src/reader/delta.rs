//! Delta operation reading for batch files.
//!
//! Provides methods for reading delta tokens from the batch stream in both
//! plain and compressed formats. Plain format is used when the batch was
//! written without compression; compressed format (zlib DEFLATED_DATA
//! headers) is used when the batch stream flags include `do_compression`.
//!
//! # Upstream Reference
//!
//! - `token.c:simple_recv_token()` - plain 4-byte LE token format
//! - `token.c:recv_deflated_token()` - compressed token format with
//!   DEFLATED_DATA headers and run-length encoded block references

use crate::error::{BatchError, BatchResult};
use protocol::wire::{CompressedToken, CompressedTokenDecoder};
use std::io::{self, Read};

use super::BatchReader;

impl BatchReader {
    /// Read delta operations for a single file using plain (uncompressed) tokens.
    ///
    /// Reads the upstream `simple_recv_token` format: each token is a 4-byte
    /// little-endian i32 where positive = literal length, negative = block
    /// reference, and zero = end-of-file.
    ///
    /// # Upstream Reference
    ///
    /// - `token.c:simple_send_token()` / `simple_recv_token()`
    pub fn read_file_delta_tokens(&mut self) -> BatchResult<Vec<protocol::wire::DeltaOp>> {
        if self.header.is_none() {
            return Err(BatchError::Io(io::Error::other(
                "Must read header before delta operations",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            let mut ops = Vec::new();
            loop {
                let token = protocol::wire::delta::read_token(reader).map_err(|e| {
                    BatchError::Io(io::Error::new(
                        e.kind(),
                        format!("Failed to read delta token: {e}"),
                    ))
                })?;

                match token {
                    None => break, // End marker (token value 0)
                    Some(n) if n > 0 => {
                        // Literal data: n bytes follow
                        let mut data = vec![0u8; n as usize];
                        reader.read_exact(&mut data).map_err(|e| {
                            BatchError::Io(io::Error::new(
                                e.kind(),
                                format!("Failed to read literal data ({n} bytes): {e}"),
                            ))
                        })?;
                        ops.push(protocol::wire::DeltaOp::Literal(data));
                    }
                    Some(n) => {
                        // Block match: block_index = -(n+1)
                        let block_index = (-(n + 1)) as u32;
                        ops.push(protocol::wire::DeltaOp::Copy {
                            block_index,
                            length: 0, // Length determined by block size at replay time
                        });
                    }
                }
            }
            Ok(ops)
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Read delta operations for a single file using compressed tokens.
    ///
    /// When the batch stream flags have `do_compression` set, the delta
    /// token data is stored using upstream's compressed token wire format
    /// (DEFLATED_DATA headers with zlib-compressed literal data and
    /// run-length encoded block references). This method uses a
    /// `CompressedTokenDecoder` to inflate the tokens.
    ///
    /// The `decoder` must be reset before each file by the caller
    /// (mirrors upstream `token.c:recv_deflated_token()` r_init state).
    ///
    /// # Upstream Reference
    ///
    /// - `token.c:recv_deflated_token()` - compressed token format
    /// - `io.c:read_buf()` - batch monitor tees compressed bytes to batch_fd
    pub fn read_compressed_delta_tokens(
        &mut self,
        decoder: &mut CompressedTokenDecoder,
    ) -> BatchResult<Vec<protocol::wire::DeltaOp>> {
        if self.header.is_none() {
            return Err(BatchError::Io(io::Error::other(
                "Must read header before delta operations",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            let mut ops = Vec::new();
            loop {
                let token = decoder.recv_token(reader).map_err(|e| {
                    BatchError::Io(io::Error::new(
                        e.kind(),
                        format!("Failed to read compressed delta token: {e}"),
                    ))
                })?;

                match token {
                    CompressedToken::End => break,
                    CompressedToken::Literal(data) => {
                        ops.push(protocol::wire::DeltaOp::Literal(data));
                    }
                    CompressedToken::BlockMatch(block_index) => {
                        ops.push(protocol::wire::DeltaOp::Copy {
                            block_index,
                            length: 0,
                        });
                    }
                }
            }
            Ok(ops)
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Read all delta operations from the batch file using internal format.
    ///
    /// This reads delta operations using the internal opcode-based format
    /// (count prefix + individual operations). For batch files written with
    /// the token format, use [`read_file_delta_tokens`](Self::read_file_delta_tokens)
    /// instead.
    pub fn read_all_delta_ops(&mut self) -> BatchResult<Vec<protocol::wire::DeltaOp>> {
        if self.header.is_none() {
            return Err(BatchError::Io(io::Error::other(
                "Must read header before delta operations",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            protocol::wire::delta::read_delta(reader).map_err(|e| {
                BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read delta operations: {e}"),
                ))
            })
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }
}
