use std::fs;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

#[cfg(target_os = "linux")]
use rustix::{
    fd::AsFd,
    fs::{FallocateFlags, fallocate},
    io::Errno,
};

use crate::local_copy::LocalCopyError;

/// Represents a region in a file, either containing data or a hole (sparse region).
///
/// Used by sparse file detection and reading operations to efficiently identify
/// and process zero-filled regions in files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SparseRegion {
    /// A region containing non-zero data.
    Data {
        /// Starting offset of the data region.
        offset: u64,
        /// Length of the data region in bytes.
        length: u64,
    },
    /// A sparse hole region (all zeros).
    Hole {
        /// Starting offset of the hole.
        offset: u64,
        /// Length of the hole in bytes.
        length: u64,
    },
}

impl SparseRegion {
    /// Returns the starting offset of this region.
    pub const fn offset(&self) -> u64 {
        match self {
            Self::Data { offset, .. } | Self::Hole { offset, .. } => *offset,
        }
    }

    /// Returns the length of this region in bytes.
    pub const fn length(&self) -> u64 {
        match self {
            Self::Data { length, .. } | Self::Hole { length, .. } => *length,
        }
    }

    /// Returns true if this is a hole (sparse) region.
    pub const fn is_hole(&self) -> bool {
        matches!(self, Self::Hole { .. })
    }

    /// Returns true if this is a data region.
    pub const fn is_data(&self) -> bool {
        matches!(self, Self::Data { .. })
    }
}

/// Threshold for detecting sparse (all-zeros) regions during file writes.
///
/// A run of zeros at least this size will be converted to a sparse hole
/// using fallocate(PUNCH_HOLE) or seek past on supported systems.
///
/// Matches upstream rsync's CHUNK_SIZE (32KB) for consistent behavior.
/// Using a larger threshold reduces syscall overhead for small zero runs
/// while still efficiently handling large sparse regions.
const SPARSE_WRITE_SIZE: usize = 32 * 1024;

/// Buffer size for writing zeros when fallocate is not supported.
/// Matches upstream rsync's do_punch_hole fallback buffer size.
const ZERO_WRITE_BUFFER_SIZE: usize = 4096;

/// Detects sparse (zero-filled) regions in data buffers.
///
/// This detector scans data buffers to identify runs of zero bytes that can be
/// efficiently represented as holes in sparse files. It uses optimized scanning
/// with a configurable minimum hole size threshold.
///
/// # Examples
///
/// ```
/// use engine::{SparseDetector, SparseRegion};
///
/// let detector = SparseDetector::new(4096);
/// let data = vec![0xAA; 100];
/// let regions = detector.scan(&data, 0);
///
/// assert_eq!(regions.len(), 1);
/// assert!(matches!(regions[0], SparseRegion::Data { offset: 0, length: 100 }));
/// ```
pub struct SparseDetector {
    min_hole_size: usize,
}

impl SparseDetector {
    /// Creates a new sparse detector with the specified minimum hole size.
    ///
    /// Runs of zeros shorter than `min_hole_size` will not be considered holes
    /// and will be treated as regular data instead. This reduces overhead for
    /// small zero runs while still efficiently handling large sparse regions.
    ///
    /// # Arguments
    ///
    /// * `min_hole_size` - Minimum number of consecutive zero bytes to treat as a hole
    pub const fn new(min_hole_size: usize) -> Self {
        Self { min_hole_size }
    }

    /// Creates a detector with the default threshold matching rsync's behavior.
    pub const fn default_threshold() -> Self {
        Self::new(SPARSE_WRITE_SIZE)
    }

    /// Scans a data buffer and returns a list of sparse regions.
    ///
    /// The buffer is analyzed to identify contiguous runs of zeros (potential holes)
    /// and non-zero data regions. Only zero runs at least `min_hole_size` bytes long
    /// are reported as holes.
    ///
    /// # Arguments
    ///
    /// * `data` - The data buffer to scan
    /// * `base_offset` - The starting offset of this data in the file (used for region offsets)
    ///
    /// # Returns
    ///
    /// A vector of `SparseRegion` entries describing the data and hole regions.
    /// An empty input buffer returns an empty vector.
    pub fn scan(&self, data: &[u8], base_offset: u64) -> Vec<SparseRegion> {
        if data.is_empty() {
            return Vec::new();
        }

        let mut regions = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            let remaining = &data[offset..];
            let zero_run_len = leading_zero_run(remaining);

            if zero_run_len >= self.min_hole_size {
                // Found a significant hole
                regions.push(SparseRegion::Hole {
                    offset: base_offset + offset as u64,
                    length: zero_run_len as u64,
                });
                offset += zero_run_len;
            } else if zero_run_len > 0 {
                // Small zero run - find the next significant zero run or end
                let data_start = offset;
                offset += zero_run_len;

                // Scan for next significant hole or end of buffer
                while offset < data.len() {
                    let segment = &data[offset..];
                    let next_zeros = leading_zero_run(segment);

                    if next_zeros >= self.min_hole_size {
                        // Found next hole, emit data region
                        break;
                    }

                    // Skip this small zero run and any following non-zero data
                    offset += next_zeros;
                    if offset < data.len() {
                        let non_zeros = segment[next_zeros..]
                            .iter()
                            .position(|&b| b == 0)
                            .unwrap_or(segment.len() - next_zeros);
                        offset += non_zeros;
                    }
                }

                // Emit the data region
                regions.push(SparseRegion::Data {
                    offset: base_offset + data_start as u64,
                    length: (offset - data_start) as u64,
                });
            } else {
                // No zeros at start, find first zero or end
                let non_zero_len = remaining
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(remaining.len());

                regions.push(SparseRegion::Data {
                    offset: base_offset + offset as u64,
                    length: non_zero_len as u64,
                });
                offset += non_zero_len;
            }
        }

        regions
    }

    /// Quickly checks if the entire buffer is all zeros.
    ///
    /// This is faster than calling `scan()` when you only need to know whether
    /// the buffer contains any non-zero data.
    ///
    /// # Arguments
    ///
    /// * `data` - The data buffer to check
    ///
    /// # Returns
    ///
    /// `true` if all bytes in the buffer are zero, `false` otherwise.
    /// An empty buffer returns `true`.
    pub fn is_all_zeros(data: &[u8]) -> bool {
        if data.is_empty() {
            return true;
        }
        leading_zero_run(data) == data.len()
    }
}

/// Reads sparse files efficiently using filesystem hole detection.
///
/// On Linux systems with filesystem support, this uses `SEEK_HOLE`/`SEEK_DATA`
/// to efficiently detect existing holes without reading zero-filled data.
/// On other platforms, it falls back to scanning file contents.
///
/// # Platform Support
///
/// - **Linux 3.1+**: Uses `lseek(SEEK_HOLE)` and `lseek(SEEK_DATA)` for efficient hole detection
/// - **Other platforms**: Falls back to reading and scanning file contents
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use engine::{SparseReader, SparseRegion};
///
/// let file = File::open("sparse_file.bin").unwrap();
/// let regions = SparseReader::detect_holes(&file).unwrap();
///
/// for region in regions {
///     match region {
///         SparseRegion::Data { offset, length } => {
///             println!("Data at {}: {} bytes", offset, length);
///         }
///         SparseRegion::Hole { offset, length } => {
///             println!("Hole at {}: {} bytes", offset, length);
///         }
///     }
/// }
/// ```
pub struct SparseReader;

impl SparseReader {
    /// Detects holes in a file using filesystem-specific mechanisms.
    ///
    /// On Linux with SEEK_HOLE/SEEK_DATA support, this efficiently queries the
    /// filesystem for hole locations without reading file contents. On other
    /// platforms, it falls back to reading and scanning the file.
    ///
    /// # Arguments
    ///
    /// * `file` - A reference to the file to analyze
    ///
    /// # Returns
    ///
    /// A vector of `SparseRegion` entries describing data and hole regions in
    /// the file, or an I/O error if the file cannot be read or queried.
    ///
    /// # Platform-Specific Behavior
    ///
    /// - **Linux**: Uses `SEEK_HOLE`/`SEEK_DATA` syscalls for efficient detection
    /// - **Other platforms**: Reads file in chunks and scans for zero runs
    #[cfg(target_os = "linux")]
    pub fn detect_holes(file: &fs::File) -> io::Result<Vec<SparseRegion>> {
        Self::detect_holes_linux(file)
    }

    /// Fallback hole detection for non-Linux platforms.
    #[cfg(not(target_os = "linux"))]
    pub fn detect_holes(file: &fs::File) -> io::Result<Vec<SparseRegion>> {
        Self::detect_holes_fallback(file)
    }

    /// Linux-specific hole detection using SEEK_HOLE and SEEK_DATA.
    #[cfg(target_os = "linux")]
    fn detect_holes_linux(file: &fs::File) -> io::Result<Vec<SparseRegion>> {
        use rustix::fs::SeekFrom as RustixSeekFrom;
        use rustix::io::Errno;

        let mut regions = Vec::new();
        let file_size = file.metadata()?.len();

        if file_size == 0 {
            return Ok(regions);
        }

        let fd = file.as_fd();
        let mut pos = 0u64;

        while pos < file_size {
            // Seek to next data region
            match rustix::fs::seek(fd, RustixSeekFrom::Data(pos as i64)) {
                Ok(data_start) => {
                    // If there was a hole before this data, record it
                    if data_start > pos {
                        regions.push(SparseRegion::Hole {
                            offset: pos,
                            length: data_start - pos,
                        });
                    }

                    // Seek to next hole after this data
                    match rustix::fs::seek(fd, RustixSeekFrom::Hole(data_start as i64)) {
                        Ok(hole_start) => {
                            // Record the data region
                            if hole_start > data_start {
                                regions.push(SparseRegion::Data {
                                    offset: data_start,
                                    length: hole_start - data_start,
                                });
                            }

                            pos = hole_start;
                        }
                        Err(Errno::NXIO) => {
                            // No more holes - rest of file is data
                            regions.push(SparseRegion::Data {
                                offset: data_start,
                                length: file_size - data_start,
                            });
                            break;
                        }
                        Err(_e) => {
                            // SEEK_HOLE not supported or other error - fall back
                            return Self::detect_holes_fallback(file);
                        }
                    }
                }
                Err(Errno::NXIO) => {
                    // No more data - rest of file is a hole
                    if pos < file_size {
                        regions.push(SparseRegion::Hole {
                            offset: pos,
                            length: file_size - pos,
                        });
                    }
                    break;
                }
                Err(_e) => {
                    // SEEK_DATA not supported or other error - fall back
                    return Self::detect_holes_fallback(file);
                }
            }
        }

        Ok(regions)
    }

