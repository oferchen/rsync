//! Batch file reader for replaying transfers.

use super::BatchConfig;
use super::format::{BatchFlags, BatchHeader};
use crate::error::{EngineError, EngineResult};
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
}

impl BatchReader {
    /// Create a new batch reader.
    pub fn new(config: BatchConfig) -> EngineResult<Self> {
        // Open the batch file
        let batch_path = config.batch_file_path();
        let file = File::open(batch_path).map_err(|e| {
            EngineError::Io(io::Error::new(
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
        })
    }

    /// Read and validate the batch header.
    ///
    /// Returns the stream flags that were recorded in the batch.
    pub fn read_header(&mut self) -> EngineResult<BatchFlags> {
        if self.header.is_some() {
            return Err(EngineError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Batch header already read",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            let header = BatchHeader::read_from(reader).map_err(|e| {
                EngineError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read batch header: {e}"),
                ))
            })?;

            // Validate protocol version
            if header.protocol_version != self.config.protocol_version {
                return Err(EngineError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Protocol version mismatch: batch has {}, expected {}",
                        header.protocol_version, self.config.protocol_version
                    ),
                )));
            }

            let flags = header.stream_flags;
            self.header = Some(header);
            Ok(flags)
        } else {
            Err(EngineError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Read data from the batch file.
    ///
    /// This reads the next chunk of data from the batch file, which
    /// could be file list entries or delta operations.
    pub fn read_data(&mut self, buf: &mut [u8]) -> EngineResult<usize> {
        if self.header.is_none() {
            return Err(EngineError::Io(io::Error::other(
                "Must read header before data",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            reader.read(buf).map_err(|e| {
                EngineError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read batch data: {e}"),
                ))
            })
        } else {
            Err(EngineError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Read exact amount of data from the batch file.
    pub fn read_exact(&mut self, buf: &mut [u8]) -> EngineResult<()> {
        if self.header.is_none() {
            return Err(EngineError::Io(io::Error::other(
                "Must read header before data",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            reader.read_exact(buf).map_err(|e| {
                EngineError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read exact batch data: {e}"),
                ))
            })
        } else {
            Err(EngineError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Get the header that was read from the batch file.
    pub fn header(&self) -> Option<&BatchHeader> {
        self.header.as_ref()
    }

    /// Get a reference to the batch configuration.
    pub fn config(&self) -> &BatchConfig {
        &self.config
    }

    /// Read a file entry from the batch file.
    ///
    /// Returns the next file list entry, or None if end of file list is reached.
    pub fn read_file_entry(&mut self) -> EngineResult<Option<super::format::FileEntry>> {
        if self.header.is_none() {
            return Err(EngineError::Io(io::Error::other(
                "Must read header before file entries",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            // Try to read the next file entry
            // If we hit EOF or an empty path, we've reached the end of the file list
            match super::format::FileEntry::read_from(reader) {
                Ok(entry) => {
                    if entry.path.is_empty() {
                        Ok(None) // End of file list marker
                    } else {
                        Ok(Some(entry))
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
                Err(e) => Err(EngineError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read file entry: {e}"),
                ))),
            }
        } else {
            Err(EngineError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Read all remaining delta operations from the batch file.
    ///
    /// Reads individual delta operations until EOF. This is a simplified
    /// implementation for single-file batches. Multi-file batches need
    /// more sophisticated parsing to detect file entry boundaries.
    ///
    /// # Implementation Note
    ///
    /// Multi-file batch parsing requires lookahead to detect when one file's
    /// delta stream ends and the next begins. Upstream rsync handles this via
    /// a state machine in batch.c that tracks file list indices.
    ///
    /// Current implementation: Single-file batches only (reads until EOF).
    /// Future enhancement: Add file boundary detection for multi-file batches.
    pub fn read_all_delta_ops(&mut self) -> EngineResult<Vec<protocol::wire::delta::DeltaOp>> {
        if self.header.is_none() {
            return Err(EngineError::Io(io::Error::other(
                "Must read header before delta operations",
            )));
        }

        let mut ops = Vec::new();

        if let Some(ref mut reader) = self.batch_file {
            // Read delta operations until EOF
            loop {
                match protocol::wire::delta::read_delta_op(reader) {
                    Ok(op) => {
                        ops.push(op);
                    }
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                        // End of delta operations
                        break;
                    }
                    Err(e) => {
                        // If we've already read some ops successfully, this might just be
                        // the end of delta data. Otherwise it's a real error.
                        if ops.is_empty() {
                            return Err(EngineError::Io(io::Error::new(
                                e.kind(),
                                format!("Failed to read first delta operation: {e}"),
                            )));
                        } else {
                            // Assume end of delta data
                            break;
                        }
                    }
                }
            }

            Ok(ops)
        } else {
            Err(EngineError::Io(io::Error::other("Batch file not open")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{BatchMode, BatchWriter};
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
                "/nonexistent/path/batch.file".to_string(),
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
        fn protocol_mismatch() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                28, // Different from the 30 used to write
            );

            let mut reader = BatchReader::new(config).unwrap();
            let result = reader.read_header();
            assert!(result.is_err());
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

    mod delta_ops_tests {
        use super::*;

        #[test]
        fn read_delta_ops_without_header() {
            let temp_dir = TempDir::new().unwrap();
            let batch_path = temp_dir.path().join("test.batch");
            create_test_batch(&batch_path);

            let config = BatchConfig::new(
                BatchMode::Read,
                batch_path.to_string_lossy().to_string(),
                30,
            );

            let mut reader = BatchReader::new(config).unwrap();
            let result = reader.read_all_delta_ops();
            assert!(result.is_err());
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
    }
}
