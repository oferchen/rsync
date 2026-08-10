//! Streaming file list reader with incremental dependency tracking.
//!
//! Wraps a [`FileListReader`](crate::flist::FileListReader) to yield entries
//! as they arrive from the wire, respecting parent directory dependencies.
//!
//! # Upstream Reference
//!
//! - `io.c:read_a_msg()` - Incremental file list segments in INC_RECURSE mode

use crate::flist::{FileEntry, FileListReader, FileListStats};

use super::{IncrementalFileList, IncrementalFileListIter};

/// Streaming file list reader that yields entries incrementally.
///
/// Wraps a [`FileListReader`] and provides an iterator-like interface
/// that yields entries as they are read from the wire, with dependency tracking.
#[derive(Debug)]
pub struct StreamingFileList<R> {
    reader: FileListReader,
    source: R,
    incremental: IncrementalFileList,
    finished_reading: bool,
}

impl<R: std::io::Read> StreamingFileList<R> {
    /// Creates a new streaming file list reader.
    pub fn new(reader: FileListReader, source: R) -> Self {
        Self {
            reader,
            source,
            incremental: IncrementalFileList::new(),
            finished_reading: false,
        }
    }

    /// Creates a new streaming file list reader with incremental recursion mode.
    pub fn with_incremental_recursion(reader: FileListReader, source: R) -> Self {
        Self {
            reader,
            source,
            incremental: IncrementalFileList::with_incremental_recursion(),
            finished_reading: false,
        }
    }

    /// Reads the next batch of entries from the wire.
    ///
    /// Reads entries until either an entry becomes ready for processing,
    /// the end of the file list is reached, or an I/O error occurs.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(entry))` - An entry is ready for processing
    /// - `Ok(None)` - No more entries (end of list reached)
    /// - `Err(e)` - An I/O error occurred
    pub fn next_ready(&mut self) -> std::io::Result<Option<FileEntry>> {
        if let Some(entry) = self.incremental.pop() {
            return Ok(Some(entry));
        }

        if self.finished_reading {
            return Ok(None);
        }

        loop {
            match self.reader.read_entry(&mut self.source)? {
                Some(entry) => {
                    self.incremental.push(entry);
                    if let Some(ready) = self.incremental.pop() {
                        return Ok(Some(ready));
                    }
                }
                None => {
                    self.finished_reading = true;
                    return Ok(self.incremental.pop());
                }
            }
        }
    }

    /// Reads all entries from the wire and returns an iterator over ready entries.
    ///
    /// Useful when you want to process entries in order after reading
    /// the complete list, but still want incremental dependency tracking.
    pub fn read_all(mut self) -> std::io::Result<IncrementalFileListIter> {
        while !self.finished_reading {
            match self.reader.read_entry(&mut self.source)? {
                Some(entry) => {
                    self.incremental.push(entry);
                }
                None => {
                    self.finished_reading = true;
                }
            }
        }
        Ok(self.incremental.into_iter())
    }

    /// Returns the number of entries ready for processing.
    #[must_use]
    pub fn ready_count(&self) -> usize {
        self.incremental.ready_count()
    }

    /// Returns the number of entries pending (waiting for parent).
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.incremental.pending_count()
    }

    /// Returns whether reading from the wire is complete.
    #[must_use]
    pub const fn is_finished_reading(&self) -> bool {
        self.finished_reading
    }

    /// Marks a directory as created (for external directory creation).
    pub fn mark_directory_created(&mut self, path: &str) {
        self.incremental.mark_directory_created(path);
    }

    /// Returns the file list statistics from the reader.
    #[must_use]
    pub const fn stats(&self) -> &FileListStats {
        self.reader.stats()
    }
}

/// Iterator adapter for streaming file list.
impl<R: std::io::Read> Iterator for StreamingFileList<R> {
    type Item = std::io::Result<FileEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_ready() {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}
