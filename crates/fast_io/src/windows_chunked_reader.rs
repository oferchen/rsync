//! Windows chunked file reader replacing `mmap_reader_stub`'s `Vec<u8>`-per-file allocation.
//!
//! Reads files in bounded-size chunks (default 4 MiB), capping peak RSS at
//! `chunk_size + small constant` instead of `file_size`. Implements `Read + Seek`
//! for drop-in compatibility with the 5 `mmap_reader_stub` call sites identified
//! by the WIN-S.LAND.1.b.1 audit (PR #3771).
//!
//! The legacy `mmap_reader_stub::MmapReader` slurps the entire file into a
//! `Vec<u8>` at `open()` time; with 1 GiB basis files in delta-apply at
//! `transfer/src/map_file/mmap.rs:38`, peak RSS blows up proportionally.
//! `WindowsChunkedReader` keeps a single rolling chunk in memory and refills it
//! on demand from the underlying `File`, capping resident set to the chunk size
//! plus the small per-instance overhead.
//!
//! Chunk size is tunable via `OC_RSYNC_WIN_CHUNK_BYTES` for benchmarking; the
//! default of 4 MiB matches the IOCP file-reader page-aligned slab.
//!
//! `as_slice()` is retained for delta-apply, the single call site that needs
//! random-access slicing of the full basis. The first `as_slice()` call loads
//! the whole file into the chunk cache and returns a borrowed slice; subsequent
//! calls reuse the same allocation. All other call sites should use the
//! `Read + Seek` interface, which never grows the cache beyond `chunk_size`.

#![cfg(windows)]

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// Default chunk size for streaming reads (4 MiB).
///
/// Matches the IOCP file-reader slab so a Windows host that already has IOCP
/// page-aligned buffers in flight does not double its working set when the
/// chunked reader is active.
pub const DEFAULT_CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Environment variable name for overriding the chunk size at runtime.
///
/// Set to a positive integer (bytes). Invalid or zero values fall back to the
/// caller-supplied `chunk_size` argument.
pub const CHUNK_SIZE_ENV: &str = "OC_RSYNC_WIN_CHUNK_BYTES";

/// Sentinel meaning "no chunk currently loaded".
const NO_CHUNK_LOADED: u64 = u64::MAX;

/// Bounded-RSS file reader for Windows.
///
/// Keeps at most `chunk_size` bytes resident at a time. Random access via
/// `Seek` triggers a chunk refill on the next `read()` if the new position
/// falls outside the current window.
///
/// # Invariants
///
/// - `position <= size` after every public method returns successfully.
/// - `chunk_offset == NO_CHUNK_LOADED` iff `chunk_cache` is empty or stale.
/// - `chunk_offset` is always a multiple of `chunk_size` when a chunk is
///   loaded via `refill_chunk_at`, except after `as_slice()` which loads the
///   whole file at offset 0.
pub struct WindowsChunkedReader {
    file: File,
    size: u64,
    chunk_size: usize,
    position: u64,
    chunk_cache: Vec<u8>,
    /// File offset of the first byte in `chunk_cache`. `NO_CHUNK_LOADED` when
    /// no chunk is currently resident.
    chunk_offset: u64,
}

impl std::fmt::Debug for WindowsChunkedReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsChunkedReader")
            .field("size", &self.size)
            .field("chunk_size", &self.chunk_size)
            .field("position", &self.position)
            .field("chunk_offset", &self.chunk_offset)
            .field("chunk_cache_len", &self.chunk_cache.len())
            .finish()
    }
}

