//! Sliding window buffered file mapper.
//!
//! Implements the `map_ptr()` pattern from upstream rsync's `fileio.c`,
//! maintaining a cached buffer window that slides forward as sequential
//! reads progress through the file.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::constants::{MAX_MAP_SIZE, align_down};

use super::MapStrategy;

/// Buffered file mapper with sliding window cache.
///
/// Maintains a buffer of up to `MAX_MAP_SIZE` bytes and slides the window
/// as needed to serve read requests efficiently.
#[derive(Debug)]
pub struct BufferedMap {
    /// The underlying file handle.
    file: File,
    /// Total file size in bytes.
    size: u64,
    /// Cached data buffer.
    buffer: Vec<u8>,
    /// Starting offset of cached data in the file.
    pub(crate) window_start: u64,
    /// Number of valid bytes in the buffer.
    pub(crate) window_len: usize,
    /// Maximum window size (typically `MAX_MAP_SIZE`).
    max_window: usize,
}

impl BufferedMap {
    /// Opens a file for buffered mapping.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or its size determined.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Self::open_with_window(path, MAX_MAP_SIZE)
    }

    /// Opens a file with a custom window size.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or its size determined.
    pub fn open_with_window<P: AsRef<Path>>(path: P, window_size: usize) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();

        Ok(Self {
            file,
            size,
            buffer: Vec::with_capacity(window_size),
            window_start: 0,
            window_len: 0,
            max_window: window_size,
        })
    }

    /// Creates a `BufferedMap` from an already-open file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file size cannot be determined.
    pub fn from_file(file: File) -> io::Result<Self> {
        Self::from_file_with_window(file, MAX_MAP_SIZE)
    }

    /// Creates a `BufferedMap` from an already-open file with custom window.
    ///
    /// # Errors
    ///
    /// Returns an error if the file size cannot be determined.
    pub fn from_file_with_window(file: File, window_size: usize) -> io::Result<Self> {
        let size = file.metadata()?.len();

        Ok(Self {
            file,
            size,
            buffer: Vec::with_capacity(window_size),
            window_start: 0,
            window_len: 0,
            max_window: window_size,
        })
    }

    /// Returns true if the requested range is within the current window.
    #[inline]
    fn is_in_window(&self, offset: u64, len: usize) -> bool {
        offset >= self.window_start
            && offset.saturating_add(len as u64) <= self.window_start + self.window_len as u64
    }

    /// Loads a new window, reusing overlapping bytes from the current window
    /// when the slide is forward (sequential access pattern).
    ///
    /// Mirrors upstream rsync's `map_ptr()` (fileio.c:268-279) which uses
    /// `memmove()` to retain bytes that overlap between the old and new window
    /// positions, avoiding redundant disk reads.
    fn load_window(&mut self, offset: u64, min_len: usize) -> io::Result<()> {
        let aligned_start = align_down(offset);

        let remaining = self.size.saturating_sub(aligned_start);
        let window_size = (self.max_window as u64).min(remaining) as usize;

        let offset_in_window = (offset - aligned_start) as usize;
        let required_size = offset_in_window + min_len;
        if window_size < required_size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "requested range extends past end of file",
            ));
        }

        // upstream: fileio.c:268-279 - reuse overlapping bytes when sliding
        // forward. The new window starts at `aligned_start`; if the old window
        // overlaps the beginning of the new window AND the new window extends
        // past the old window's end, shift the overlap via copy_within and
        // only read the new portion from disk.
        let old_end = self.window_start + self.window_len as u64;
        let (read_start, read_offset) = if self.window_len > 0
            && aligned_start >= self.window_start
            && aligned_start < old_end
            && aligned_start + window_size as u64 >= old_end
        {
            let reuse_len = (old_end - aligned_start) as usize;
            let src_offset = (aligned_start - self.window_start) as usize;

            self.buffer.resize(window_size, 0);
            self.buffer
                .copy_within(src_offset..src_offset + reuse_len, 0);

            (old_end, reuse_len)
        } else {
            self.buffer.resize(window_size, 0);
            (aligned_start, 0)
        };

        let read_size = window_size - read_offset;
        if read_size > 0 {
            self.file.seek(SeekFrom::Start(read_start))?;
            self.file
                .read_exact(&mut self.buffer[read_offset..window_size])?;
        }

        self.window_start = aligned_start;
        self.window_len = window_size;

        Ok(())
    }
}

impl MapStrategy for BufferedMap {
    fn map_ptr(&mut self, offset: u64, len: usize) -> io::Result<&[u8]> {
        if len == 0 {
            return Ok(&[]);
        }

        if offset.saturating_add(len as u64) > self.size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "requested range extends past end of file",
            ));
        }

        if !self.is_in_window(offset, len) {
            self.load_window(offset, len)?;
        }

        let start = (offset - self.window_start) as usize;
        let end = start + len;
        Ok(&self.buffer[start..end])
    }

    #[inline]
    fn window_size(&self) -> usize {
        self.max_window
    }

    #[inline]
    fn file_size(&self) -> u64 {
        self.size
    }
}
