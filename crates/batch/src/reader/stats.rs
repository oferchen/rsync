//! Transfer statistics reading for batch files.
//!
//! Provides the method for reading `BatchStats` from the end of a batch file,
//! matching upstream rsync's `main.c` stats serialization.

use crate::error::{BatchError, BatchResult};
use crate::format::BatchStats;
use std::io;

use super::BatchReader;

impl BatchReader {
    /// Read transfer statistics from the batch file.
    ///
    /// Upstream rsync writes these statistics at the end of the batch file.
    /// Call this method after all protocol stream data has been consumed.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:370-383`: stats read with `read_varlong30(f, 3)`
    pub fn read_stats(&mut self) -> BatchResult<BatchStats> {
        if self.header.is_none() {
            return Err(BatchError::Io(io::Error::other(
                "Must read header before stats",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            BatchStats::read_from(reader, self.config.protocol_version).map_err(|e| {
                BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read batch stats: {e}"),
                ))
            })
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }
}
