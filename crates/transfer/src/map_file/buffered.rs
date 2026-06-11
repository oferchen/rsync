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

    /// Returns a shared reference to the underlying file handle.
    ///
    /// Exposed so optimizations like the IUD-10 `copy_file_range` fast path
    /// can borrow the fd without disturbing the sliding-window cache state.
    #[inline]
    pub fn file(&self) -> &File {
        &self.file
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
        //
        // upstream: fileio.c:236 `realloc_array` only grows the backing buffer;
        // it never shrinks. Mirror that invariant: when a smaller window is
        // requested (e.g., the new window is near EOF after a full-size load),
        // `Vec::resize` would otherwise truncate the bytes the overlap branch
        // is about to relocate via `copy_within`, panicking on out-of-bounds
        // source or dropping bytes intended for reuse. Using `.max(buffer.len())`
        // keeps the buffer monotonically non-decreasing; `window_len` continues
        // to bound the valid region so callers never observe stale tail bytes.
        let old_end = self.window_start + self.window_len as u64;
        let target_len = window_size.max(self.buffer.len());
        let (read_start, read_offset) = if self.window_len > 0
            && aligned_start >= self.window_start
            && aligned_start < old_end
            && aligned_start + window_size as u64 >= old_end
        {
            let reuse_len = (old_end - aligned_start) as usize;
            let src_offset = (aligned_start - self.window_start) as usize;

            self.buffer.resize(target_len, 0);
            // UTS-18.f: fail-loud guard. A malformed delta stream can request a
            // window whose `reuse_len` (derived from the prior window extent)
            // exceeds the resized buffer length when the file tails off near
            // EOF, or whose `src_offset + reuse_len` exceeds the prior valid
            // window. Validate before `copy_within` so we surface an
            // `InvalidData` error instead of aborting the process.
            let src_end = src_offset.checked_add(reuse_len).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "buffered map_file overlap range overflowed: src_offset={src_offset} reuse_len={reuse_len}"
                    ),
                )
            })?;
            if src_end > self.buffer.len() || reuse_len > self.buffer.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "buffered map_file overlap range {src_offset}..{src_end} exceeds buffer length {buf_len} (reuse_len={reuse_len})",
                        buf_len = self.buffer.len(),
                    ),
                ));
            }
            self.buffer.copy_within(src_offset..src_end, 0);

            (old_end, reuse_len)
        } else {
            self.buffer.resize(target_len, 0);
            (aligned_start, 0)
        };

        let read_size = window_size - read_offset;
        if read_size > 0 {
            self.file.seek(SeekFrom::Start(read_start))?;
            self.file
                .read_exact(&mut self.buffer[read_offset..window_size])?;
        }

        self.window_start = aligned_start;
        // UTS-18.g: root-cause clamp. `window_size` already takes the min of
        // `max_window` and `remaining`, but record `window_len` against
        // `file_size - window_start` so the invariant is locally provable at
        // the single assignment site. Future callers and any state-fabrication
        // path (tests, recovery, partial loads) cannot leave `window_len`
        // claiming more bytes than the file actually has at `window_start`.
        let remaining_from_start = self
            .size
            .saturating_sub(self.window_start)
            .min(usize::MAX as u64) as usize;
        self.window_len = window_size.min(remaining_from_start);

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
        let end = start.checked_add(len).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("buffered map_file range overflowed: start={start} len={len}"),
            )
        })?;
        self.buffer.get(start..end).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "buffered map_file slice {start}..{end} exceeds buffer length {buf_len}",
                    buf_len = self.buffer.len(),
                ),
            )
        })
    }

    #[inline]
    fn window_size(&self) -> usize {
        self.max_window
    }

    #[inline]
    fn file_size(&self) -> u64 {
        self.size
    }

    #[inline]
    fn buffered_file(&self) -> Option<&File> {
        Some(&self.file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTS-18.f regression: when the cached window state is inconsistent with
    /// the requested slice (an `end` that runs past the buffer length), the
    /// guarded `map_ptr` must return `InvalidData` instead of aborting the
    /// process with a slice-bounds panic. Mirrors the production-crash ratio
    /// (range_end > buffer length) at a stripped-down scale: offset=128,
    /// len=64, buffer length=32.
    #[test]
    fn map_ptr_slice_out_of_range_returns_err_not_panic() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&vec![0u8; 1024]).unwrap();
        tmp.flush().unwrap();

        let mut map = BufferedMap::open_with_window(tmp.path(), 256).unwrap();

        // Fabricate a window state that claims to cover [window_start ..
        // window_start + window_len) but whose backing buffer is shorter than
        // window_len. A bare `&self.buffer[start..end]` would panic; the
        // fail-loud guard converts the out-of-range slice into an Err.
        map.window_start = 0;
        map.window_len = 192;
        map.buffer = vec![0u8; 32];

        let result = MapStrategy::map_ptr(&mut map, 128, 64);
        let err = result.expect_err("expected Err, not panic");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(
            msg.contains("128..192") && msg.contains("32"),
            "error message missing bounds detail: {msg}"
        );
    }

    /// Drives the overlap-shrink branch of `load_window` directly: when the
    /// prior cached window's claimed extent exceeds the resized buffer, the
    /// `copy_within` source range would walk past the new buffer length. The
    /// guard converts this into `InvalidData` instead of a panic.
    #[test]
    fn load_window_overlap_shrink_returns_err_not_panic() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&vec![0u8; 2048]).unwrap();
        tmp.flush().unwrap();

        let mut map = BufferedMap::open_with_window(tmp.path(), 1024).unwrap();

        // Fabricate an inconsistent cached state: the window falsely claims to
        // span [0..1500). A forward slide to offset=1024 takes the overlap
        // branch (aligned_start is inside the old window) and computes
        // reuse_len=476 with src_offset=1024 - but the resized buffer can only
        // hold max_window=1024 bytes, so src_offset+reuse_len=1500 walks past
        // it. Exactly the production crash shape (range_end > buffer length)
        // at scale.
        map.window_start = 0;
        map.window_len = 1500;

        let result = MapStrategy::map_ptr(&mut map, 1024, 512);
        let err = result.expect_err("expected Err, not panic");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("exceeds buffer length"),
            "error message missing bounds detail: {err}"
        );
    }

    /// UTS-18.g positive root-cause regression: a legitimate transfer of a
    /// file smaller than `MAX_MAP_SIZE` must complete successfully. Mirrors
    /// the production crash math (file=48128 bytes, `MAX_MAP_SIZE=262144`):
    /// `window_len` must clamp to the file's remaining bytes (48128), not
    /// inherit the requested `max_window` (262144). A subsequent full-file
    /// `map_ptr` must return `Ok` with all 48128 bytes - never the
    /// "exceeds buffer length" Err that PR #5566's guards surface when state
    /// is inconsistent.
    #[test]
    fn load_window_clamps_window_len_to_remaining_file_bytes() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        const FILE_SIZE: usize = 48128;

        let payload: Vec<u8> = (0..FILE_SIZE).map(|i| (i & 0xff) as u8).collect();
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&payload).unwrap();
        tmp.flush().unwrap();

        let mut map = BufferedMap::open_with_window(tmp.path(), MAX_MAP_SIZE).unwrap();

        // Trigger a window load over the entire file. `load_window` is
        // private; drive it through the public `map_ptr` entry that all four
        // untrusted callers use (token_loop.rs, response.rs, sync.rs,
        // applicator.rs).
        let slice = MapStrategy::map_ptr(&mut map, 0, FILE_SIZE)
            .expect("legitimate full-file map_ptr must return Ok, not 'exceeds buffer length' Err");
        assert_eq!(slice.len(), FILE_SIZE);
        assert_eq!(slice, &payload[..]);

        // The clamp invariant: window_len reflects the file's remaining
        // bytes (48128), never the requested max_window (262144).
        assert_eq!(
            map.window_len, FILE_SIZE,
            "window_len must clamp to remaining file bytes ({FILE_SIZE}), got {actual}",
            actual = map.window_len,
        );
        assert!(
            map.window_len <= map.buffer.len(),
            "window_len ({wl}) must not exceed buffer length ({bl})",
            wl = map.window_len,
            bl = map.buffer.len(),
        );

        // A repeat full-file slice must hit the cached window and return
        // the same bytes - the path PR #5566's guard would reject if
        // `window_len` lied about buffer extent.
        let again = MapStrategy::map_ptr(&mut map, 0, FILE_SIZE).expect("cached map_ptr must Ok");
        assert_eq!(again, &payload[..]);
    }
}
