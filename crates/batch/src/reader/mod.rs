//! Batch file reader for replaying transfers.
//!
//! This module provides `BatchReader` for opening and reading batch files
//! created by `BatchWriter`. The reader is split into focused submodules:
//!
//! - Core struct, construction, raw I/O, and accessors (this file)
//! - [`flist`] - file list deserialization (protocol and local formats)
//! - [`delta`] - delta token and operation reading
//! - [`stats`] - transfer statistics reading

mod delta;
mod flist;
mod stats;

#[cfg(test)]
mod tests;

use crate::BatchConfig;
use crate::error::{BatchError, BatchResult};
use crate::format::{BatchFlags, BatchHeader};
use protocol::codec::NdxCodecEnum;
use protocol::flist::FileListReader;
use std::fs::File;
use std::io::{self, BufReader, Read};

/// Reader for batch mode operations.
///
/// Reads and replays a previously recorded batch file, applying the
/// same changes to a different destination.
pub struct BatchReader {
    /// Configuration for this batch operation.
    config: BatchConfig,
    /// Reader for the binary batch file.
    batch_file: Option<BufReader<File>>,
    /// The header read from the file.
    header: Option<BatchHeader>,
    /// Accumulated I/O error code from the file list sender.
    ///
    /// Populated after [`read_protocol_flist`](Self::read_protocol_flist) returns.
    /// Upstream `flist.c:recv_file_list()` accumulates `io_error |= err` when
    /// the sender reports errors during file list generation.
    io_error: i32,
    /// NDX codec initialized during flist reading and reused for delta replay.
    ///
    /// With INC_RECURSE, the NDX codec is first used to read incremental flist
    /// segment headers, then continues with the same state for delta NDX values.
    /// upstream: sender and receiver share one continuous `read_ndx()` state.
    ndx_codec: Option<NdxCodecEnum>,
    /// File list reader preserved for incremental sub-list reading.
    ///
    /// With INC_RECURSE, the batch stream interleaves flist sub-list segments
    /// with delta operations. The flist reader must persist across calls so
    /// sub-lists can be decoded on-the-fly during delta replay.
    /// upstream: flist.c state persists across `recv_additional_file_list()` calls.
    flist_reader: Option<FileListReader>,
    /// Next NDX start for incremental flist segments.
    ///
    /// Tracks the starting index for the next sub-list segment, incremented
    /// as entries are appended during INC_RECURSE replay.
    flist_next_ndx_start: i32,
}

impl BatchReader {
    /// Create a new batch reader.
    pub fn new(config: BatchConfig) -> BatchResult<Self> {
        let batch_path = config.batch_file_path();
        let file = File::open(batch_path).map_err(|e| {
            BatchError::Io(io::Error::new(
                e.kind(),
                format!(
                    "Failed to open batch file '{}': {}",
                    batch_path.display(),
                    e
                ),
            ))
        })?;

        Ok(Self {
            config,
            batch_file: Some(BufReader::new(file)),
            header: None,
            io_error: 0,
            ndx_codec: None,
            flist_reader: None,
            flist_next_ndx_start: 0,
        })
    }

    /// Read and validate the batch header.
    ///
    /// Returns the stream flags that were recorded in the batch.
    pub fn read_header(&mut self) -> BatchResult<BatchFlags> {
        if self.header.is_some() {
            return Err(BatchError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Batch header already read",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            let header = BatchHeader::read_from(reader).map_err(|e| {
                BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read batch header: {e}"),
                ))
            })?;

            // Adopt protocol version, compat flags, and checksum seed from
            // the batch header. upstream: compat.c:setup_protocol() reads
            // these values from the batch fd and uses them directly, so the
            // reader must use whatever the batch was written with.
            self.config.protocol_version = header.protocol_version;
            self.config.compat_flags = header.compat_flags;
            self.config.checksum_seed = header.checksum_seed;

            let flags = header.stream_flags;
            self.header = Some(header);
            Ok(flags)
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Read data from the batch file.
    ///
    /// This reads the next chunk of data from the batch file, which
    /// could be file list entries or delta operations.
    pub fn read_data(&mut self, buf: &mut [u8]) -> BatchResult<usize> {
        if self.header.is_none() {
            return Err(BatchError::Io(io::Error::other(
                "Must read header before data",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            reader.read(buf).map_err(|e| {
                BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read batch data: {e}"),
                ))
            })
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Read exact amount of data from the batch file.
    pub fn read_exact(&mut self, buf: &mut [u8]) -> BatchResult<()> {
        if self.header.is_none() {
            return Err(BatchError::Io(io::Error::other(
                "Must read header before data",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            reader.read_exact(buf).map_err(|e| {
                BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read exact batch data: {e}"),
                ))
            })
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Get the header that was read from the batch file.
    pub const fn header(&self) -> Option<&BatchHeader> {
        self.header.as_ref()
    }

    /// Get a reference to the batch configuration.
    pub const fn config(&self) -> &BatchConfig {
        &self.config
    }

    /// Returns the accumulated I/O error code from the file list sender.
    ///
    /// This is populated after [`read_protocol_flist`](Self::read_protocol_flist)
    /// returns. A non-zero value indicates the sender encountered errors during
    /// file list generation, but the transfer can proceed with the files that
    /// were successfully listed.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:recv_file_list()` accumulates `io_error |= err`
    pub const fn io_error(&self) -> i32 {
        self.io_error
    }

    /// Returns a mutable reference to the underlying batch file reader.
    ///
    /// This is useful when callers need direct access to the stream, for
    /// example to pass it to protocol-level decoders like `read_delta`.
    ///
    /// Returns `None` if the batch file has not been opened or has been closed.
    pub fn inner_reader(&mut self) -> Option<&mut BufReader<File>> {
        self.batch_file.as_mut()
    }

    /// Returns the NDX codec initialized during flist reading.
    ///
    /// The codec carries state from reading incremental flist segment NDX
    /// values (INC_RECURSE). The delta replay loop must continue with this
    /// same codec to correctly decode subsequent NDX values.
    ///
    /// Returns `None` if `read_protocol_flist()` has not been called yet.
    pub fn take_ndx_codec(&mut self) -> Option<NdxCodecEnum> {
        self.ndx_codec.take()
    }
}
