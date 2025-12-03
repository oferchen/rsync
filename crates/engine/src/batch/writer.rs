//! Batch file writer for recording transfers.

use super::BatchConfig;
use super::format::{BatchFlags, BatchHeader};
use crate::error::{EngineError, EngineResult};
use std::fs::File;
use std::io::{self, BufWriter, Write};

/// Writer for batch mode operations.
///
/// Records file list and delta operations to a batch file that can be
/// replayed later. This allows offline distribution of changes.
#[derive(Debug)]
pub struct BatchWriter {
    /// Configuration for this batch operation.
    config: BatchConfig,
    /// Writer for the binary batch file.
    batch_file: Option<BufWriter<File>>,
    /// Whether the header has been written.
    header_written: bool,
}

impl BatchWriter {
    /// Create a new batch writer.
    pub fn new(config: BatchConfig) -> EngineResult<Self> {
        // Create the batch file
        let batch_path = config.batch_file_path();
        let file = File::create(batch_path).map_err(|e| {
            EngineError::Io(io::Error::new(
                e.kind(),
                format!(
                    "Failed to create batch file '{}': {}",
                    batch_path.display(),
                    e
                ),
            ))
        })?;

        Ok(Self {
            config,
            batch_file: Some(BufWriter::new(file)),
            header_written: false,
        })
    }

    /// Write the batch header with stream flags.
    pub fn write_header(&mut self, flags: BatchFlags) -> EngineResult<()> {
        if self.header_written {
            return Err(EngineError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Batch header already written",
            )));
        }

        let mut header = BatchHeader::new(self.config.protocol_version, self.config.checksum_seed);
        header.compat_flags = self.config.compat_flags;
        header.stream_flags = flags;

        if let Some(ref mut writer) = self.batch_file {
            header.write_to(writer).map_err(|e| {
                EngineError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to write batch header: {}", e),
                ))
            })?;
            self.header_written = true;
            Ok(())
        } else {
            Err(EngineError::Io(io::Error::new(
                io::ErrorKind::Other,
                "Batch file not open",
            )))
        }
    }

    /// Write raw data to the batch file.
    ///
    /// This is used to record file list and delta operations as they
    /// occur during the transfer. The data should be in the same format
    /// as it would be sent over the network.
    pub fn write_data(&mut self, data: &[u8]) -> EngineResult<()> {
        if !self.header_written {
            return Err(EngineError::Io(io::Error::new(
                io::ErrorKind::Other,
                "Must write header before data",
            )));
        }

        if let Some(ref mut writer) = self.batch_file {
            writer.write_all(data).map_err(|e| {
                EngineError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to write batch data: {}", e),
                ))
            })?;
            Ok(())
        } else {
            Err(EngineError::Io(io::Error::new(
                io::ErrorKind::Other,
                "Batch file not open",
            )))
        }
    }

    /// Flush any buffered data to disk.
    pub fn flush(&mut self) -> EngineResult<()> {
        if let Some(ref mut writer) = self.batch_file {
            writer.flush().map_err(|e| {
                EngineError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to flush batch file: {}", e),
                ))
            })?;
        }
        Ok(())
    }

    /// Finalize the batch file and close it.
    ///
    /// This ensures all data is written and the file is properly closed.
    /// After calling this, the writer can no longer be used.
    pub fn finalize(mut self) -> EngineResult<()> {
        self.flush()?;
        if let Some(writer) = self.batch_file.take() {
            drop(writer);
        }
        Ok(())
    }

    /// Get a reference to the batch configuration.
    pub fn config(&self) -> &BatchConfig {
        &self.config
    }
}

impl Drop for BatchWriter {
    fn drop(&mut self) {
        // Ensure file is flushed on drop
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::BatchMode;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_batch_writer_create() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let writer = BatchWriter::new(config);
        assert!(writer.is_ok());
    }

    #[test]
    fn test_batch_writer_write_header() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        )
        .with_checksum_seed(12345);

        let mut writer = BatchWriter::new(config).unwrap();

        let mut flags = BatchFlags::default();
        flags.recurse = true;
        flags.preserve_uid = true;

        assert!(writer.write_header(flags).is_ok());
        assert!(writer.flush().is_ok());

        // Verify file exists and has content
        assert!(batch_path.exists());
        let metadata = fs::metadata(&batch_path).unwrap();
        assert!(metadata.len() > 0);
    }

    #[test]
    fn test_batch_writer_write_data() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();

        // Must write header first
        assert!(writer.write_data(b"test").is_err());

        let flags = BatchFlags::default();
        writer.write_header(flags).unwrap();

        // Now data write should succeed
        assert!(writer.write_data(b"test data").is_ok());
        assert!(writer.flush().is_ok());
    }

    #[test]
    fn test_batch_writer_finalize() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();
        let flags = BatchFlags::default();
        writer.write_header(flags).unwrap();
        writer.write_data(b"some data").unwrap();

        assert!(writer.finalize().is_ok());
        assert!(batch_path.exists());
    }
}
