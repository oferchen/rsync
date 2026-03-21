//! crates/batch/src/reader.rs
//!
//! Batch file reader for replaying transfers.

use crate::BatchConfig;
use crate::error::{BatchError, BatchResult};
use crate::format::{BatchFlags, BatchHeader, BatchStats, FileEntry};
use std::fs::File;
use std::io::{self, BufReader, Read};

use protocol::CompatibilityFlags;
use protocol::ProtocolVersion;
use protocol::flist::FileListReader;

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
}

impl BatchReader {
    /// Create a new batch reader.
    pub fn new(config: BatchConfig) -> BatchResult<Self> {
        // Open the batch file
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

    /// Read all delta operations from the batch file.
    ///
    /// This reads delta operations until EOF is reached.
    /// Suitable for single-file batches or when processing one file at a time.
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

    /// Read a file entry from the batch file using local encoding.
    ///
    /// Returns the next file list entry, or None if end of file list is reached.
    ///
    /// **Note:** This uses a local serialization format that is not compatible
    /// with upstream rsync's batch files. For protocol-compatible batch files,
    /// use [`read_protocol_flist`](Self::read_protocol_flist) instead.
    pub fn read_file_entry(&mut self) -> BatchResult<Option<FileEntry>> {
        if self.header.is_none() {
            return Err(BatchError::Io(io::Error::other(
                "Must read header before file entries",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            // Try to read the next file entry
            // If we hit EOF or an empty path, we've reached the end of the file list
            match FileEntry::read_from(reader) {
                Ok(entry) => {
                    if entry.path.is_empty() {
                        Ok(None) // End of file list marker
                    } else {
                        Ok(Some(entry))
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
                Err(e) => Err(BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read file entry: {e}"),
                ))),
            }
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Read the entire file list from the batch file using the protocol flist
    /// decoder.
    ///
    /// This method decodes file list entries using the same wire format that
    /// upstream rsync uses in batch files - a raw tee of the protocol stream.
    /// The decoder is configured using the protocol version and compatibility
    /// flags from the batch header, plus the stream flags (preserve_uid, etc.)
    /// that were recorded when the batch was written.
    ///
    /// Returns all decoded file entries. After this call, the batch file reader
    /// is positioned at the start of the delta operations section.
    ///
    /// # Upstream Reference
    ///
    /// - `batch.c` — batch file body is a raw protocol stream tee
    /// - `flist.c:recv_file_entry()` — wire format decoded by `FileListReader`
    pub fn read_protocol_flist(&mut self) -> BatchResult<Vec<protocol::flist::FileEntry>> {
        if self.header.is_none() {
            return Err(BatchError::Io(io::Error::other(
                "Must read header before protocol flist",
            )));
        }

        let header = self.header.as_ref().expect("header checked above");
        let flags = header.stream_flags;

        let protocol_version =
            ProtocolVersion::try_from(header.protocol_version as u8).map_err(|_| {
                BatchError::InvalidFormat(format!(
                    "unsupported protocol version {} in batch header",
                    header.protocol_version,
                ))
            })?;

        // Build the flist reader, configuring preserve flags to match the
        // options that were active when the batch was written.
        let mut flist_reader = if let Some(cf) = header.compat_flags {
            let compat = CompatibilityFlags::from_bits(cf as u32);
            FileListReader::with_compat_flags(protocol_version, compat)
        } else {
            FileListReader::new(protocol_version)
        };
        // upstream: batch.c flag_ptr[] - preserve_devices (bit 4) covers both
        // --devices and --specials (upstream `-D` = `--devices --specials`).
        // The flist reader needs both flags set to correctly decode device and
        // special file entries.
        flist_reader = flist_reader
            .with_preserve_uid(flags.preserve_uid)
            .with_preserve_gid(flags.preserve_gid)
            .with_preserve_links(flags.preserve_links)
            .with_preserve_devices(flags.preserve_devices)
            .with_preserve_specials(flags.preserve_devices)
            .with_preserve_hard_links(flags.preserve_hard_links)
            .with_preserve_acls(flags.preserve_acls)
            .with_preserve_xattrs(flags.preserve_xattrs);

        // upstream: flist.c:150 - when always_checksum is set, each regular file
        // entry in the flist carries a trailing checksum of flist_csum_len bytes.
        // Without this, the reader would skip those bytes and go out of sync.
        // The checksum length depends on the negotiated algorithm. For batch files
        // without explicit negotiation, the default is MD5 (protocol >= 30) or
        // MD4 (protocol < 30) - both produce 16-byte digests.
        if flags.always_checksum {
            let csum_len = default_flist_csum_len(header.protocol_version);
            flist_reader = flist_reader.with_always_checksum(csum_len);
        }

        let reader = self
            .batch_file
            .as_mut()
            .ok_or_else(|| BatchError::Io(io::Error::other("Batch file not open")))?;

        let mut entries = Vec::new();
        loop {
            match flist_reader.read_entry(reader) {
                Ok(Some(entry)) => entries.push(entry),
                Ok(None) => break,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => {
                    return Err(BatchError::Io(io::Error::new(
                        e.kind(),
                        format!("Failed to read protocol flist entry: {e}"),
                    )));
                }
            }
        }

        // Capture any I/O error accumulated during flist reading.
        // upstream: flist.c:recv_file_list() does `io_error |= err` when the
        // sender reports errors, then breaks the loop without aborting.
        self.io_error = flist_reader.io_error();

        Ok(entries)
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
}

/// Returns the default flist checksum length for a batch file.
///
/// Upstream `flist.c:150` computes `flist_csum_len = csum_len_for_type(file_sum_nni->num, 1)`.
/// Without explicit checksum negotiation (which batch files bypass), the default
/// file checksum algorithm is MD5 (protocol >= 30) or MD4 (protocol < 30). Both
/// produce 16-byte digests. Protocol < 27 with `CSUM_MD4_ARCHAIC` uses 2 bytes
/// for flist checksums, but we only support protocol >= 27.
///
/// # Upstream Reference
///
/// - `checksum.c:csum_len_for_type()` - MD4=16, MD5=16, XXH3_128=16, XXH64=8
fn default_flist_csum_len(protocol_version: i32) -> usize {
    // All supported protocols (27-32) default to MD4 or MD5, both 16 bytes.
    // If XXH3-128 is negotiated via checksum seeds, it is also 16 bytes.
    // XXH64 and XXH3-64 are 8 bytes but require explicit negotiation which
    // is not recorded in the batch stream flags. Conservative default: 16.
    let _ = protocol_version;
    16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BatchMode, BatchWriter};
    use std::path::Path;
    use tempfile::TempDir;

    #[allow(clippy::field_reassign_with_default)]
    fn create_test_batch(path: &Path) {
        let config = BatchConfig::new(BatchMode::Write, path.to_string_lossy().to_string(), 30)
            .with_checksum_seed(12345);

        let mut writer = BatchWriter::new(config).unwrap();
        let mut flags = BatchFlags::default();
        flags.recurse = true;
        writer.write_header(flags).unwrap();
        writer.write_data(b"test data here").unwrap();
        writer.finalize().unwrap();
    }

    mod reader_creation_tests {
        use super::*;

        #[test]
        fn create_with_valid_file() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let reader = BatchReader::new(config);
            assert!(reader.is_ok());
        }

        #[test]
        fn create_with_nonexistent_file() {
            let config = BatchConfig::new(
                BatchMode::Read,
                "/nonexistent/path/batch.file".to_owned(),
                30,
            );

            let reader = BatchReader::new(config);
            assert!(reader.is_err());
        }

        #[test]
        fn header_is_none_before_read() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let reader = BatchReader::new(config).unwrap();
            assert!(reader.header().is_none());
        }
    }

    mod header_tests {
        use super::*;

        #[test]
        fn read_header_success() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let mut reader = BatchReader::new(config).unwrap();
            let flags = reader.read_header().unwrap();

            assert!(flags.recurse);
            assert!(reader.header().is_some());
        }

        #[test]
        fn double_header_read_error() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();
            let result = reader.read_header();
            assert!(result.is_err());
        }

        #[test]
        fn adopts_protocol_version_from_header() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                28, // Different from the 30 used to write
            );

            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            // Config should adopt the protocol version from the batch header
            assert_eq!(reader.config().protocol_version, 30);
            assert_eq!(reader.header().unwrap().protocol_version, 30);
        }

        #[test]
        fn adopts_compat_flags_from_header() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("compat.batch");

            // Write a batch with non-zero compat flags
            let config = BatchConfig::new(
                BatchMode::Write,
                batch_path.to_string_lossy().to_string(),
                31,
            )
            .with_compat_flags(0x3F)
            .with_checksum_seed(99);
            let mut writer = BatchWriter::new(config).unwrap();
            writer.write_header(BatchFlags::default()).unwrap();
            writer.finalize().unwrap();

            // Read back with different config values
            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                31,
            );
            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            // Config must adopt the compat flags from the batch header
            assert_eq!(reader.config().compat_flags, Some(0x3F));
            assert_eq!(reader.header().unwrap().compat_flags, Some(0x3F));
        }

        #[test]
        fn adopts_checksum_seed_from_header() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("seed.batch");

            // Write a batch with a specific checksum seed
            let config = BatchConfig::new(
                BatchMode::Write,
                batch_path.to_string_lossy().to_string(),
                30,
            )
            .with_checksum_seed(0xDEAD);
            let mut writer = BatchWriter::new(config).unwrap();
            writer.write_header(BatchFlags::default()).unwrap();
            writer.finalize().unwrap();

            // Read back with default seed (0)
            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );
            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            // Config must adopt the checksum seed from the batch header
            assert_eq!(reader.config().checksum_seed, 0xDEAD);
            assert_eq!(reader.header().unwrap().checksum_seed, 0xDEAD);
        }

        #[test]
        fn adopts_none_compat_flags_for_old_protocol() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("old_proto.batch");

            // Write with protocol 28 (no compat flags)
            let config = BatchConfig::new(
                BatchMode::Write,
                batch_path.to_string_lossy().to_string(),
                28,
            )
            .with_checksum_seed(42);
            let mut writer = BatchWriter::new(config).unwrap();
            writer.write_header(BatchFlags::default()).unwrap();
            writer.finalize().unwrap();

            // Read back - compat_flags should be None for protocol < 30
            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                31, // config says 31 but batch says 28
            );
            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            assert_eq!(reader.config().protocol_version, 28);
            assert_eq!(reader.config().compat_flags, None);
            assert_eq!(reader.config().checksum_seed, 42);
        }
    }

    mod data_tests {
        use super::*;

        #[test]
        fn read_data_without_header() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let mut reader = BatchReader::new(config).unwrap();
            let mut buf = [0u8; 100];
            assert!(reader.read_data(&mut buf).is_err());
        }

        #[test]
        fn read_data_success() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            let mut buf = [0u8; 100];
            let n = reader.read_data(&mut buf).unwrap();
            assert!(n > 0);
            assert_eq!(&buf[..14], b"test data here");
        }

        #[test]
        fn read_exact_without_header() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let mut reader = BatchReader::new(config).unwrap();
            let mut buf = [0u8; 10];
            assert!(reader.read_exact(&mut buf).is_err());
        }

        #[test]
        fn read_exact_success() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            let mut buf = [0u8; 4];
            reader.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"test");
        }

        #[test]
        fn read_exact_insufficient_data() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            // Try to read more data than available
            let mut buf = [0u8; 1000];
            let result = reader.read_exact(&mut buf);
            assert!(result.is_err());
        }
    }

    mod file_entry_tests {
        use super::*;

        #[test]
        fn read_file_entry_without_header() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let mut reader = BatchReader::new(config).unwrap();
            let result = reader.read_file_entry();
            assert!(result.is_err());
        }

        #[test]
        fn read_file_entry_returns_none_on_eof() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("empty.batch");

            // Create a batch with just header, no file entries
            let config = BatchConfig::new(
                BatchMode::Write,
                batch_path.to_string_lossy().to_string(),
                30,
            );
            let mut writer = BatchWriter::new(config).unwrap();
            writer.write_header(BatchFlags::default()).unwrap();
            writer.finalize().unwrap();

            // Read it back
            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );
            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            // Should return None on EOF
            let entry = reader.read_file_entry().unwrap();
            assert!(entry.is_none());
        }
    }

    mod config_tests {
        use super::*;

        #[test]
        fn config_accessor() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let reader = BatchReader::new(config).unwrap();
            assert_eq!(reader.config().protocol_version, 30);
        }

        #[test]
        fn header_accessor_before_read() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let reader = BatchReader::new(config).unwrap();
            assert!(reader.header().is_none());
        }

        #[test]
        fn header_accessor_after_read() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();
            let header = reader.header().unwrap();
            assert_eq!(header.protocol_version, 30);
        }

        #[test]
        fn io_error_starts_at_zero() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let reader = BatchReader::new(config).unwrap();
            assert_eq!(reader.io_error(), 0);
        }
    }

    mod flist_deserialization_tests {
        use super::*;
        use protocol::flist::{FileEntry as ProtocolFileEntry, FileListWriter};

        /// Write a batch with protocol flist entries and read them back using
        /// `read_protocol_flist`. This validates the core deserialization path
        /// that batch replay depends on.
        #[test]
        fn protocol_flist_roundtrip_basic() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("flist_basic.batch");
            let protocol_version = 31;

            // Write phase
            let write_config = BatchConfig::new(
                BatchMode::Write,
                batch_path.to_string_lossy().to_string(),
                protocol_version,
            )
            .with_checksum_seed(42);

            let mut writer = BatchWriter::new(write_config).unwrap();
            let flags = BatchFlags {
                recurse: true,
                preserve_uid: true,
                preserve_gid: true,
                ..Default::default()
            };
            writer.write_header(flags).unwrap();

            let protocol =
                protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
            let mut flist_writer = FileListWriter::new(protocol)
                .with_preserve_uid(true)
                .with_preserve_gid(true);

            let entries = vec![
                {
                    let mut e = ProtocolFileEntry::new_file("alpha.txt".into(), 1024, 0o644);
                    e.set_mtime(1_700_000_000, 0);
                    e.set_uid(1000);
                    e.set_gid(1000);
                    e
                },
                {
                    let mut e = ProtocolFileEntry::new_file("beta.txt".into(), 2048, 0o644);
                    e.set_mtime(1_700_000_001, 0);
                    e.set_uid(1001);
                    e.set_gid(1001);
                    e
                },
                {
                    let mut e = ProtocolFileEntry::new_directory("subdir".into(), 0o755);
                    e.set_mtime(1_700_000_002, 0);
                    e.set_uid(1000);
                    e.set_gid(1000);
                    e
                },
            ];

            for entry in &entries {
                let mut buf = Vec::new();
                flist_writer.write_entry(&mut buf, entry).unwrap();
                writer.write_data(&buf).unwrap();
            }

            let mut end_buf = Vec::new();
            flist_writer.write_end(&mut end_buf, None).unwrap();
            writer.write_data(&end_buf).unwrap();
            writer.finalize().unwrap();

            // Read phase
            let read_config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                protocol_version,
            );
            let mut reader = BatchReader::new(read_config).unwrap();
            reader.read_header().unwrap();

            let read_entries = reader.read_protocol_flist().unwrap();
            assert_eq!(read_entries.len(), 3);
            assert_eq!(read_entries[0].name(), "alpha.txt");
            assert_eq!(read_entries[0].size(), 1024);
            assert_eq!(read_entries[0].uid(), Some(1000));
            assert_eq!(read_entries[1].name(), "beta.txt");
            assert_eq!(read_entries[1].size(), 2048);
            assert_eq!(read_entries[2].name(), "subdir");
            assert!(read_entries[2].is_dir());

            // io_error should be zero for a clean flist
            assert_eq!(reader.io_error(), 0);
        }

        /// Validates that `always_checksum` (--checksum / -c) is correctly wired
        /// to the flist reader. When this flag is set, each regular file entry
        /// in the flist carries a trailing checksum. If the reader doesn't consume
        /// these bytes, subsequent entries will be deserialized incorrectly.
        ///
        /// upstream: flist.c:670 writes checksum bytes, flist.c:1202 reads them
        #[test]
        fn protocol_flist_roundtrip_with_always_checksum() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("flist_checksum.batch");
            let protocol_version = 31;
            let csum_len = 16; // MD5 digest length

            // Write phase
            let write_config = BatchConfig::new(
                BatchMode::Write,
                batch_path.to_string_lossy().to_string(),
                protocol_version,
            )
            .with_checksum_seed(99);

            let mut writer = BatchWriter::new(write_config).unwrap();
            let flags = BatchFlags {
                recurse: true,
                always_checksum: true,
                ..Default::default()
            };
            writer.write_header(flags).unwrap();

            let protocol =
                protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
            let mut flist_writer = FileListWriter::new(protocol)
                .with_always_checksum(csum_len);

            // Two regular files - each will have a checksum after it on the wire
            let entries = vec![
                {
                    let mut e = ProtocolFileEntry::new_file("file1.dat".into(), 500, 0o644);
                    e.set_mtime(1_700_000_000, 0);
                    e.set_checksum(vec![0xAA; csum_len]);
                    e
                },
                {
                    let mut e = ProtocolFileEntry::new_file("file2.dat".into(), 1500, 0o644);
                    e.set_mtime(1_700_000_001, 0);
                    e.set_checksum(vec![0xBB; csum_len]);
                    e
                },
            ];

            for entry in &entries {
                let mut buf = Vec::new();
                flist_writer.write_entry(&mut buf, entry).unwrap();
                writer.write_data(&buf).unwrap();
            }

            let mut end_buf = Vec::new();
            flist_writer.write_end(&mut end_buf, None).unwrap();
            writer.write_data(&end_buf).unwrap();
            writer.finalize().unwrap();

            // Read phase - the reader must correctly consume checksum bytes
            let read_config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                protocol_version,
            );
            let mut reader = BatchReader::new(read_config).unwrap();
            reader.read_header().unwrap();

            let read_entries = reader.read_protocol_flist().unwrap();
            assert_eq!(
                read_entries.len(),
                2,
                "should read both entries when always_checksum is wired correctly"
            );
            assert_eq!(read_entries[0].name(), "file1.dat");
            assert_eq!(read_entries[0].size(), 500);
            assert_eq!(read_entries[1].name(), "file2.dat");
            assert_eq!(read_entries[1].size(), 1500);
        }

        /// Verifies that `default_flist_csum_len` returns 16 for all supported
        /// protocol versions, matching upstream MD4/MD5 digest length.
        #[test]
        fn default_flist_csum_len_values() {
            for proto in [27, 28, 29, 30, 31, 32] {
                assert_eq!(
                    default_flist_csum_len(proto),
                    16,
                    "flist_csum_len should be 16 for protocol {proto}"
                );
            }
        }

        /// Verifies that an empty flist (just the end marker) reads back as
        /// an empty vec with zero io_error.
        #[test]
        fn protocol_flist_empty() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("flist_empty.batch");
            let protocol_version = 31;

            let write_config = BatchConfig::new(
                BatchMode::Write,
                batch_path.to_string_lossy().to_string(),
                protocol_version,
            );

            let mut writer = BatchWriter::new(write_config).unwrap();
            writer.write_header(BatchFlags::default()).unwrap();

            let protocol =
                protocol::ProtocolVersion::try_from(protocol_version as u8).unwrap();
            let mut flist_writer = FileListWriter::new(protocol);

            // Write only the end marker, no entries
            let mut end_buf = Vec::new();
            flist_writer.write_end(&mut end_buf, None).unwrap();
            writer.write_data(&end_buf).unwrap();
            writer.finalize().unwrap();

            let read_config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                protocol_version,
            );
            let mut reader = BatchReader::new(read_config).unwrap();
            reader.read_header().unwrap();

            let read_entries = reader.read_protocol_flist().unwrap();
            assert!(read_entries.is_empty());
            assert_eq!(reader.io_error(), 0);
        }
    }
}
