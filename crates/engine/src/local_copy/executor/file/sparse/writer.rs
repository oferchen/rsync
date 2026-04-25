//! High-level sparse file writer wrapping a standard `File`.
//!
//! Provides a [`Write`] implementation that transparently creates filesystem
//! holes for zero-filled regions when sparse mode is enabled.

use std::fs;
use std::io::{self, Seek, SeekFrom, Write};

use super::state::{SparseWriteState, write_sparse_chunk};

/// Wrapper around a file for writing with sparse support.
///
/// This wraps a standard `File` and provides high-level methods for writing
/// files with automatic sparse hole creation. When sparse mode is enabled,
/// zero-filled regions are efficiently stored as holes using filesystem
/// mechanisms (seek or fallocate).
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use engine::SparseWriter;
///
/// let file = File::create("output.bin").unwrap();
/// let mut writer = SparseWriter::new(file, true);
///
/// // Write data - zeros will automatically become holes if sparse is enabled
/// writer.write_region(0, b"hello").unwrap();
/// writer.write_region(1000, &[0u8; 10000]).unwrap(); // This becomes a hole
/// writer.write_region(11000, b"world").unwrap();
///
/// writer.finish(11005).unwrap();
/// ```
pub struct SparseWriter {
    file: fs::File,
    sparse_enabled: bool,
    state: SparseWriteState,
}

impl SparseWriter {
    /// Creates a new sparse writer wrapping the given file.
    ///
    /// # Arguments
    ///
    /// * `file` - The file to write to
    /// * `sparse_enabled` - Whether to create sparse holes for zero regions
    #[must_use]
    pub fn new(file: fs::File, sparse_enabled: bool) -> Self {
        Self {
            file,
            sparse_enabled,
            state: SparseWriteState::default(),
        }
    }

    /// Writes a region of data at the specified offset.
    ///
    /// If sparse mode is enabled, zero-filled portions of the data will be
    /// converted to holes. Otherwise, all data is written densely.
    ///
    /// Note: For correct sparse handling, regions must be written sequentially
    /// and contiguously. Non-sequential writes may not create proper holes.
    ///
    /// # Arguments
    ///
    /// * `offset` - File offset where this data should be written
    /// * `data` - The data to write
    ///
    /// # Returns
    ///
    /// An I/O error if the write fails.
    pub fn write_region(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        if self.sparse_enabled {
            self.file
                .seek(SeekFrom::Start(offset))
                .map_err(|e| io::Error::new(e.kind(), format!("seek to offset {offset}: {e}")))?;

            let path = std::path::Path::new("");
            write_sparse_chunk(&mut self.file, &mut self.state, data, path)
                .map_err(io::Error::other)?;
        } else {
            self.file
                .seek(SeekFrom::Start(offset))
                .map_err(|e| io::Error::new(e.kind(), format!("seek to offset {offset}: {e}")))?;
            self.file.write_all(data)?;
        }

        Ok(())
    }

    /// Finishes writing and sets the final file size.
    ///
    /// Any pending sparse zeros are flushed, and the file is truncated to the
    /// specified size. After this call, the writer should not be used again.
    ///
    /// # Arguments
    ///
    /// * `total_size` - The final size of the file in bytes
    ///
    /// # Returns
    ///
    /// An I/O error if finishing fails.
    pub fn finish(mut self, total_size: u64) -> io::Result<()> {
        if self.sparse_enabled {
            let path = std::path::Path::new("");
            self.state
                .finish(&mut self.file, path)
                .map_err(io::Error::other)?;
        }

        self.file.set_len(total_size)?;
        self.file.sync_all()?;

        Ok(())
    }

    /// Returns a reference to the underlying file.
    pub fn file(&self) -> &fs::File {
        &self.file
    }

    /// Returns a mutable reference to the underlying file.
    pub fn file_mut(&mut self) -> &mut fs::File {
        &mut self.file
    }
}