    /// Fallback hole detection by reading and scanning file contents.
    ///
    /// This is used on platforms without SEEK_HOLE/SEEK_DATA support or when
    /// those operations fail. It reads the file in chunks and uses
    /// `SparseDetector` to identify zero runs.
    fn detect_holes_fallback(file: &fs::File) -> io::Result<Vec<SparseRegion>> {
        use std::io::Read;

        let file_size = file.metadata()?.len();
        if file_size == 0 {
            return Ok(Vec::new());
        }

        let mut file_clone = file.try_clone()?;
        file_clone.seek(SeekFrom::Start(0))?;

        let detector = SparseDetector::new(SPARSE_WRITE_SIZE);
        let mut all_regions = Vec::new();
        let mut buffer = vec![0u8; 1024 * 1024]; // 1MB chunks
        let mut offset = 0u64;

        loop {
            let bytes_read = file_clone.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }

            let chunk_regions = detector.scan(&buffer[..bytes_read], offset);
            all_regions.extend(chunk_regions);
            offset += bytes_read as u64;
        }

        // Coalesce adjacent regions of the same type
        Self::coalesce_regions(&mut all_regions);

        Ok(all_regions)
    }

    /// Coalesces adjacent regions of the same type.
    ///
    /// If two Data regions or two Hole regions are adjacent, they are merged
    /// into a single region to simplify the region list.
    fn coalesce_regions(regions: &mut Vec<SparseRegion>) {
        if regions.len() < 2 {
            return;
        }

        let mut write_idx = 0;
        let mut read_idx = 1;

        while read_idx < regions.len() {
            let can_merge = match (regions[write_idx], regions[read_idx]) {
                (
                    SparseRegion::Data {
                        offset: o1,
                        length: l1,
                    },
                    SparseRegion::Data {
                        offset: o2,
                        length: _,
                    },
                ) if o1 + l1 == o2 => true,
                (
                    SparseRegion::Hole {
                        offset: o1,
                        length: l1,
                    },
                    SparseRegion::Hole {
                        offset: o2,
                        length: _,
                    },
                ) if o1 + l1 == o2 => true,
                _ => false,
            };

            if can_merge {
                // Merge regions[read_idx] into regions[write_idx]
                let merged = match (regions[write_idx], regions[read_idx]) {
                    (
                        SparseRegion::Data { offset, length: l1 },
                        SparseRegion::Data { length: l2, .. },
                    ) => SparseRegion::Data {
                        offset,
                        length: l1 + l2,
                    },
                    (
                        SparseRegion::Hole { offset, length: l1 },
                        SparseRegion::Hole { length: l2, .. },
                    ) => SparseRegion::Hole {
                        offset,
                        length: l1 + l2,
                    },
                    _ => unreachable!(),
                };
                regions[write_idx] = merged;
            } else {
                write_idx += 1;
                regions[write_idx] = regions[read_idx];
            }

            read_idx += 1;
        }

        regions.truncate(write_idx + 1);
    }
}

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
            // For sparse mode, seek and write with sparse handling
            self.file
                .seek(SeekFrom::Start(offset))
                .map_err(|e| io::Error::new(e.kind(), format!("seek to offset {offset}: {e}")))?;

            let path = std::path::Path::new(""); // Path only used for error messages
            write_sparse_chunk(&mut self.file, &mut self.state, data, path)
                .map_err(io::Error::other)?;
        } else {
            // Dense write - seek to offset and write
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

/// Punches a hole in the file at the specified position for the given length.
///
/// Mirrors upstream rsync's `do_punch_hole()` function (syscall.c) with a
/// three-tier fallback strategy:
///
/// 1. Try `FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE` - creates actual hole
/// 2. Fall back to `FALLOC_FL_ZERO_RANGE` - zeroes range without allocation
/// 3. Final fallback: write zeros - universal but dense
///
/// After a successful call, the file position will be at `pos + len`.
///
/// # Arguments
///
/// * `file` - The file to punch holes in
/// * `path` - Path for error reporting
/// * `pos` - Starting position for the hole
/// * `len` - Length of the hole in bytes
///
/// # TODO
/// Currently only used in tests. Will be used for delta transfer in-place updates.
#[cfg(target_os = "linux")]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn punch_hole(
    file: &mut fs::File,
    path: &Path,
    pos: u64,
    len: u64,
) -> Result<(), LocalCopyError> {
    if len == 0 {
        return Ok(());
    }

    // Ensure position doesn't exceed i64::MAX for fallocate
    if pos > i64::MAX as u64 || len > i64::MAX as u64 {
        return write_zeros_fallback(file, path, len);
    }

    let fd = file.as_fd();

    // Strategy 1: Try PUNCH_HOLE | KEEP_SIZE (creates actual filesystem hole)
    let punch_flags = FallocateFlags::PUNCH_HOLE | FallocateFlags::KEEP_SIZE;
    match fallocate(fd, punch_flags, pos, len) {
        Ok(()) => {
            // Seek to pos + len after successful hole punch
            file.seek(SeekFrom::Start(pos + len))
                .map_err(|e| LocalCopyError::io("seek after hole punch", path, e))?;
            return Ok(());
        }
        Err(Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => {
            // PUNCH_HOLE not supported, try ZERO_RANGE
        }
        Err(_errno) => {
            // Unexpected error, fall through to ZERO_RANGE fallback
        }
    }

    // Strategy 2: Try ZERO_RANGE (zeroes range without allocation on some systems)
    match fallocate(fd, FallocateFlags::ZERO_RANGE, pos, len) {
        Ok(()) => {
            // Seek to pos + len after successful zero range
            file.seek(SeekFrom::Start(pos + len))
                .map_err(|e| LocalCopyError::io("seek after zero range", path, e))?;
            return Ok(());
        }
        Err(Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => {
            // ZERO_RANGE not supported, fall back to writing zeros
        }
        Err(_errno) => {
            // Unexpected error, fall through to write zeros
        }
    }

    // Strategy 3: Write zeros (universal but allocates space)
    write_zeros_fallback(file, path, len)
}

/// Non-Linux platforms fall back to writing zeros directly.
/// This includes macOS, BSD, and Windows which don't support Linux's
/// fallocate PUNCH_HOLE/ZERO_RANGE flags.
///
/// After a successful call, the file position will be at `pos + len`,
/// matching the Linux implementation's behavior.
///
/// # TODO
/// Currently only used in tests. Will be used for delta transfer in-place updates.
#[cfg(not(target_os = "linux"))]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn punch_hole(
    file: &mut fs::File,
    path: &Path,
    pos: u64,
    len: u64,
) -> Result<(), LocalCopyError> {
    if len == 0 {
        return Ok(());
    }

    // Seek to the starting position before writing zeros
    file.seek(SeekFrom::Start(pos))
        .map_err(|e| LocalCopyError::io("seek before writing zeros", path, e))?;

    write_zeros_fallback(file, path, len)
}

/// Writes zeros to fill the specified length.
///
/// This is the final fallback when fallocate-based hole punching is not
/// available. Unlike hole punching, this allocates disk space.
fn write_zeros_fallback(
    file: &mut fs::File,
    path: &Path,
    mut len: u64,
) -> Result<(), LocalCopyError> {
    let zeros = [0u8; ZERO_WRITE_BUFFER_SIZE];

    while len > 0 {
        let chunk_size = len.min(ZERO_WRITE_BUFFER_SIZE as u64) as usize;
        file.write_all(&zeros[..chunk_size])
            .map_err(|e| LocalCopyError::io("write zeros for sparse hole", path, e))?;
        len -= chunk_size as u64;
    }

    Ok(())
}

/// Tracks pending zero runs during sparse file writing.
///
/// This struct accumulates consecutive zero bytes and flushes them either
/// by seeking (for new files) or by punching holes (for in-place updates).
#[derive(Default)]
pub(crate) struct SparseWriteState {
    pending_zero_run: u64,
    /// Position where the pending zero run starts (used for punch_hole path)
    ///
    /// TODO: Will be used for delta transfer in-place updates via flush_with_punch_hole
    #[cfg_attr(not(test), allow(dead_code))]
    zero_run_start_pos: u64,
}

impl SparseWriteState {
    const fn accumulate(&mut self, additional: usize) {
        self.pending_zero_run = self.pending_zero_run.saturating_add(additional as u64);
    }

    /// Flushes pending zeros by seeking forward.
    ///
    /// This is the default strategy for new files where the filesystem
    /// automatically creates sparse regions when seeking past end of file.
    fn flush(&mut self, writer: &mut fs::File, destination: &Path) -> Result<(), LocalCopyError> {
        if self.pending_zero_run == 0 {
            return Ok(());
        }

        let mut remaining = self.pending_zero_run;
        while remaining > 0 {
            let step = remaining.min(i64::MAX as u64);
            writer
                .seek(SeekFrom::Current(step as i64))
                .map_err(|error| {
                    LocalCopyError::io("seek in destination file", destination, error)
                })?;
            remaining -= step;
        }

        self.pending_zero_run = 0;
        Ok(())
    }

    /// Flushes pending zeros by punching a hole in the file.
    ///
    /// This is used for in-place updates where we need to deallocate
    /// disk blocks. Falls back to writing zeros if hole punching is
    /// not supported.
    ///
    /// # TODO
    /// Currently only used in tests. Will be used for delta transfer in-place updates.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn flush_with_punch_hole(
        &mut self,
        writer: &mut fs::File,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        if self.pending_zero_run == 0 {
            return Ok(());
        }

        let pos = self.zero_run_start_pos;
        let len = self.pending_zero_run;

        punch_hole(writer, destination, pos, len)?;

        self.pending_zero_run = 0;
        Ok(())
    }

    const fn replace(&mut self, next_run: usize) {
        self.pending_zero_run = next_run as u64;
    }

    /// Updates the starting position for the next zero run.
    ///
    /// # TODO
    /// Currently only used in tests. Will be used for delta transfer in-place updates.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn set_zero_run_start(&mut self, pos: u64) {
        self.zero_run_start_pos = pos;
    }

    /// Returns the pending zero run length.
    ///
    /// # TODO
    /// Currently only used in tests. Will be used for delta transfer in-place updates.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn pending_zeros(&self) -> u64 {
        self.pending_zero_run
    }

    /// Finishes sparse writing by flushing any remaining zeros via seeking.
    pub(crate) fn finish(
        &mut self,
        writer: &mut fs::File,
        destination: &Path,
    ) -> Result<u64, LocalCopyError> {
        self.flush(writer, destination)?;

        writer
            .stream_position()
            .map_err(|error| LocalCopyError::io("seek in destination file", destination, error))
    }

    /// Finishes sparse writing by punching holes for any remaining zeros.
    ///
    /// Use this variant when updating files in-place to deallocate disk
    /// blocks for zero regions.
    ///
    /// # TODO
    /// Currently only used in tests. Will be used for delta transfer in-place updates.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn finish_with_punch_hole(
        &mut self,
        writer: &mut fs::File,
        destination: &Path,
    ) -> Result<u64, LocalCopyError> {
        self.flush_with_punch_hole(writer, destination)?;

        writer
            .stream_position()
            .map_err(|error| LocalCopyError::io("seek in destination file", destination, error))
    }
}