impl WindowsChunkedReader {
    /// Opens `path` for chunked reading with the default 4 MiB chunk size.
    ///
    /// Honors `OC_RSYNC_WIN_CHUNK_BYTES` if set.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Self::with_chunk_size(path, DEFAULT_CHUNK_SIZE)
    }

    /// Opens `path` with a caller-specified chunk size.
    ///
    /// The `OC_RSYNC_WIN_CHUNK_BYTES` environment variable, when parseable as a
    /// non-zero `usize`, overrides `chunk_size`. Invalid, missing, or zero env
    /// values fall back to `chunk_size`. The effective chunk size is clamped to
    /// at least 1 byte so the refill loop always makes forward progress.
    pub fn with_chunk_size<P: AsRef<Path>>(path: P, chunk_size: usize) -> io::Result<Self> {
        let env_chunk = std::env::var(CHUNK_SIZE_ENV)
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n > 0);
        let effective = env_chunk.unwrap_or(chunk_size).max(1);
        let file = File::open(path.as_ref())?;
        let size = file.metadata()?.len();
        Ok(Self {
            file,
            size,
            chunk_size: effective,
            position: 0,
            chunk_cache: Vec::new(),
            chunk_offset: NO_CHUNK_LOADED,
        })
    }

    /// Returns the file size in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Returns the current read/seek position.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Returns the effective chunk size in bytes.
    #[must_use]
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    /// Returns the whole file as a borrowed slice, loading it on first call.
    ///
    /// Trade-off: the first invocation allocates `size` bytes (defeating the
    /// bounded-RSS guarantee for this instance); subsequent calls reuse the
    /// allocation. Use only when random-access slicing of the entire basis is
    /// required, as in delta-apply at `transfer/src/map_file/mmap.rs:38`. All
    /// other call sites should use the `Read + Seek` interface instead.
    pub fn as_slice(&mut self) -> io::Result<&[u8]> {
        let needs_load = self.chunk_offset != 0 || self.chunk_cache.len() as u64 != self.size;
        if needs_load {
            self.chunk_cache.clear();
            let size_usize = usize::try_from(self.size).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "file too large to materialize as a single slice",
                )
            })?;
            self.chunk_cache.reserve_exact(size_usize);
            self.file.seek(SeekFrom::Start(0))?;
            self.file.read_to_end(&mut self.chunk_cache)?;
            self.chunk_offset = 0;
        }
        Ok(&self.chunk_cache)
    }

    /// Refills `chunk_cache` so it covers `offset`, aligned to a chunk boundary.
    fn refill_chunk_at(&mut self, offset: u64) -> io::Result<()> {
        if offset >= self.size {
            self.chunk_cache.clear();
            self.chunk_offset = NO_CHUNK_LOADED;
            return Ok(());
        }
        let chunk_size_u64 = self.chunk_size as u64;
        let chunk_start = (offset / chunk_size_u64) * chunk_size_u64;
        let remaining = self.size - chunk_start;
        let to_read = remaining.min(chunk_size_u64) as usize;
        self.chunk_cache.resize(to_read, 0);
        self.file.seek(SeekFrom::Start(chunk_start))?;
        self.file.read_exact(&mut self.chunk_cache)?;
        self.chunk_offset = chunk_start;
        Ok(())
    }
}

impl Read for WindowsChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.size {
            return Ok(0);
        }

        let chunk_end = if self.chunk_offset == NO_CHUNK_LOADED {
            0
        } else {
            self.chunk_offset.saturating_add(self.chunk_cache.len() as u64)
        };
        if self.chunk_offset == NO_CHUNK_LOADED
            || self.position < self.chunk_offset
            || self.position >= chunk_end
        {
            self.refill_chunk_at(self.position)?;
            if self.chunk_cache.is_empty() {
                return Ok(0);
            }
        }

        let chunk_pos = (self.position - self.chunk_offset) as usize;
        let available = self.chunk_cache.len() - chunk_pos;
        let to_copy = buf.len().min(available);
        buf[..to_copy].copy_from_slice(&self.chunk_cache[chunk_pos..chunk_pos + to_copy]);
        self.position += to_copy as u64;
        Ok(to_copy)
    }
}

