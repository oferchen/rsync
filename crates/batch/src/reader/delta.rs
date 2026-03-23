//! Delta operation reading for batch files.
//!
//! Provides methods for reading delta tokens and operations from the batch
//! stream. Two formats are supported: the upstream token format used in
//! protocol-compatible batch files, and an internal opcode-based format.

use crate::error::{BatchError, BatchResult};
use std::io::{self, Read};

use super::BatchReader;

impl BatchReader {
    /// Read delta operations for a single file from the batch stream.
    ///
    /// Reads the upstream token-format delta stream until the end marker
    /// (write_int(0)) is encountered. Each call consumes exactly one file's
    /// worth of delta data.
    ///
    /// Returns a vector of `DeltaOp` that can be applied to reconstruct the file.
    ///
    /// # Upstream Reference
    ///
    /// - `token.c:simple_send_token()` - token format:
    ///   positive i32 = literal length, negative i32 = block match, 0 = end
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