pub(crate) fn write_sparse_chunk(
    writer: &mut fs::File,
    state: &mut SparseWriteState,
    chunk: &[u8],
    destination: &Path,
) -> Result<usize, LocalCopyError> {
    // Mirror rsync's write_sparse: always report the full chunk length as
    // consumed even when large sections become holes. Callers that track
    // literal bytes should account for sparseness separately.
    if chunk.is_empty() {
        return Ok(0);
    }

    let mut offset = 0usize;

    while offset < chunk.len() {
        let segment_end = (offset + SPARSE_WRITE_SIZE).min(chunk.len());
        let segment = &chunk[offset..segment_end];

        let leading = leading_zero_run(segment);
        state.accumulate(leading);

        if leading == segment.len() {
            offset = segment_end;
            continue;
        }

        let trailing = trailing_zero_run(&segment[leading..]);
        let data_start = offset + leading;
        let data_end = segment_end - trailing;

        if data_end > data_start {
            state.flush(writer, destination)?;
            writer
                .write_all(&chunk[data_start..data_end])
                .map_err(|error| LocalCopyError::io("copy file", destination, error))?;
        }

        state.replace(trailing);
        offset = segment_end;
    }

    Ok(chunk.len())
}

#[inline]
fn leading_zero_run(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    let mut iter = bytes.chunks_exact(16);

    for chunk in &mut iter {
        // SAFETY: chunks_exact(16) guarantees exactly 16-byte slices, so try_into cannot fail.
        let chunk: &[u8; 16] = chunk.try_into().expect("chunks_exact guarantees 16 bytes");
        if u128::from_ne_bytes(*chunk) == 0 {
            offset += 16;
            continue;
        }

        let position = chunk.iter().position(|&byte| byte != 0).unwrap_or(16);
        return offset + position;
    }

    offset + leading_zero_run_scalar(iter.remainder())
}

#[inline]
fn leading_zero_run_scalar(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|&&byte| byte == 0).count()
}

#[inline]
fn trailing_zero_run(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    let mut iter = bytes.rchunks_exact(16);

    for chunk in &mut iter {
        // SAFETY: rchunks_exact(16) guarantees exactly 16-byte slices, so try_into cannot fail.
        let chunk: &[u8; 16] = chunk.try_into().expect("chunks_exact guarantees 16 bytes");
        if u128::from_ne_bytes(*chunk) == 0 {
            offset += 16;
            continue;
        }

        let trailing = chunk.iter().rev().take_while(|&&byte| byte == 0).count();
        return offset + trailing;
    }

    offset + trailing_zero_run_scalar(iter.remainder())
}

#[inline]
fn trailing_zero_run_scalar(bytes: &[u8]) -> usize {
    bytes.iter().rev().take_while(|&&byte| byte == 0).count()
}

#[cfg(test)]
mod tests {
    use super::{
        SparseWriteState, leading_zero_run, leading_zero_run_scalar, punch_hole, trailing_zero_run,
        trailing_zero_run_scalar, write_sparse_chunk, write_zeros_fallback,
    };
    use std::fs;
    use std::io::{Read, Seek, SeekFrom, Write};
    use tempfile::NamedTempFile;

    #[test]
    fn leading_zero_run_matches_scalar_reference() {
        let cases: &[&[u8]] = &[
            &[],
            &[0],
            &[0, 0, 0],
            &[0, 0, 1, 0, 0],
            &[0, 7, 0, 0, 0],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0, 1],
        ];

        for case in cases {
            assert_eq!(
                leading_zero_run(case),
                leading_zero_run_scalar(case),
                "leading zero-run length mismatch for {case:?}"
            );
        }