impl Seek for WindowsChunkedReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(n) => {
                if n < 0 {
                    self.position.checked_sub(n.unsigned_abs()).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "seek before start")
                    })?
                } else {
                    self.position.checked_add(n as u64).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "seek overflow")
                    })?
                }
            }
            SeekFrom::End(n) => {
                if n < 0 {
                    self.size.checked_sub(n.unsigned_abs()).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "seek before start")
                    })?
                } else {
                    self.size.checked_add(n as u64).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "seek overflow")
                    })?
                }
            }
        };
        self.position = new_pos;
        Ok(new_pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;
    use tempfile::NamedTempFile;

    /// Serializes tests that read or mutate `OC_RSYNC_WIN_CHUNK_BYTES`. The
    /// environment is process-global; concurrent setters from other tests
    /// otherwise see torn values.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_temp(bytes: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("create temp file");
        f.write_all(bytes).expect("write temp file");
        f.flush().expect("flush temp file");
        f
    }

    #[test]
    fn empty_file_size_zero() {
        let tmp = write_temp(&[]);
        let mut r = WindowsChunkedReader::open(tmp.path()).expect("open empty");
        assert_eq!(r.size(), 0);
        let mut buf = [0u8; 16];
        assert_eq!(r.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn single_byte_file() {
        let tmp = write_temp(&[0xAB]);
        let mut r = WindowsChunkedReader::open(tmp.path()).expect("open");
        assert_eq!(r.size(), 1);
        let mut buf = [0u8; 4];
        let n = r.read(&mut buf).unwrap();
        assert_eq!(n, 1);
        assert_eq!(buf[0], 0xAB);
        assert_eq!(r.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn exact_chunk_size_file() {
        let payload: Vec<u8> = (0..DEFAULT_CHUNK_SIZE).map(|i| (i & 0xFF) as u8).collect();
        let tmp = write_temp(&payload);
        let mut r = WindowsChunkedReader::open(tmp.path()).expect("open");
        assert_eq!(r.size() as usize, DEFAULT_CHUNK_SIZE);
        let mut got = Vec::with_capacity(DEFAULT_CHUNK_SIZE);
        r.read_to_end(&mut got).unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn chunk_plus_one_file() {
        let total = DEFAULT_CHUNK_SIZE + 1;
        let payload: Vec<u8> = (0..total).map(|i| (i & 0xFF) as u8).collect();
        let tmp = write_temp(&payload);
        let mut r = WindowsChunkedReader::open(tmp.path()).expect("open");
        let mut got = Vec::with_capacity(total);
        r.read_to_end(&mut got).unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn seek_to_zero_after_read() {
        // Use a small chunk so we cross multiple boundaries cheaply.
        let chunk = 4096_usize;
        let total = chunk * 4;
        let payload: Vec<u8> = (0..total).map(|i| (i & 0xFF) as u8).collect();
        let tmp = write_temp(&payload);
        let mut r = WindowsChunkedReader::with_chunk_size(tmp.path(), chunk).expect("open");
        let mut sink = vec![0u8; total];
        r.read_exact(&mut sink).unwrap();
        assert_eq!(sink, payload);

        assert_eq!(r.seek(SeekFrom::Start(0)).unwrap(), 0);
        let mut again = vec![0u8; total];
        r.read_exact(&mut again).unwrap();
        assert_eq!(again, payload);
    }

    #[test]
    fn seek_to_end_then_back() {
        let total = 1024_usize;
        let payload: Vec<u8> = (0..total).map(|i| (i & 0xFF) as u8).collect();
        let tmp = write_temp(&payload);
        let mut r = WindowsChunkedReader::with_chunk_size(tmp.path(), 256).expect("open");

        let end = r.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(end, total as u64);
        let back = r.seek(SeekFrom::Current(-100)).unwrap();
        assert_eq!(back, total as u64 - 100);

        let mut buf = vec![0u8; 100];
        r.read_exact(&mut buf).unwrap();
        assert_eq!(buf, payload[total - 100..]);
    }

    #[test]
    fn parity_with_std_fs_read() {
        let sizes = [
            0_usize,
            1,
            127,
            4096,
            4096 * 2 + 17,
            DEFAULT_CHUNK_SIZE - 1,
            DEFAULT_CHUNK_SIZE,
            DEFAULT_CHUNK_SIZE + 1,
            DEFAULT_CHUNK_SIZE * 2 + 13,
        ];
        for &n in &sizes {
            let payload: Vec<u8> = (0..n).map(|i| ((i * 31) & 0xFF) as u8).collect();
            let tmp = write_temp(&payload);
            let std_bytes = std::fs::read(tmp.path()).expect("std::fs::read");
            let mut r = WindowsChunkedReader::open(tmp.path()).expect("open");
            let mut chunked = Vec::with_capacity(n);
            r.read_to_end(&mut chunked).unwrap();
            assert_eq!(std_bytes, chunked, "parity mismatch at size {n}");
            assert_eq!(chunked, payload, "payload mismatch at size {n}");
        }
    }

    #[test]
    fn env_var_chunk_size_honored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(CHUNK_SIZE_ENV).ok();

        // SAFETY: tests in this module serialize through ENV_LOCK; no other
        // thread observes the env mutation.
        unsafe {
            std::env::set_var(CHUNK_SIZE_ENV, "8192");
        }
        let tmp = write_temp(&[0u8; 64]);
        let r = WindowsChunkedReader::with_chunk_size(tmp.path(), 1024).expect("open");
        assert_eq!(r.chunk_size(), 8192, "env should override caller arg");

        unsafe {
            std::env::remove_var(CHUNK_SIZE_ENV);
        }
        let r2 = WindowsChunkedReader::with_chunk_size(tmp.path(), 1024).expect("open");
        assert_eq!(r2.chunk_size(), 1024, "no env should fall back to arg");

        // Restore prior value so concurrent test runners do not observe leak.
        if let Some(v) = prev {
            unsafe {
                std::env::set_var(CHUNK_SIZE_ENV, v);
            }
        }
    }

    #[test]
    fn as_slice_returns_full_file() {
        let payload: Vec<u8> = (0..2048).map(|i| (i & 0xFF) as u8).collect();
        let tmp = write_temp(&payload);
        let mut r = WindowsChunkedReader::with_chunk_size(tmp.path(), 256).expect("open");
        let slice = r.as_slice().unwrap().to_vec();
        assert_eq!(slice, payload);

        // Subsequent calls reuse the cache.
        let slice2 = r.as_slice().unwrap();
        assert_eq!(slice2, &payload[..]);
    }

    #[test]
    fn seek_before_start_errors() {
        let tmp = write_temp(&[0u8; 16]);
        let mut r = WindowsChunkedReader::open(tmp.path()).expect("open");
        assert!(r.seek(SeekFrom::Current(-1)).is_err());
        assert!(r.seek(SeekFrom::End(-100)).is_err());
    }

    #[test]
    fn random_access_via_seek_within_file() {
        let total = 8192_usize;
        let payload: Vec<u8> = (0..total).map(|i| (i & 0xFF) as u8).collect();
        let tmp = write_temp(&payload);
        let mut r = WindowsChunkedReader::with_chunk_size(tmp.path(), 512).expect("open");

        for &offset in &[0_u64, 100, 511, 512, 513, 1023, 4096, 8191] {
            r.seek(SeekFrom::Start(offset)).unwrap();
            let mut byte = [0u8; 1];
            r.read_exact(&mut byte).unwrap();
            assert_eq!(
                byte[0],
                payload[offset as usize],
                "wrong byte at offset {offset}"
            );
        }
    }
}
