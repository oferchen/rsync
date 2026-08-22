//! Batch file reader for replaying transfers.
//!
//! This module provides `BatchReader` for opening and reading batch files
//! created by `BatchWriter`. The reader is split into focused submodules:
//!
//! - Core struct, construction, raw I/O, and accessors (this file)
//! - `flist` - file list deserialization (protocol and local formats)
//! - `delta` - delta token and operation reading
//! - `stats` - transfer statistics reading

mod delta;
mod flist;

pub use flist::default_flist_csum_len;
mod stats;

#[cfg(test)]
mod tests;

use crate::BatchConfig;
use crate::error::{BatchError, BatchResult};
use crate::format::{BatchFlags, BatchHeader};
use protocol::codec::NdxCodecEnum;
use protocol::flist::FileListReader;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

/// Input source for a batch reader.
///
/// A `--read-batch` argument of `-` selects standard input, mirroring upstream
/// `batch.c:open_batch_files` (`strcmp(batch_name, "-") == 0` opens
/// `STDIN_FILENO`). Upstream reads the batch fd purely sequentially, but stdin
/// is not seekable, so the `Stdin` variant buffers the stream into memory:
/// oc-rsync's compression-codec auto-detection (see [`crate::replay`]) peeks
/// ahead and restores the position via `Seek`, which the in-memory `Cursor`
/// supports while a raw stdin fd would not. Both variants are therefore
/// `Read + Seek`.
/// upstream: batch.c:open_batch_files
pub(crate) enum BatchSource {
    /// A batch file opened from a filesystem path.
    File(File),
    /// Standard input buffered into memory (the `--read-batch=-` path).
    Stdin(Cursor<Vec<u8>>),
}

impl Read for BatchSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(f) => f.read(buf),
            Self::Stdin(c) => c.read(buf),
        }
    }
}

impl Seek for BatchSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Self::File(f) => f.seek(pos),
            Self::Stdin(c) => c.seek(pos),
        }
    }
}

/// Buffered reader over a [`BatchSource`], the concrete stream type threaded
/// through header, file-list, delta, and replay decoding.
pub(crate) type BatchStream = BufReader<BatchSource>;

/// Reader for batch mode operations.
///
/// Reads and replays a previously recorded batch file, applying the
/// same changes to a different destination.
pub struct BatchReader {
    /// Configuration for this batch operation.
    config: BatchConfig,
    /// Reader for the binary batch stream (file or buffered stdin).
    batch_file: Option<BatchStream>,
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
    ///
    /// A batch path of `-` reads from standard input instead of opening a
    /// file, mirroring upstream `batch.c:open_batch_files`.
    pub fn new(config: BatchConfig) -> BatchResult<Self> {
        let source = Self::open_source(config.batch_file_path())?;
        Ok(Self::from_source(config, source))
    }

    /// Open the batch input source described by `path`.
    ///
    /// A path of `-` selects standard input, matching upstream
    /// `batch.c:open_batch_files` (`strcmp(batch_name, "-") == 0` uses
    /// `STDIN_FILENO`). stdin is buffered into memory because oc-rsync's
    /// codec auto-detection seeks the stream; see [`BatchSource`].
    /// upstream: batch.c:open_batch_files
    fn open_source(path: &Path) -> BatchResult<BatchSource> {
        if path.as_os_str() == "-" {
            let mut buf = Vec::new();
            io::stdin().lock().read_to_end(&mut buf).map_err(|e| {
                BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read batch file from stdin: {e}"),
                ))
            })?;
            Ok(BatchSource::Stdin(Cursor::new(buf)))
        } else {
            // upstream: batch.c:267 opens the read-batch file through
            // `open_no_attacker_symlinks()`. The path is operator-supplied and
            // may transit attacker-writable parents, so a planted symlink would
            // feed the replay a batch stream the operator never named.
            let file = crate::operator_file::open_read(path).map_err(|e| {
                BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to open batch file '{}': {}", path.display(), e),
                ))
            })?;
            Ok(BatchSource::File(file))
        }
    }

    /// Construct a reader over an in-memory batch image, exactly as the
    /// `--read-batch=-` path buffers standard input. Exercises the stdin
    /// source deterministically without touching the process-wide stdin fd.
    #[cfg(test)]
    pub(crate) fn from_stdin_bytes(config: BatchConfig, bytes: Vec<u8>) -> Self {
        Self::from_source(config, BatchSource::Stdin(Cursor::new(bytes)))
    }

    /// Build a reader over an already-opened [`BatchSource`].
    fn from_source(config: BatchConfig, source: BatchSource) -> Self {
        Self {
            config,
            batch_file: Some(BufReader::new(source)),
            header: None,
            io_error: 0,
            ndx_codec: None,
            flist_reader: None,
            flist_next_ndx_start: 0,
        }
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

            // A batch written by a newer rsync than this build supports cannot
            // be replayed correctly, so abort rather than adopt an unsupported
            // protocol. The comparison uses this build's maximum supported
            // protocol, matching upstream's `remote_protocol > protocol_version`
            // (for --read-batch `protocol_version` is the client's compiled max).
            // upstream: compat.c:609-613 setup_protocol()
            let max_supported = i32::from(protocol::SUPPORTED_PROTOCOL_BOUNDS.1);
            if header.protocol_version > max_supported {
                // upstream: compat.c:610-611 rprintf(FERROR, "The protocol
                // version in the batch file is too new (%d > %d).\n", ...) -
                // match the diagnostic text, including the trailing period.
                return Err(BatchError::Io(protocol::protocol_violation(format!(
                    "The protocol version in the batch file is too new ({} > {}).",
                    header.protocol_version, max_supported
                ))));
            }

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
    pub(crate) fn inner_reader(&mut self) -> Option<&mut BatchStream> {
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