        let mut long = vec![0u8; 512];
        assert_eq!(leading_zero_run(&long), long.len());
        long[511] = 42;
        assert_eq!(leading_zero_run(&long), 511);
        long.push(0);
        assert_eq!(leading_zero_run(&long[511..]), 0);
        assert_eq!(leading_zero_run(&long[512..]), 1);
    }

    #[test]
    fn trailing_zero_run_matches_scalar_reference() {
        let cases: &[&[u8]] = &[
            &[],
            &[0],
            &[0, 0, 0],
            &[0, 0, 1, 0, 0],
            &[0, 7, 0, 0, 0],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0, 1],
            &[1, 0],
            &[1, 2, 3, 0, 0, 0],
        ];

        for case in cases {
            assert_eq!(
                trailing_zero_run(case),
                trailing_zero_run_scalar(case),
                "trailing zero-run length mismatch for {case:?}"
            );
        }

        let mut long = vec![0u8; 512];
        assert_eq!(trailing_zero_run(&long), long.len());
        long[0] = 42;
        assert_eq!(trailing_zero_run(&long), 511);
        long.insert(0, 0);
        assert_eq!(trailing_zero_run(&long[..512]), 510);
    }

    #[test]
    fn sparse_writer_accumulates_zero_runs_across_chunks() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let first = [b'A', b'B', 0, 0, 0];
        let written_first =
            write_sparse_chunk(file.as_file_mut(), &mut state, &first, path.as_path())
                .expect("write first chunk");

        let second = [0, 0, b'C', b'D'];
        let written_second =
            write_sparse_chunk(file.as_file_mut(), &mut state, &second, path.as_path())
                .expect("write second chunk");

        assert_eq!(written_first, first.len());
        assert_eq!(written_second, second.len());

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finalise sparse writer");

        let total = (first.len() + second.len()) as u64;
        file.as_file_mut()
            .set_len(total)
            .expect("truncate file to final length");
        file.as_file_mut()
            .seek(SeekFrom::Start(0))
            .expect("rewind for verification");

        let mut buffer = vec![0u8; total as usize];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back contents");

        assert_eq!(&buffer[0..2], b"AB");
        assert!(buffer[2..7].iter().all(|&byte| byte == 0));
        assert_eq!(&buffer[7..9], b"CD");
    }

    #[test]
    fn sparse_writer_flushes_trailing_zero_run() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let chunk = [b'Z', 0, 0, 0, 0];
        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write chunk");
        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("flush trailing zeros");

        assert_eq!(written, chunk.len());

        file.as_file_mut()
            .set_len(chunk.len() as u64)
            .expect("truncate file");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0u8; chunk.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back data");

        assert_eq!(buffer[0], b'Z');
        assert!(buffer[1..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn sparse_writer_reports_zero_literal_bytes_for_all_zero_chunks() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let zeros = [0u8; 32];
        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &zeros, path.as_path())
            .expect("write zero chunk");

        assert_eq!(written, zeros.len());

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish sparse writer");

        file.as_file_mut()
            .set_len(zeros.len() as u64)
            .expect("truncate file");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![1u8; zeros.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back zeros");

        assert!(buffer.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn sparse_writer_skips_large_interior_zero_runs() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let mut chunk = vec![0u8; super::SPARSE_WRITE_SIZE * 2];
        chunk[0] = b'L';
        let last = super::SPARSE_WRITE_SIZE * 2 - 1;
        chunk[last] = b'R';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write sparse chunk");

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish sparse writer");

        file.as_file_mut()
            .set_len(chunk.len() as u64)
            .expect("truncate file");

        assert_eq!(written, chunk.len());

        file.as_file_mut()
            .seek(SeekFrom::Start(0))
            .expect("rewind for verification");
        let mut buffer = vec![0u8; chunk.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back data");

        assert_eq!(buffer[0], b'L');
        assert!(buffer[1..buffer.len() - 1].iter().all(|&byte| byte == 0));
        assert_eq!(buffer[buffer.len() - 1], b'R');
    }

    #[test]
    fn sparse_writer_writes_small_interior_zero_runs_dense() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let mut chunk = vec![0u8; super::SPARSE_WRITE_SIZE / 2];
        chunk[0] = b'L';
        let last = chunk.len() - 1;
        chunk[last] = b'R';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write sparse chunk");

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish sparse writer");

        file.as_file_mut()
            .set_len(chunk.len() as u64)
            .expect("truncate file");

        assert_eq!(written, chunk.len());

        file.as_file_mut()
            .seek(SeekFrom::Start(0))
            .expect("rewind for verification");
        let mut buffer = vec![0u8; chunk.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back data");

        assert_eq!(buffer[0], b'L');
        assert!(buffer[1..buffer.len() - 1].iter().all(|&byte| byte == 0));
        assert_eq!(buffer[buffer.len() - 1], b'R');
    }

    #[test]
    fn finish_reports_final_offset_after_trailing_zeros() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let chunk = [b'A', 0, 0, 0, 0];
        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write sparse chunk");

        assert_eq!(written, chunk.len());

        let final_offset = state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finalise sparse writer");

        assert_eq!(final_offset, chunk.len() as u64);

        file.as_file_mut()
            .set_len(final_offset)
            .expect("truncate to sparse length");

        let metadata = file.as_file_mut().metadata().expect("metadata");
        assert_eq!(metadata.len(), final_offset);
    }

    // ==================== punch_hole tests ====================

    #[test]
    fn punch_hole_zero_length_is_noop() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Write some data first
        file.as_file_mut()
            .write_all(b"test data")
            .expect("write data");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        // Punching a zero-length hole should succeed without changing anything
        punch_hole(file.as_file_mut(), &path, 0, 0).expect("punch zero-length hole");

        // File should be unchanged
        let mut buffer = vec![0u8; 9];
        file.as_file_mut().read_exact(&mut buffer).expect("read");
        assert_eq!(&buffer, b"test data");
    }

    #[test]
    fn punch_hole_creates_zeros_in_file() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Write some non-zero data
        let data = vec![0xAAu8; 4096];
        file.as_file_mut().write_all(&data).expect("write data");

        // Punch a hole in the middle
        punch_hole(file.as_file_mut(), &path, 1024, 2048).expect("punch hole");

        // Read back and verify the hole contains zeros
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
        let mut buffer = vec![0u8; 4096];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        // First 1024 bytes should be unchanged
        assert!(buffer[..1024].iter().all(|&b| b == 0xAA));
        // Middle 2048 bytes should be zeros (the hole)
        assert!(buffer[1024..3072].iter().all(|&b| b == 0));
        // Last 1024 bytes should be unchanged
        assert!(buffer[3072..].iter().all(|&b| b == 0xAA));
    }

    #[test]
    fn punch_hole_advances_file_position() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Pre-allocate file
        file.as_file_mut().set_len(8192).expect("set length");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        // Punch a hole starting at position 1000
        punch_hole(file.as_file_mut(), &path, 1000, 500).expect("punch hole");

        // File position should now be at 1500
        let pos = file.as_file_mut().stream_position().expect("position");
        assert_eq!(pos, 1500);
    }

    // ==================== write_zeros_fallback tests ====================

    #[test]
    fn write_zeros_fallback_writes_exact_length() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        write_zeros_fallback(file.as_file_mut(), &path, 1234).expect("write zeros");

        let metadata = file.as_file_mut().metadata().expect("metadata");
        assert_eq!(metadata.len(), 1234);

        // Verify all bytes are zero
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
        let mut buffer = vec![1u8; 1234];
        file.as_file_mut().read_exact(&mut buffer).expect("read");
        assert!(buffer.iter().all(|&b| b == 0));
    }

    #[test]
    fn write_zeros_fallback_handles_large_length() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Write more than one buffer's worth of zeros
        let len = super::ZERO_WRITE_BUFFER_SIZE as u64 * 3 + 123;
        write_zeros_fallback(file.as_file_mut(), &path, len).expect("write zeros");

        let metadata = file.as_file_mut().metadata().expect("metadata");
        assert_eq!(metadata.len(), len);
    }

    // ==================== SparseWriteState hole punching tests ====================

    #[test]
    fn sparse_state_flush_with_punch_hole() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Pre-allocate with non-zero data
        let data = vec![0xBBu8; 8192];
        file.as_file_mut().write_all(&data).expect("write");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut state = SparseWriteState::default();
        state.set_zero_run_start(1000);
        state.accumulate(2000);

        state
            .flush_with_punch_hole(file.as_file_mut(), &path)
            .expect("flush with punch");

        // Verify the hole was punched
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
        let mut buffer = vec![0u8; 8192];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        // Before the hole: unchanged
        assert!(buffer[..1000].iter().all(|&b| b == 0xBB));
        // The hole: zeros
        assert!(buffer[1000..3000].iter().all(|&b| b == 0));
        // After the hole: unchanged
        assert!(buffer[3000..].iter().all(|&b| b == 0xBB));
    }

    #[test]
    fn sparse_state_finish_with_punch_hole() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Pre-allocate
        file.as_file_mut().set_len(4096).expect("set length");

        let mut state = SparseWriteState::default();
        state.set_zero_run_start(500);
        state.accumulate(1000);

        let final_pos = state
            .finish_with_punch_hole(file.as_file_mut(), &path)
            .expect("finish with punch");

        // Position should be at end of punched hole
        assert_eq!(final_pos, 1500);
    }

    #[test]
    fn sparse_state_pending_zeros_tracks_accumulation() {
        let mut state = SparseWriteState::default();
        assert_eq!(state.pending_zeros(), 0);

        state.accumulate(100);
        assert_eq!(state.pending_zeros(), 100);

        state.accumulate(200);
        assert_eq!(state.pending_zeros(), 300);
    }

    // ==================== Boundary Condition Tests ====================

    #[test]
    fn sparse_writer_exactly_sparse_write_size() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Chunk exactly at SPARSE_WRITE_SIZE boundary
        let mut chunk = vec![0u8; super::SPARSE_WRITE_SIZE];
        chunk[0] = b'S';
        chunk[super::SPARSE_WRITE_SIZE - 1] = b'E';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write exactly SPARSE_WRITE_SIZE");

        assert_eq!(written, super::SPARSE_WRITE_SIZE);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut()
            .set_len(chunk.len() as u64)
            .expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0u8; chunk.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back");

        assert_eq!(buffer[0], b'S');
        assert_eq!(buffer[super::SPARSE_WRITE_SIZE - 1], b'E');
    }

    #[test]
    fn sparse_writer_just_under_sparse_write_size() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // One byte under the threshold - should write densely
        let mut chunk = vec![0u8; super::SPARSE_WRITE_SIZE - 1];
        chunk[0] = b'A';
        chunk[super::SPARSE_WRITE_SIZE - 2] = b'Z';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write just under SPARSE_WRITE_SIZE");

        assert_eq!(written, super::SPARSE_WRITE_SIZE - 1);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut()
            .set_len(chunk.len() as u64)
            .expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0u8; chunk.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back");

        assert_eq!(buffer[0], b'A');
        assert_eq!(buffer[super::SPARSE_WRITE_SIZE - 2], b'Z');
    }

    #[test]
    fn sparse_writer_just_over_sparse_write_size() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // One byte over the threshold - should potentially use sparse writes
        let mut chunk = vec![0u8; super::SPARSE_WRITE_SIZE + 1];
        chunk[0] = b'X';
        chunk[super::SPARSE_WRITE_SIZE] = b'Y';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write just over SPARSE_WRITE_SIZE");

        assert_eq!(written, super::SPARSE_WRITE_SIZE + 1);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut()
            .set_len(chunk.len() as u64)
            .expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0u8; chunk.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back");

        assert_eq!(buffer[0], b'X');
        assert_eq!(buffer[super::SPARSE_WRITE_SIZE], b'Y');
    }

    // ==================== Data Pattern Tests ====================

    #[test]
    fn sparse_writer_leading_zeros_only() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Only leading zeros followed by data at the end
        let mut chunk = vec![0u8; 2048];
        chunk[2047] = b'X';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write leading zeros");

        assert_eq!(written, 2048);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut()
            .set_len(chunk.len() as u64)
            .expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![1u8; chunk.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back");

        assert!(buffer[..2047].iter().all(|&b| b == 0));
        assert_eq!(buffer[2047], b'X');
    }

    #[test]
    fn sparse_writer_trailing_zeros_only() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Data at start followed by only trailing zeros
        let mut chunk = vec![0u8; 2048];
        chunk[0] = b'Y';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write trailing zeros");

        assert_eq!(written, 2048);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut()
            .set_len(chunk.len() as u64)
            .expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![1u8; chunk.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back");

        assert_eq!(buffer[0], b'Y');
        assert!(buffer[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn sparse_writer_single_byte_surrounded_by_zeros() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Single non-zero byte in the middle of zeros
        let mut chunk = vec![0u8; 4096];
        chunk[2048] = b'M';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write single byte");

        assert_eq!(written, 4096);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut()
            .set_len(chunk.len() as u64)
            .expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![1u8; chunk.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back");

        assert!(buffer[..2048].iter().all(|&b| b == 0));
        assert_eq!(buffer[2048], b'M');
        assert!(buffer[2049..].iter().all(|&b| b == 0));
    }

    // ==================== State Machine Tests ====================

    #[test]
    fn sparse_state_replace_vs_accumulate() {
        let mut state = SparseWriteState::default();

        // Accumulate some zeros
        state.set_zero_run_start(100);
        state.accumulate(500);
        assert_eq!(state.pending_zeros(), 500);

        // Replace resets to new value
        state.replace(200);
        assert_eq!(state.pending_zeros(), 200);

        // Further accumulation adds to the replacement value
        state.accumulate(100);
        assert_eq!(state.pending_zeros(), 300);
    }

    #[test]
    fn sparse_state_zero_run_start_tracking() {
        let mut state = SparseWriteState::default();

        state.set_zero_run_start(1000);
        state.accumulate(500);

        // Zero run start should be preserved
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        file.as_file_mut().set_len(8192).expect("set length");

        let final_pos = state
            .finish_with_punch_hole(file.as_file_mut(), &path)
            .expect("finish");

        // Final position should be start + accumulated
        assert_eq!(final_pos, 1500);
    }

    #[test]
    fn sparse_state_multiple_flush_cycles() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Pre-allocate
        let data = vec![0xCCu8; 16384];
        file.as_file_mut().write_all(&data).expect("write");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut state = SparseWriteState::default();

        // First flush cycle
        state.set_zero_run_start(1000);
        state.accumulate(500);
        state
            .flush_with_punch_hole(file.as_file_mut(), &path)
            .expect("first flush");
        assert_eq!(state.pending_zeros(), 0);

        // Second flush cycle
        state.set_zero_run_start(5000);
        state.accumulate(1000);
        state
            .flush_with_punch_hole(file.as_file_mut(), &path)
            .expect("second flush");
        assert_eq!(state.pending_zeros(), 0);

        // Verify both holes were punched
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
        let mut buffer = vec![0u8; 16384];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        // First hole: 1000-1500
        assert!(buffer[..1000].iter().all(|&b| b == 0xCC));
        assert!(buffer[1000..1500].iter().all(|&b| b == 0));
        assert!(buffer[1500..5000].iter().all(|&b| b == 0xCC));
        // Second hole: 5000-6000
        assert!(buffer[5000..6000].iter().all(|&b| b == 0));
        assert!(buffer[6000..].iter().all(|&b| b == 0xCC));
    }

    // ==================== Zero-Run Detection Edge Cases ====================

    #[test]
    fn leading_zero_run_single_byte() {
        assert_eq!(leading_zero_run(&[0]), 1);
        assert_eq!(leading_zero_run(&[1]), 0);
    }

    #[test]
    fn leading_zero_run_misaligned_16_bytes() {
        // Test with lengths that don't align to 16-byte SIMD boundaries
        for len in 1..=20 {
            let all_zeros = vec![0u8; len];
            assert_eq!(leading_zero_run(&all_zeros), len);

            let mut with_nonzero = vec![0u8; len];
            if len > 0 {
                with_nonzero[len - 1] = 1;
                assert_eq!(leading_zero_run(&with_nonzero), len - 1);
            }
        }
    }

    #[test]
    fn trailing_zero_run_single_byte() {
        assert_eq!(trailing_zero_run(&[0]), 1);
        assert_eq!(trailing_zero_run(&[1]), 0);
    }

    #[test]
    fn trailing_zero_run_misaligned_16_bytes() {
        // Test with lengths that don't align to 16-byte boundaries
        for len in 1..=20 {
            let all_zeros = vec![0u8; len];
            assert_eq!(trailing_zero_run(&all_zeros), len);

            let mut with_nonzero = vec![0u8; len];
            if len > 0 {
                with_nonzero[0] = 1;
                assert_eq!(trailing_zero_run(&with_nonzero), len - 1);
            }
        }
    }

    #[test]
    fn zero_run_at_16_byte_boundary() {
        // Test exactly 16 bytes (SIMD fast path boundary)
        let zeros_16 = vec![0u8; 16];
        assert_eq!(leading_zero_run(&zeros_16), 16);
        assert_eq!(trailing_zero_run(&zeros_16), 16);

        // 17 bytes (16 + 1)
        let mut data_17 = vec![0u8; 17];
        data_17[16] = 1;
        assert_eq!(leading_zero_run(&data_17), 16);

        let mut data_17_trail = vec![0u8; 17];
        data_17_trail[0] = 1;
        assert_eq!(trailing_zero_run(&data_17_trail), 16);
    }

    // ==================== Empty Input Tests ====================

    #[test]
    fn write_sparse_chunk_empty_chunk_returns_zero() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let empty: [u8; 0] = [];
        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &empty, path.as_path())
            .expect("write empty chunk");

        assert_eq!(written, 0);
        assert_eq!(state.pending_zeros(), 0);
    }

    #[test]
    fn leading_zero_run_empty_slice() {
        assert_eq!(leading_zero_run(&[]), 0);
    }

    #[test]
    fn trailing_zero_run_empty_slice() {
        assert_eq!(trailing_zero_run(&[]), 0);
    }

    // ==================== Flush Edge Cases ====================

    #[test]
    fn sparse_state_flush_with_zero_pending_is_noop() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Pre-write some data to have a position
        file.as_file_mut().write_all(b"test").expect("write");

        let mut state = SparseWriteState::default();
        assert_eq!(state.pending_zeros(), 0);

        // Flush with zero pending should do nothing
        state
            .flush(file.as_file_mut(), path.as_path())
            .expect("flush zero pending");

        // Position should be unchanged
        let pos = file.as_file_mut().stream_position().expect("position");
        assert_eq!(pos, 4);
    }

    #[test]
    fn sparse_state_flush_with_punch_hole_zero_pending_is_noop() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        file.as_file_mut().write_all(b"data").expect("write");

        let mut state = SparseWriteState::default();

        state
            .flush_with_punch_hole(file.as_file_mut(), &path)
            .expect("flush punch hole zero pending");

        let pos = file.as_file_mut().stream_position().expect("position");
        assert_eq!(pos, 4);
    }

    // ==================== Large Value Tests ====================

    #[test]
    fn sparse_state_accumulate_saturation() {
        let mut state = SparseWriteState::default();

        // Accumulate large values
        state.accumulate(usize::MAX);
        let first = state.pending_zeros();

        // Another accumulation should saturate, not overflow
        state.accumulate(usize::MAX);
        let second = state.pending_zeros();

        // Should saturate at u64::MAX (second >= first means no overflow)
        assert!(second >= first);
    }

    #[test]
    fn sparse_state_default_values() {
        let state = SparseWriteState::default();
        assert_eq!(state.pending_zeros(), 0);
    }

    // ==================== Multi-Segment Chunk Tests ====================

    #[test]
    fn write_sparse_chunk_multiple_segments_in_one_chunk() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Create a chunk larger than 2x SPARSE_WRITE_SIZE to test multiple iterations
        let size = super::SPARSE_WRITE_SIZE * 3;
        let mut chunk = vec![0u8; size];

        // Place data at start of each segment
        chunk[0] = b'1';
        chunk[super::SPARSE_WRITE_SIZE] = b'2';
        chunk[super::SPARSE_WRITE_SIZE * 2] = b'3';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write multi-segment chunk");

        assert_eq!(written, size);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(size as u64).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0u8; size];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert_eq!(buffer[0], b'1');
        assert_eq!(buffer[super::SPARSE_WRITE_SIZE], b'2');
        assert_eq!(buffer[super::SPARSE_WRITE_SIZE * 2], b'3');
    }

    #[test]
    fn write_sparse_chunk_non_aligned_size() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Size not aligned to SPARSE_WRITE_SIZE
        let size = super::SPARSE_WRITE_SIZE + 1234;
        let mut chunk = vec![0u8; size];
        chunk[0] = b'F';
        chunk[size - 1] = b'L';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write non-aligned chunk");

        assert_eq!(written, size);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(size as u64).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0u8; size];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert_eq!(buffer[0], b'F');
        assert_eq!(buffer[size - 1], b'L');
    }

    // ==================== Dense Data Tests ====================

    #[test]
    fn write_sparse_chunk_all_nonzero_data() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // All non-zero data - no sparse optimization possible
        let chunk = vec![0xFFu8; 4096];

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write dense chunk");

        assert_eq!(written, 4096);
        assert_eq!(state.pending_zeros(), 0);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0u8; 4096];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert!(buffer.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn write_sparse_chunk_alternating_pattern() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Alternating zero and non-zero bytes
        let mut chunk = vec![0u8; 256];
        for (i, byte) in chunk.iter_mut().enumerate() {
            if i % 2 == 0 {
                *byte = 0xAA;
            }
        }

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write alternating");

        assert_eq!(written, 256);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(256).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![1u8; 256];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert_eq!(buffer, chunk);
    }

    // ==================== Boundary Alignment Tests ====================

    #[test]
    fn leading_zero_run_at_multiple_of_16() {
        // Test 32, 48, 64 byte boundaries
        for multiplier in 2..=4 {
            let len = 16 * multiplier;
            let all_zeros = vec![0u8; len];
            assert_eq!(leading_zero_run(&all_zeros), len);

            // Non-zero at first position
            let mut first_nonzero = vec![0u8; len];
            first_nonzero[0] = 1;
            assert_eq!(leading_zero_run(&first_nonzero), 0);

            // Non-zero at last position
            let mut last_nonzero = vec![0u8; len];
            last_nonzero[len - 1] = 1;
            assert_eq!(leading_zero_run(&last_nonzero), len - 1);

            // Non-zero exactly at 16-byte boundary
            let mut boundary_nonzero = vec![0u8; len];
            boundary_nonzero[16] = 1;
            assert_eq!(leading_zero_run(&boundary_nonzero), 16);
        }
    }

    #[test]
    fn trailing_zero_run_at_multiple_of_16() {
        for multiplier in 2..=4 {
            let len = 16 * multiplier;
            let all_zeros = vec![0u8; len];
            assert_eq!(trailing_zero_run(&all_zeros), len);

            // Non-zero at last position
            let mut last_nonzero = vec![0u8; len];
            last_nonzero[len - 1] = 1;
            assert_eq!(trailing_zero_run(&last_nonzero), 0);

            // Non-zero at first position
            let mut first_nonzero = vec![0u8; len];
            first_nonzero[0] = 1;
            assert_eq!(trailing_zero_run(&first_nonzero), len - 1);

            // Non-zero exactly at 16-byte boundary from end
            let mut boundary_nonzero = vec![0u8; len];
            boundary_nonzero[len - 17] = 1;
            assert_eq!(trailing_zero_run(&boundary_nonzero), 16);
        }
    }

    #[test]
    fn zero_run_with_interior_nonzero() {
        // Test case where non-zero byte is in middle of 16-byte chunk
        let mut data = vec![0u8; 32];
        data[8] = 0xFF; // Middle of first 16-byte chunk

        assert_eq!(leading_zero_run(&data), 8);
        assert_eq!(trailing_zero_run(&data), 23);
    }

    // ==================== Write Zeros Fallback Tests ====================

    #[test]
    fn write_zeros_fallback_zero_length() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Zero length should succeed but write nothing
        write_zeros_fallback(file.as_file_mut(), &path, 0).expect("write zero length");

        let metadata = file.as_file_mut().metadata().expect("metadata");
        assert_eq!(metadata.len(), 0);
    }

    #[test]
    fn write_zeros_fallback_smaller_than_buffer() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Less than ZERO_WRITE_BUFFER_SIZE
        let small_size = 100u64;
        write_zeros_fallback(file.as_file_mut(), &path, small_size).expect("write small");

        let metadata = file.as_file_mut().metadata().expect("metadata");
        assert_eq!(metadata.len(), small_size);

        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
        let mut buffer = vec![1u8; small_size as usize];
        file.as_file_mut().read_exact(&mut buffer).expect("read");
        assert!(buffer.iter().all(|&b| b == 0));
    }

    #[test]
    fn write_zeros_fallback_exact_buffer_size() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Exactly ZERO_WRITE_BUFFER_SIZE
        let exact_size = super::ZERO_WRITE_BUFFER_SIZE as u64;
        write_zeros_fallback(file.as_file_mut(), &path, exact_size).expect("write exact");

        let metadata = file.as_file_mut().metadata().expect("metadata");
        assert_eq!(metadata.len(), exact_size);
    }

    // ==================== Punch Hole Tests ====================

    #[test]
    fn punch_hole_at_file_end() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Pre-allocate file
        file.as_file_mut().set_len(8192).expect("set length");

        // Punch hole at the end
        punch_hole(file.as_file_mut(), &path, 4096, 4096).expect("punch at end");

        // Verify final position
        let pos = file.as_file_mut().stream_position().expect("position");
        assert_eq!(pos, 8192);
    }

    #[test]
    fn punch_hole_at_file_start() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Write non-zero data
        let data = vec![0xDDu8; 4096];
        file.as_file_mut().write_all(&data).expect("write");

        // Punch hole at position 0
        punch_hole(file.as_file_mut(), &path, 0, 2048).expect("punch at start");

        // Verify hole is zeroed
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
        let mut buffer = vec![0xFFu8; 4096];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert!(buffer[..2048].iter().all(|&b| b == 0));
        assert!(buffer[2048..].iter().all(|&b| b == 0xDD));
    }

    #[test]
    fn punch_hole_entire_file() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Write non-zero data
        let data = vec![0xEEu8; 8192];
        file.as_file_mut().write_all(&data).expect("write");

        // Punch hole for entire file
        punch_hole(file.as_file_mut(), &path, 0, 8192).expect("punch entire file");

        // Verify all zeros
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
        let mut buffer = vec![0xFFu8; 8192];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert!(buffer.iter().all(|&b| b == 0));
    }

    #[test]
    fn punch_hole_small_length() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Write data
        let data = vec![0xCCu8; 1024];
        file.as_file_mut().write_all(&data).expect("write");

        // Punch a small hole (1 byte)
        punch_hole(file.as_file_mut(), &path, 512, 1).expect("punch 1 byte");

        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
        let mut buffer = vec![0xFFu8; 1024];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert!(buffer[..512].iter().all(|&b| b == 0xCC));
        assert_eq!(buffer[512], 0);
        assert!(buffer[513..].iter().all(|&b| b == 0xCC));
    }

    // ==================== Finish Method Tests ====================

    #[test]
    fn sparse_state_finish_returns_correct_position() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Write some data
        file.as_file_mut().write_all(b"hello").expect("write");

        // Accumulate zeros
        state.accumulate(100);

        let final_pos = state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        // Should be initial position (5) + pending zeros (100)
        assert_eq!(final_pos, 105);
    }

    #[test]
    fn sparse_state_finish_no_pending() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        file.as_file_mut().write_all(b"data").expect("write");

        let final_pos = state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish no pending");

        assert_eq!(final_pos, 4);
    }

    #[test]
    fn sparse_state_finish_with_punch_hole_returns_position() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Pre-allocate
        file.as_file_mut().set_len(10000).expect("set length");

        let mut state = SparseWriteState::default();
        state.set_zero_run_start(1000);
        state.accumulate(500);

        let final_pos = state
            .finish_with_punch_hole(file.as_file_mut(), &path)
            .expect("finish punch");

        assert_eq!(final_pos, 1500);
    }

    // ==================== Chunked Write Integration ====================

    #[test]
    fn write_sparse_chunk_sequential_calls() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // First call: data + zeros
        let chunk1 = [b'A', b'B', 0, 0];
        write_sparse_chunk(file.as_file_mut(), &mut state, &chunk1, path.as_path())
            .expect("write chunk1");

        // Second call: continues zeros then data
        let chunk2 = [0, 0, b'C', b'D'];
        write_sparse_chunk(file.as_file_mut(), &mut state, &chunk2, path.as_path())
            .expect("write chunk2");

        // Third call: more data
        let chunk3 = [b'E', b'F'];
        write_sparse_chunk(file.as_file_mut(), &mut state, &chunk3, path.as_path())
            .expect("write chunk3");

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        let total = (chunk1.len() + chunk2.len() + chunk3.len()) as u64;
        file.as_file_mut().set_len(total).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; total as usize];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert_eq!(&buffer[0..2], b"AB");
        assert!(buffer[2..6].iter().all(|&b| b == 0));
        assert_eq!(&buffer[6..8], b"CD");
        assert_eq!(&buffer[8..10], b"EF");
    }

    #[test]
    fn write_sparse_chunk_all_zeros_followed_by_data() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // All zeros
        let zeros = [0u8; 100];
        write_sparse_chunk(file.as_file_mut(), &mut state, &zeros, path.as_path())
            .expect("write zeros");

        // Then data
        let data = [b'X', b'Y', b'Z'];
        write_sparse_chunk(file.as_file_mut(), &mut state, &data, path.as_path())
            .expect("write data");

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        let total = (zeros.len() + data.len()) as u64;
        file.as_file_mut().set_len(total).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; total as usize];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert!(buffer[..100].iter().all(|&b| b == 0));
        assert_eq!(&buffer[100..103], b"XYZ");
    }

    // ==================== SIMD Fast Path Verification ====================

    #[test]
    fn leading_zero_run_exercises_simd_path() {
        // 48 bytes = 3 full 16-byte chunks (exercises SIMD fast path)
        let all_zeros_48 = vec![0u8; 48];
        assert_eq!(leading_zero_run(&all_zeros_48), 48);

        // Non-zero in third 16-byte chunk
        let mut data = vec![0u8; 48];
        data[32] = 1;
        assert_eq!(leading_zero_run(&data), 32);

        // Non-zero in second 16-byte chunk
        let mut data = vec![0u8; 48];
        data[20] = 1;
        assert_eq!(leading_zero_run(&data), 20);

        // Non-zero in first 16-byte chunk
        let mut data = vec![0u8; 48];
        data[5] = 1;
        assert_eq!(leading_zero_run(&data), 5);
    }

    #[test]
    fn trailing_zero_run_exercises_simd_path() {
        // 48 bytes = 3 full 16-byte chunks (exercises SIMD fast path)
        let all_zeros_48 = vec![0u8; 48];
        assert_eq!(trailing_zero_run(&all_zeros_48), 48);

        // Non-zero in first 16-byte chunk (trailing considers from end)
        let mut data = vec![0u8; 48];
        data[10] = 1;
        assert_eq!(trailing_zero_run(&data), 37); // 48 - 10 - 1

        // Non-zero in second 16-byte chunk from end
        let mut data = vec![0u8; 48];
        data[25] = 1;
        assert_eq!(trailing_zero_run(&data), 22); // 48 - 25 - 1

        // Non-zero at very end
        let mut data = vec![0u8; 48];
        data[47] = 1;
        assert_eq!(trailing_zero_run(&data), 0);
    }

    // ==================== SPARSE_WRITE_SIZE Boundary Tests ====================

    #[test]
    fn write_sparse_chunk_multiple_of_sparse_write_size() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Exactly 2x SPARSE_WRITE_SIZE
        let size = super::SPARSE_WRITE_SIZE * 2;
        let mut chunk = vec![0u8; size];
        chunk[0] = b'A';
        chunk[size - 1] = b'Z';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write 2x sparse_write_size");

        assert_eq!(written, size);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(size as u64).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; size];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert_eq!(buffer[0], b'A');
        assert!(buffer[1..size - 1].iter().all(|&b| b == 0));
        assert_eq!(buffer[size - 1], b'Z');
    }

    // ==================== Data at Segment Boundaries ====================

    #[test]
    fn write_sparse_chunk_data_at_segment_boundary() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Data exactly at SPARSE_WRITE_SIZE - 1 boundary
        let size = super::SPARSE_WRITE_SIZE + 10;
        let mut chunk = vec![0u8; size];
        chunk[super::SPARSE_WRITE_SIZE - 1] = b'B';
        chunk[super::SPARSE_WRITE_SIZE] = b'C';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write boundary data");

        assert_eq!(written, size);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(size as u64).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; size];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert!(
            buffer[..super::SPARSE_WRITE_SIZE - 1]
                .iter()
                .all(|&b| b == 0)
        );
        assert_eq!(buffer[super::SPARSE_WRITE_SIZE - 1], b'B');
        assert_eq!(buffer[super::SPARSE_WRITE_SIZE], b'C');
        assert!(
            buffer[super::SPARSE_WRITE_SIZE + 1..]
                .iter()
                .all(|&b| b == 0)
        );
    }

    // ==================== Scalar Remainder Tests ====================

    #[test]
    fn leading_zero_run_scalar_remainder() {
        // Test cases where remainder after SIMD chunks needs scalar processing
        // 18 bytes = 16 (SIMD) + 2 (scalar remainder)
        let mut data = vec![0u8; 18];
        assert_eq!(leading_zero_run(&data), 18);

        data[17] = 1;
        assert_eq!(leading_zero_run(&data), 17);

        data[16] = 1;
        data[17] = 0;
        assert_eq!(leading_zero_run(&data), 16);
    }

    #[test]
    fn trailing_zero_run_scalar_remainder() {
        // 18 bytes = 16 (SIMD from end) + 2 (scalar remainder at start)
        let mut data = vec![0u8; 18];
        assert_eq!(trailing_zero_run(&data), 18);

        data[0] = 1;
        assert_eq!(trailing_zero_run(&data), 17);

        data[0] = 0;
        data[1] = 1;
        assert_eq!(trailing_zero_run(&data), 16);
    }

    // ==================== Mixed Pattern Tests ====================

    #[test]
    fn write_sparse_chunk_scattered_nonzero_bytes() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Scattered non-zero bytes throughout the chunk
        let size = 1024;
        let mut chunk = vec![0u8; size];
        chunk[0] = b'A';
        chunk[100] = b'B';
        chunk[500] = b'C';
        chunk[999] = b'D';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write scattered");

        assert_eq!(written, size);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(size as u64).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; size];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert_eq!(buffer[0], b'A');
        assert_eq!(buffer[100], b'B');
        assert_eq!(buffer[500], b'C');
        assert_eq!(buffer[999], b'D');
    }

    #[test]
    fn write_sparse_chunk_contiguous_data_block() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Zeros, then contiguous data block, then zeros
        let size = 1024;
        let mut chunk = vec![0u8; size];
        for byte in &mut chunk[400..600] {
            *byte = 0xBB;
        }

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write contiguous block");

        assert_eq!(written, size);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(size as u64).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; size];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert!(buffer[..400].iter().all(|&b| b == 0));
        assert!(buffer[400..600].iter().all(|&b| b == 0xBB));
        assert!(buffer[600..].iter().all(|&b| b == 0));
    }

    // ==================== Edge Case: data_end <= data_start ====================

    #[test]
    fn write_sparse_chunk_segment_all_leading_zeros() {
        // Test case where entire segment is leading zeros (triggers continue branch)
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Create a chunk where first segment is all zeros, but has data later
        let size = super::SPARSE_WRITE_SIZE * 2;
        let mut chunk = vec![0u8; size];
        // Data only in second segment
        chunk[super::SPARSE_WRITE_SIZE + 10] = b'X';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write with all-zero first segment");

        assert_eq!(written, size);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(size as u64).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; size];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert!(
            buffer[..super::SPARSE_WRITE_SIZE + 10]
                .iter()
                .all(|&b| b == 0)
        );
        assert_eq!(buffer[super::SPARSE_WRITE_SIZE + 10], b'X');
        assert!(
            buffer[super::SPARSE_WRITE_SIZE + 11..]
                .iter()
                .all(|&b| b == 0)
        );
    }

    #[test]
    fn write_sparse_chunk_last_byte_only() {
        // Only the very last byte is non-zero
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let mut chunk = vec![0u8; 100];
        chunk[99] = b'Z';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write last byte only");

        assert_eq!(written, 100);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(100).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; 100];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert!(buffer[..99].iter().all(|&b| b == 0));
        assert_eq!(buffer[99], b'Z');
    }

    #[test]
    fn write_sparse_chunk_first_byte_only() {
        // Only the very first byte is non-zero
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let mut chunk = vec![0u8; 100];
        chunk[0] = b'A';

        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write first byte only");

        assert_eq!(written, 100);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(100).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; 100];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert_eq!(buffer[0], b'A');
        assert!(buffer[1..].iter().all(|&b| b == 0));
    }

    // ==================== Single Byte Edge Cases ====================

    #[test]
    fn write_sparse_chunk_single_zero_byte() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let chunk = [0u8; 1];
        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write single zero");

        assert_eq!(written, 1);
        assert_eq!(state.pending_zeros(), 1);

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(1).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = [0xFFu8; 1];
        file.as_file_mut().read_exact(&mut buffer).expect("read");
        assert_eq!(buffer[0], 0);
    }

    #[test]
    fn write_sparse_chunk_single_nonzero_byte() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let chunk = [0xABu8; 1];
        let written = write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write single nonzero");

        assert_eq!(written, 1);
        assert_eq!(state.pending_zeros(), 0);

        let pos = file.as_file_mut().stream_position().expect("position");
        assert_eq!(pos, 1);
    }

    // ==================== Verify Replace vs Accumulate Behavior ====================

    #[test]
    fn write_sparse_chunk_trailing_zeros_become_next_leading() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // First chunk ends with trailing zeros
        let chunk1 = [b'A', 0, 0, 0, 0];
        write_sparse_chunk(file.as_file_mut(), &mut state, &chunk1, path.as_path())
            .expect("write chunk1");

        // These trailing zeros should be replaced by the next call's leading
        let pending_after_chunk1 = state.pending_zeros();
        assert_eq!(pending_after_chunk1, 4);

        // Second chunk starts with leading zeros
        let chunk2 = [0, 0, b'B'];
        write_sparse_chunk(file.as_file_mut(), &mut state, &chunk2, path.as_path())
            .expect("write chunk2");

        // Pending should now be 0 since we wrote 'B'
        assert_eq!(state.pending_zeros(), 0);

        let pos = state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        file.as_file_mut().set_len(pos).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; pos as usize];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert_eq!(buffer[0], b'A');
        assert!(buffer[1..7].iter().all(|&b| b == 0));
        assert_eq!(buffer[7], b'B');
    }

    // ==================== Zero-Run Scalar vs SIMD Consistency ====================

    #[test]
    fn leading_zero_run_consistency_across_sizes() {
        // Test that scalar and SIMD paths produce consistent results
        for size in [
            1, 7, 15, 16, 17, 31, 32, 33, 47, 48, 49, 63, 64, 65, 100, 255,
        ] {
            let all_zeros = vec![0u8; size];
            assert_eq!(
                leading_zero_run(&all_zeros),
                leading_zero_run_scalar(&all_zeros),
                "leading zero mismatch at size {size}"
            );

            if size > 0 {
                // Non-zero at start
                let mut start_nonzero = vec![0u8; size];
                start_nonzero[0] = 1;
                assert_eq!(
                    leading_zero_run(&start_nonzero),
                    leading_zero_run_scalar(&start_nonzero),
                    "leading zero start nonzero mismatch at size {size}"
                );

                // Non-zero at end
                let mut end_nonzero = vec![0u8; size];
                end_nonzero[size - 1] = 1;
                assert_eq!(
                    leading_zero_run(&end_nonzero),
                    leading_zero_run_scalar(&end_nonzero),
                    "leading zero end nonzero mismatch at size {size}"
                );

                // Non-zero in middle
                if size > 2 {
                    let mut mid_nonzero = vec![0u8; size];
                    mid_nonzero[size / 2] = 1;
                    assert_eq!(
                        leading_zero_run(&mid_nonzero),
                        leading_zero_run_scalar(&mid_nonzero),
                        "leading zero middle nonzero mismatch at size {size}"
                    );
                }
            }
        }
    }

    #[test]
    fn trailing_zero_run_consistency_across_sizes() {
        for size in [
            1, 7, 15, 16, 17, 31, 32, 33, 47, 48, 49, 63, 64, 65, 100, 255,
        ] {
            let all_zeros = vec![0u8; size];
            assert_eq!(
                trailing_zero_run(&all_zeros),
                trailing_zero_run_scalar(&all_zeros),
                "trailing zero mismatch at size {size}"
            );

            if size > 0 {
                // Non-zero at start
                let mut start_nonzero = vec![0u8; size];
                start_nonzero[0] = 1;
                assert_eq!(
                    trailing_zero_run(&start_nonzero),
                    trailing_zero_run_scalar(&start_nonzero),
                    "trailing zero start nonzero mismatch at size {size}"
                );

                // Non-zero at end
                let mut end_nonzero = vec![0u8; size];
                end_nonzero[size - 1] = 1;
                assert_eq!(
                    trailing_zero_run(&end_nonzero),
                    trailing_zero_run_scalar(&end_nonzero),
                    "trailing zero end nonzero mismatch at size {size}"
                );
            }
        }
    }

    // ==================== Large Pending Zero Flush ====================

    #[test]
    fn sparse_state_flush_large_pending() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Accumulate a large amount of zeros (but not so large it takes forever)
        let large_pending = 10 * 1024 * 1024u64; // 10MB
        state.accumulate(large_pending as usize);

        let final_pos = state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish large pending");

        assert_eq!(final_pos, large_pending);
    }

    // ==================== File Truncation Tests ====================

    #[test]
    fn sparse_write_with_truncation_preserves_holes() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Write data with sparse holes
        let chunk = [b'H', 0, 0, 0, 0, 0, 0, 0, b'T'];
        write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write chunk");

        let final_pos = state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        // Truncate to exact size
        file.as_file_mut().set_len(final_pos).expect("truncate");

        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; final_pos as usize];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert_eq!(buffer[0], b'H');
        assert!(buffer[1..8].iter().all(|&b| b == 0));
        assert_eq!(buffer[8], b'T');
    }

    // ==================== Multiple Write Cycles ====================

    #[test]
    fn sparse_writer_multiple_write_finish_cycles() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // First cycle
        let mut state1 = SparseWriteState::default();
        let chunk1 = [b'1', 0, 0, b'2'];
        write_sparse_chunk(file.as_file_mut(), &mut state1, &chunk1, path.as_path())
            .expect("write chunk1");
        let pos1 = state1
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish1");
        assert_eq!(pos1, 4);

        // Second cycle (continue writing)
        let mut state2 = SparseWriteState::default();
        let chunk2 = [b'3', 0, b'4'];
        write_sparse_chunk(file.as_file_mut(), &mut state2, &chunk2, path.as_path())
            .expect("write chunk2");
        let pos2 = state2
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish2");
        assert_eq!(pos2, 7);

        file.as_file_mut().set_len(7).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0xFFu8; 7];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert_eq!(buffer[0], b'1');
        assert!(buffer[1..3].iter().all(|&b| b == 0));
        assert_eq!(buffer[3], b'2');
        assert_eq!(buffer[4], b'3');
        assert_eq!(buffer[5], 0);
        assert_eq!(buffer[6], b'4');
    }

    // ==================== Verify Const Methods ====================

    #[test]
    fn sparse_state_const_methods() {
        let mut state = SparseWriteState::default();

        // accumulate is const
        state.accumulate(10);
        assert_eq!(state.pending_zeros(), 10);

        // replace is const
        state.replace(5);
        assert_eq!(state.pending_zeros(), 5);

        // pending_zeros is const
        let _pending: u64 = state.pending_zeros();
    }

    // ==================== SparseRegion Tests ====================

    #[test]
    fn sparse_region_accessors() {
        let data = super::SparseRegion::Data {
            offset: 100,
            length: 200,
        };
        assert_eq!(data.offset(), 100);
        assert_eq!(data.length(), 200);
        assert!(data.is_data());
        assert!(!data.is_hole());

        let hole = super::SparseRegion::Hole {
            offset: 500,
            length: 1000,
        };
        assert_eq!(hole.offset(), 500);
        assert_eq!(hole.length(), 1000);
        assert!(hole.is_hole());
        assert!(!hole.is_data());
    }

    #[test]
    fn sparse_region_equality() {
        let data1 = super::SparseRegion::Data {
            offset: 0,
            length: 100,
        };
        let data2 = super::SparseRegion::Data {
            offset: 0,
            length: 100,
        };
        let data3 = super::SparseRegion::Data {
            offset: 0,
            length: 200,
        };

        assert_eq!(data1, data2);
        assert_ne!(data1, data3);

        let hole1 = super::SparseRegion::Hole {
            offset: 0,
            length: 100,
        };
        let hole2 = super::SparseRegion::Hole {
            offset: 0,
            length: 100,
        };

        assert_eq!(hole1, hole2);
        assert_ne!(data1, hole1);
    }

    // ==================== SparseDetector Tests ====================

    #[test]
    fn sparse_detector_all_zeros() {
        let detector = super::SparseDetector::new(100);
        let data = vec![0u8; 1000];
        let regions = detector.scan(&data, 0);

        assert_eq!(regions.len(), 1);
        assert_eq!(
            regions[0],
            super::SparseRegion::Hole {
                offset: 0,
                length: 1000
            }
        );
    }

    #[test]
    fn sparse_detector_all_data() {
        let detector = super::SparseDetector::new(100);
        let data = vec![0xAAu8; 1000];
        let regions = detector.scan(&data, 0);

        assert_eq!(regions.len(), 1);
        assert_eq!(
            regions[0],
            super::SparseRegion::Data {
                offset: 0,
                length: 1000
            }
        );
    }

    #[test]
    fn sparse_detector_mixed_regions() {
        let detector = super::SparseDetector::new(100);
        let mut data = vec![0xBBu8; 50]; // Data
        data.extend_from_slice(&[0u8; 200]); // Hole
        data.extend_from_slice(&[0xCCu8; 75]); // Data

        let regions = detector.scan(&data, 1000);

        assert_eq!(regions.len(), 3);
        assert_eq!(
            regions[0],
            super::SparseRegion::Data {
                offset: 1000,
                length: 50
            }
        );
        assert_eq!(
            regions[1],
            super::SparseRegion::Hole {
                offset: 1050,
                length: 200
            }
        );
        assert_eq!(
            regions[2],
            super::SparseRegion::Data {
                offset: 1250,
                length: 75
            }
        );
    }

    #[test]
    fn sparse_detector_small_zero_runs_treated_as_data() {
        let detector = super::SparseDetector::new(100);
        let mut data = vec![0xAAu8; 50];
        data.extend_from_slice(&[0u8; 10]); // Small run - should be part of data
        data.extend_from_slice(&[0xBBu8; 50]);

        let regions = detector.scan(&data, 0);

        // The scanner treats two separate data blocks with a small zero run between them
        // The total length should equal input
        let total_length: u64 = regions.iter().map(|r| r.length()).sum();
        assert_eq!(total_length, 110);

        // All regions should be data (small zero runs don't create holes)
        assert!(regions.iter().all(|r| r.is_data()));
    }

    #[test]
    fn sparse_detector_threshold_boundary() {
        let detector = super::SparseDetector::new(100);

        // Exactly at threshold - should be a hole
        let data = vec![0u8; 100];
        let regions = detector.scan(&data, 0);
        assert_eq!(regions.len(), 1);
        assert!(regions[0].is_hole());

        // One byte below threshold - should be data
        let data = vec![0u8; 99];
        let regions = detector.scan(&data, 0);
        assert_eq!(regions.len(), 1);
        assert!(!regions[0].is_hole()); // 99 bytes is below threshold

        // One byte above threshold - should be a hole
        let data = vec![0u8; 101];
        let regions = detector.scan(&data, 0);
        assert_eq!(regions.len(), 1);
        assert!(regions[0].is_hole());
    }

    #[test]
    fn sparse_detector_empty_buffer() {
        let detector = super::SparseDetector::new(100);
        let data: &[u8] = &[];
        let regions = detector.scan(data, 0);

        assert_eq!(regions.len(), 0);
    }

    #[test]
    fn sparse_detector_is_all_zeros() {
        assert!(super::SparseDetector::is_all_zeros(&[]));
        assert!(super::SparseDetector::is_all_zeros(&[0u8; 1000]));
        assert!(!super::SparseDetector::is_all_zeros(&[1]));
        assert!(!super::SparseDetector::is_all_zeros(&[0, 0, 0, 1, 0]));

        let mut large = vec![0u8; 10000];
        assert!(super::SparseDetector::is_all_zeros(&large));
        large[5000] = 1;
        assert!(!super::SparseDetector::is_all_zeros(&large));
    }

    #[test]
    fn sparse_detector_base_offset_applied() {
        let detector = super::SparseDetector::new(10);
        let data = vec![0xAAu8; 50];
        let regions = detector.scan(&data, 5000);

        assert_eq!(regions.len(), 1);
        assert_eq!(
            regions[0],
            super::SparseRegion::Data {
                offset: 5000,
                length: 50
            }
        );
    }

    #[test]
    fn sparse_detector_multiple_holes_and_data() {
        let detector = super::SparseDetector::new(50);

        let mut data = vec![0xAAu8; 100]; // Data
        data.extend_from_slice(&[0u8; 100]); // Hole
        data.extend_from_slice(&[0xBBu8; 100]); // Data
        data.extend_from_slice(&[0u8; 100]); // Hole
        data.extend_from_slice(&[0xCCu8; 100]); // Data

        let regions = detector.scan(&data, 0);

        assert_eq!(regions.len(), 5);
        assert!(regions[0].is_data());
        assert_eq!(regions[0].offset(), 0);
        assert_eq!(regions[0].length(), 100);

        assert!(regions[1].is_hole());
        assert_eq!(regions[1].offset(), 100);
        assert_eq!(regions[1].length(), 100);

        assert!(regions[2].is_data());
        assert_eq!(regions[2].offset(), 200);
        assert_eq!(regions[2].length(), 100);

        assert!(regions[3].is_hole());
        assert_eq!(regions[3].offset(), 300);
        assert_eq!(regions[3].length(), 100);

        assert!(regions[4].is_data());
        assert_eq!(regions[4].offset(), 400);
        assert_eq!(regions[4].length(), 100);
    }

    #[test]
    fn sparse_detector_default_threshold() {
        let detector = super::SparseDetector::default_threshold();

        // Should use SPARSE_WRITE_SIZE as threshold
        let small_zeros = vec![0u8; super::SPARSE_WRITE_SIZE - 1];
        let regions = detector.scan(&small_zeros, 0);
        assert_eq!(regions.len(), 1);
        assert!(regions[0].is_data() || regions[0].length() < super::SPARSE_WRITE_SIZE as u64);

        let large_zeros = vec![0u8; super::SPARSE_WRITE_SIZE + 1];
        let regions = detector.scan(&large_zeros, 0);
        assert_eq!(regions.len(), 1);
        assert!(regions[0].is_hole());
    }

    // ==================== SparseWriter Tests ====================

    #[test]
    fn sparse_writer_basic_write() {
        let file = NamedTempFile::new().expect("temp file");
        let mut writer = super::SparseWriter::new(
            file.as_file().try_clone().expect("clone"),
            false, // Dense mode
        );

        writer.write_region(0, b"hello").expect("write");
        writer.write_region(10, b"world").expect("write");
        writer.finish(15).expect("finish");

        // Verify contents
        let mut file_handle = file.reopen().expect("reopen");
        use std::io::Read;
        let mut contents = Vec::new();
        file_handle.read_to_end(&mut contents).expect("read");

        assert_eq!(&contents[0..5], b"hello");
        assert_eq!(&contents[10..15], b"world");
    }

    #[test]
    fn sparse_writer_sparse_mode() {
        let file = NamedTempFile::new().expect("temp file");
        let mut writer = super::SparseWriter::new(
            file.as_file().try_clone().expect("clone"),
            true, // Sparse mode
        );

        // Build the data as one sequential write
        let mut data = vec![b'A'];
        data.extend_from_slice(&vec![0u8; super::SPARSE_WRITE_SIZE * 2]);
        data.push(b'B');

        writer.write_region(0, &data).expect("write");
        writer.finish(data.len() as u64).expect("finish");

        // Verify contents
        let mut file_handle = file.reopen().expect("reopen");
        use std::io::Read;
        let mut contents = Vec::new();
        file_handle.read_to_end(&mut contents).expect("read");

        assert_eq!(contents.len(), data.len());
        assert_eq!(contents[0], b'A');
        let zero_end = 1 + super::SPARSE_WRITE_SIZE * 2;
        assert!(contents[1..zero_end].iter().all(|&b| b == 0));
        assert_eq!(contents[zero_end], b'B');
    }

    #[test]
    fn sparse_writer_empty_data() {
        let file = NamedTempFile::new().expect("temp file");
        let mut writer = super::SparseWriter::new(file.as_file().try_clone().expect("clone"), true);

        writer.write_region(0, &[]).expect("write empty");
        writer.finish(0).expect("finish");

        let metadata = file.as_file().metadata().expect("metadata");
        assert_eq!(metadata.len(), 0);
    }

    #[test]
    fn sparse_writer_file_accessors() {
        let file = NamedTempFile::new().expect("temp file");
        let mut writer =
            super::SparseWriter::new(file.as_file().try_clone().expect("clone"), false);

        // Test accessors
        let _file_ref: &fs::File = writer.file();
        let _file_mut: &mut fs::File = writer.file_mut();
    }

    // ==================== SparseReader Tests ====================

    #[test]
    fn sparse_reader_empty_file() {
        let file = NamedTempFile::new().expect("temp file");
        let regions = super::SparseReader::detect_holes(file.as_file()).expect("detect holes");

        assert_eq!(regions.len(), 0);
    }

    #[test]
    fn sparse_reader_all_data_file() {
        let mut file = NamedTempFile::new().expect("temp file");
        file.as_file_mut()
            .write_all(&vec![0xAAu8; 1000])
            .expect("write data");

        let regions = super::SparseReader::detect_holes(file.as_file()).expect("detect holes");

        // Should be one data region
        assert_eq!(regions.len(), 1);
        assert!(regions[0].is_data());
        assert_eq!(regions[0].offset(), 0);
        assert_eq!(regions[0].length(), 1000);
    }

    #[test]
    fn sparse_reader_all_zeros_file() {
        let mut file = NamedTempFile::new().expect("temp file");
        file.as_file_mut()
            .write_all(&vec![0u8; super::SPARSE_WRITE_SIZE * 2])
            .expect("write zeros");

        let regions = super::SparseReader::detect_holes(file.as_file()).expect("detect holes");

        // Should detect as hole (or possibly data depending on how it was written)
        // The important part is that it detects something
        assert!(!regions.is_empty());
    }

    #[test]
    fn sparse_reader_mixed_file() {
        let mut file = NamedTempFile::new().expect("temp file");

        // Write: data, zeros, data
        file.as_file_mut()
            .write_all(&[0xBBu8; 100])
            .expect("write data");
        file.as_file_mut()
            .write_all(&vec![0u8; super::SPARSE_WRITE_SIZE * 2])
            .expect("write zeros");
        file.as_file_mut()
            .write_all(&[0xCCu8; 100])
            .expect("write data");

        let regions = super::SparseReader::detect_holes(file.as_file()).expect("detect holes");

        // Should detect multiple regions
        assert!(!regions.is_empty());

        // Verify total size matches
        let total_length: u64 = regions.iter().map(|r| r.length()).sum();
        let expected_size = 200 + super::SPARSE_WRITE_SIZE * 2;
        assert_eq!(total_length, expected_size as u64);
    }

    #[test]
    fn sparse_reader_coalesce_adjacent_data() {
        let mut regions = vec![
            super::SparseRegion::Data {
                offset: 0,
                length: 100,
            },
            super::SparseRegion::Data {
                offset: 100,
                length: 200,
            },
        ];

        super::SparseReader::coalesce_regions(&mut regions);

        assert_eq!(regions.len(), 1);
        assert_eq!(
            regions[0],
            super::SparseRegion::Data {
                offset: 0,
                length: 300
            }
        );
    }

    #[test]
    fn sparse_reader_coalesce_adjacent_holes() {
        let mut regions = vec![
            super::SparseRegion::Hole {
                offset: 0,
                length: 100,
            },
            super::SparseRegion::Hole {
                offset: 100,
                length: 200,
            },
        ];

        super::SparseReader::coalesce_regions(&mut regions);

        assert_eq!(regions.len(), 1);
        assert_eq!(
            regions[0],
            super::SparseRegion::Hole {
                offset: 0,
                length: 300
            }
        );
    }

    #[test]
    fn sparse_reader_coalesce_mixed_types() {
        let mut regions = vec![
            super::SparseRegion::Data {
                offset: 0,
                length: 100,
            },
            super::SparseRegion::Hole {
                offset: 100,
                length: 200,
            },
            super::SparseRegion::Data {
                offset: 300,
                length: 100,
            },
        ];

        super::SparseReader::coalesce_regions(&mut regions);

        // Different types should not coalesce
        assert_eq!(regions.len(), 3);
    }

    #[test]
    fn sparse_reader_coalesce_non_adjacent() {
        let mut regions = vec![
            super::SparseRegion::Data {
                offset: 0,
                length: 100,
            },
            super::SparseRegion::Data {
                offset: 200, // Gap
                length: 100,
            },
        ];

        super::SparseReader::coalesce_regions(&mut regions);

        // Non-adjacent regions should not coalesce
        assert_eq!(regions.len(), 2);
    }

    #[test]
    fn sparse_reader_coalesce_empty() {
        let mut regions: Vec<super::SparseRegion> = vec![];
        super::SparseReader::coalesce_regions(&mut regions);
        assert_eq!(regions.len(), 0);
    }

    #[test]
    fn sparse_reader_coalesce_single() {
        let mut regions = vec![super::SparseRegion::Data {
            offset: 0,
            length: 100,
        }];

        super::SparseReader::coalesce_regions(&mut regions);

        assert_eq!(regions.len(), 1);
    }

    #[test]
    fn sparse_reader_coalesce_multiple_adjacent() {
        let mut regions = vec![
            super::SparseRegion::Data {
                offset: 0,
                length: 100,
            },
            super::SparseRegion::Data {
                offset: 100,
                length: 200,
            },
            super::SparseRegion::Data {
                offset: 300,
                length: 50,
            },
            super::SparseRegion::Hole {
                offset: 350,
                length: 100,
            },
            super::SparseRegion::Hole {
                offset: 450,
                length: 50,
            },
        ];

        super::SparseReader::coalesce_regions(&mut regions);

        assert_eq!(regions.len(), 2);
        assert_eq!(
            regions[0],
            super::SparseRegion::Data {
                offset: 0,
                length: 350
            }
        );
        assert_eq!(
            regions[1],
            super::SparseRegion::Hole {
                offset: 350,
                length: 150
            }
        );
    }

    // ==================== Very Small SPARSE_WRITE_SIZE Multiples ====================

    #[test]
    fn write_sparse_chunk_very_small_chunks() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        // Write many very small chunks
        for i in 0..100u8 {
            let chunk = [i, 0];
            write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
                .expect("write small chunk");
        }

        let final_pos = state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finish");

        assert_eq!(final_pos, 200);
    }

    // ==================== Punch Hole Edge Cases ====================

    #[test]
    fn punch_hole_consecutive_holes() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Write non-zero data
        let data = vec![0xAAu8; 8192];
        file.as_file_mut().write_all(&data).expect("write");

        // Punch consecutive holes
        punch_hole(file.as_file_mut(), &path, 1000, 500).expect("punch hole 1");
        punch_hole(file.as_file_mut(), &path, 1500, 500).expect("punch hole 2");
        punch_hole(file.as_file_mut(), &path, 2000, 500).expect("punch hole 3");

        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
        let mut buffer = vec![0xFFu8; 8192];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        // All three holes should be punched
        assert!(buffer[..1000].iter().all(|&b| b == 0xAA));
        assert!(buffer[1000..2500].iter().all(|&b| b == 0));
        assert!(buffer[2500..].iter().all(|&b| b == 0xAA));
    }

    #[test]
    fn punch_hole_overlapping_holes() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // Write non-zero data
        let data = vec![0xBBu8; 4096];
        file.as_file_mut().write_all(&data).expect("write");

        // Punch overlapping holes
        punch_hole(file.as_file_mut(), &path, 1000, 1000).expect("punch hole 1");
        punch_hole(file.as_file_mut(), &path, 1500, 1000).expect("punch hole 2 overlaps");

        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
        let mut buffer = vec![0xFFu8; 4096];
        file.as_file_mut().read_exact(&mut buffer).expect("read");

        assert!(buffer[..1000].iter().all(|&b| b == 0xBB));
        assert!(buffer[1000..2500].iter().all(|&b| b == 0));
        assert!(buffer[2500..].iter().all(|&b| b == 0xBB));
    }
}
