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
//! default of 4 MiB matches the IOCP file-reader page-aligned slab. Explicit
//! per-instance overrides go through [`WindowsChunkedReader::open_with_chunk_size`]
//! and take precedence over the environment variable.
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
use std::sync::OnceLock;

/// Default chunk size for streaming reads (4 MiB).
///
/// Matches the IOCP file-reader slab so a Windows host that already has IOCP
/// page-aligned buffers in flight does not double its working set when the
/// chunked reader is active.
pub const DEFAULT_CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Hard lower bound on chunk size accepted by the validated constructors
/// (4 KiB).
///
/// Matches the smallest Windows page-aligned slab IOCP issues, below which
/// per-read syscall overhead dominates and the bounded-RSS contract stops
/// providing useful headroom. Any value below this bound is rejected by the
/// validated constructors and treated as an invalid env override.
pub const MIN_CHUNK_SIZE: usize = 4 * 1024;

/// Hard upper bound on chunk size accepted by the validated constructors
/// (64 MiB).
///
/// The cap exists to keep the bounded-RSS contract meaningful: a caller that
/// passes a chunk size larger than this is asking for behavior closer to the
/// legacy `mmap_reader_stub::MmapReader` slurp than to a streaming reader, and
/// would defeat the WIN-S.LAND.1.b regression budget. 64 MiB leaves ample
/// headroom above the 4 MiB default for benchmarking sweeps without permitting
/// pathological values.
pub const MAX_CHUNK_SIZE: usize = 64 * 1024 * 1024;

/// Environment variable name for overriding the chunk size at runtime.
///
/// Set to a power of two within `MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE` (4 KiB to
/// 64 MiB inclusive). Unset, empty, non-numeric, non-power-of-two, or
/// out-of-range values fall back to [`DEFAULT_CHUNK_SIZE`]; invalid overrides
/// emit a `tracing::warn!` note so operators tuning the knob can see why
/// their value was ignored. Only consulted by [`WindowsChunkedReader::open`];
/// explicit per-instance overrides via
/// [`WindowsChunkedReader::open_with_chunk_size`] ignore the env var.
///
/// Read once at process startup via [`OnceLock`]; subsequent `open()` calls
/// reuse the cached result and never touch the environment again, matching
/// the pattern documented on
/// `engine::concurrent_delta::parallel_apply::ring_cap_env::RING_CAP_ENV`.
pub const CHUNK_SIZE_ENV: &str = "OC_RSYNC_WIN_CHUNK_BYTES";

/// Sentinel meaning "no chunk currently loaded".
const NO_CHUNK_LOADED: u64 = u64::MAX;

/// Process-wide cache for the parsed env override.
///
/// `None` after initialisation means the env var was unset or invalid;
/// callers fall back to [`DEFAULT_CHUNK_SIZE`]. `Some(n)` means the env var
/// was a valid power-of-two within `MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE` and
/// every `open()` will use `n` as the chunk size.
static ENV_OVERRIDE: OnceLock<Option<usize>> = OnceLock::new();

/// Validates `chunk_size` for the explicit constructors.
///
/// Accepts any positive value up to [`MAX_CHUNK_SIZE`] so unit tests and
/// benchmarks can pick small or non-power-of-two sizes to exercise chunk
/// boundaries cheaply. The stricter env-var validation
/// ([`is_valid_env_chunk_size`]) layers a power-of-two and lower-bound check
/// on top so a misconfigured operator does not silently land on an
/// unaligned slab.
fn validate_chunk_size(chunk_size: usize) -> io::Result<()> {
    if chunk_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "WindowsChunkedReader chunk_size must be > 0",
        ));
    }
    if chunk_size > MAX_CHUNK_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "WindowsChunkedReader chunk_size {chunk_size} exceeds MAX_CHUNK_SIZE {MAX_CHUNK_SIZE}"
            ),
        ));
    }
    Ok(())
}

/// Returns `true` when `chunk_size` satisfies the env-var contract: a power
/// of two within `MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE`.
fn is_valid_env_chunk_size(chunk_size: usize) -> bool {
    chunk_size >= MIN_CHUNK_SIZE
        && chunk_size <= MAX_CHUNK_SIZE
        && chunk_size.is_power_of_two()
}

/// Returns the cached env override, loading it from the environment on first
/// call. Subsequent calls reuse the cached value.
fn cached_env_override() -> Option<usize> {
    *ENV_OVERRIDE.get_or_init(load_chunk_size_from_env)
}

/// Reads `OC_RSYNC_WIN_CHUNK_BYTES` and returns a validated chunk size.
///
/// Returns `None` when the var is unset or empty (the common case). Returns
/// `None` and emits a `tracing::warn!` note when the var is set but invalid
/// (not UTF-8, non-numeric, not a power of two, or outside
/// `MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE`) so operators tuning the knob can see
/// why their override was ignored. Factored out so the [`OnceLock`]
/// initialiser stays a single function pointer.
fn load_chunk_size_from_env() -> Option<usize> {
    let raw = std::env::var_os(CHUNK_SIZE_ENV)?;
    let Some(text) = raw.to_str() else {
        tracing::warn!(
            env = CHUNK_SIZE_ENV,
            "WindowsChunkedReader: env value not valid UTF-8; using DEFAULT_CHUNK_SIZE"
        );
        return None;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<usize>() {
        Ok(n) if is_valid_env_chunk_size(n) => Some(n),
        Ok(n) => {
            tracing::warn!(
                env = CHUNK_SIZE_ENV,
                value = n,
                min = MIN_CHUNK_SIZE,
                max = MAX_CHUNK_SIZE,
                "WindowsChunkedReader: env value invalid (out of bounds or not a power of two); using DEFAULT_CHUNK_SIZE"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                env = CHUNK_SIZE_ENV,
                value = trimmed,
                "WindowsChunkedReader: env value not a positive integer; using DEFAULT_CHUNK_SIZE"
            );
            None
        }
    }
}

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
    /// Opens `path` for chunked reading with the [default chunk size].
    ///
    /// Resolution order for the effective chunk size:
    ///
    /// 1. `OC_RSYNC_WIN_CHUNK_BYTES`, when set to a base-10 power of two
    ///    within `MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE` (4 KiB to 64 MiB).
    /// 2. [`DEFAULT_CHUNK_SIZE`] (4 MiB).
    ///
    /// The env var is read once per process and cached via an internal
    /// [`OnceLock`]; later `open()` calls reuse the cached resolution without
    /// re-touching the environment. An unset env var is the silent common
    /// case; a set-but-invalid value (not UTF-8, non-numeric, not a power of
    /// two, or out of range) is reported via `tracing::warn!` exactly once
    /// and treated as "no override".
    ///
    /// Use [`open_with_chunk_size`](Self::open_with_chunk_size) when the caller
    /// needs an explicit, validated chunk size that ignores the env var.
    ///
    /// [default chunk size]: DEFAULT_CHUNK_SIZE
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let chunk_size = cached_env_override().unwrap_or(DEFAULT_CHUNK_SIZE);
        Self::open_inner(path.as_ref(), chunk_size)
    }

    /// Opens `path` with an explicit, validated chunk size.
    ///
    /// `chunk_size` must satisfy `0 < chunk_size <= MAX_CHUNK_SIZE` (the
    /// upper bound is 64 MiB); values outside that range return
    /// [`io::ErrorKind::InvalidInput`]. Power-of-two and minimum-size
    /// constraints apply only to the [`CHUNK_SIZE_ENV`] override path; an
    /// explicit constructor argument may pick any size in that window to
    /// make boundary tests cheap. The environment variable is **ignored**:
    /// an explicit constructor argument always wins.
    ///
    /// Use [`open`](Self::open) when the caller wants the env-aware default
    /// path instead.
    pub fn open_with_chunk_size(path: &Path, chunk_size: usize) -> io::Result<Self> {
        validate_chunk_size(chunk_size)?;
        Self::open_inner(path, chunk_size)
    }

    /// Opens `path` with a caller-specified chunk size.
    ///
    /// Equivalent to [`open_with_chunk_size`](Self::open_with_chunk_size);
    /// retained as a generic-`P` convenience for existing call sites.
    /// Validates `chunk_size` against the same `0 < n <= MAX_CHUNK_SIZE`
    /// window and ignores `OC_RSYNC_WIN_CHUNK_BYTES`.
    pub fn with_chunk_size<P: AsRef<Path>>(path: P, chunk_size: usize) -> io::Result<Self> {
        Self::open_with_chunk_size(path.as_ref(), chunk_size)
    }

    fn open_inner(path: &Path, chunk_size: usize) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            file,
            size,
            chunk_size,
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

    /// Returns the file size in bytes. Alias for [`Self::size`].
    ///
    /// Provided for ergonomics with the `Read`-trait ecosystem, where `len`
    /// is the conventional name for the total length of a readable resource.
    /// Always agrees with [`Self::size`] for a given instance.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.size
    }

    /// Returns `true` when the underlying file is empty.
    ///
    /// Companion to [`Self::len`] to satisfy the `len()`/`is_empty()`
    /// clippy convention.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.size == 0
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
            self.chunk_offset
                .saturating_add(self.chunk_cache.len() as u64)
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
    fn explicit_constructor_ignores_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(CHUNK_SIZE_ENV).ok();

        // SAFETY: tests in this module serialize through ENV_LOCK; no other
        // thread observes the env mutation.
        unsafe {
            std::env::set_var(CHUNK_SIZE_ENV, "8192");
        }
        let tmp = write_temp(&[0u8; 64]);
        let r = WindowsChunkedReader::open_with_chunk_size(tmp.path(), 1024).expect("open");
        assert_eq!(
            r.chunk_size(),
            1024,
            "explicit constructor must win over env"
        );

        let r_shim = WindowsChunkedReader::with_chunk_size(tmp.path(), 2048).expect("open");
        assert_eq!(
            r_shim.chunk_size(),
            2048,
            "with_chunk_size shim must also ignore env"
        );

        restore_env(prev);
    }

    /// Tests the parser directly so the process-wide [`ENV_OVERRIDE`] cache
    /// does not poison sibling tests in the same process. The cache itself
    /// is exercised by [`open_uses_env_override_via_cache`].
    #[test]
    fn env_parser_accepts_power_of_two_in_bounds() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(CHUNK_SIZE_ENV).ok();

        for valid in [MIN_CHUNK_SIZE, 8 * 1024, 1 << 20, MAX_CHUNK_SIZE] {
            unsafe {
                std::env::set_var(CHUNK_SIZE_ENV, valid.to_string());
            }
            assert_eq!(
                load_chunk_size_from_env(),
                Some(valid),
                "env value {valid} must round-trip"
            );
        }

        restore_env(prev);
    }

    #[test]
    fn env_parser_returns_none_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(CHUNK_SIZE_ENV).ok();

        unsafe {
            std::env::remove_var(CHUNK_SIZE_ENV);
        }
        assert_eq!(load_chunk_size_from_env(), None);

        restore_env(prev);
    }

    #[test]
    fn env_parser_rejects_invalid_values() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(CHUNK_SIZE_ENV).ok();

        // Empty / whitespace / non-numeric / negative / zero all parse to
        // None without panicking.
        for bogus in ["", "   ", "not-a-number", "-1", "0"] {
            unsafe {
                std::env::set_var(CHUNK_SIZE_ENV, bogus);
            }
            assert_eq!(
                load_chunk_size_from_env(),
                None,
                "invalid env {bogus:?} must fall back to None"
            );
        }

        // Below MIN_CHUNK_SIZE (4 KiB) fails the env contract even though
        // the explicit constructor would accept it.
        let too_small = (MIN_CHUNK_SIZE / 2).to_string();
        unsafe {
            std::env::set_var(CHUNK_SIZE_ENV, &too_small);
        }
        assert_eq!(load_chunk_size_from_env(), None);

        // Above MAX_CHUNK_SIZE (64 MiB) fails.
        let too_big = (MAX_CHUNK_SIZE * 2).to_string();
        unsafe {
            std::env::set_var(CHUNK_SIZE_ENV, &too_big);
        }
        assert_eq!(load_chunk_size_from_env(), None);

        // Power-of-two requirement: 12 KiB is in range but not a power of 2.
        let not_pow2 = (12 * 1024usize).to_string();
        unsafe {
            std::env::set_var(CHUNK_SIZE_ENV, &not_pow2);
        }
        assert_eq!(load_chunk_size_from_env(), None);

        restore_env(prev);
    }

    /// Confirms the [`ENV_OVERRIDE`] cache observably feeds [`open`]'s
    /// chunk-size resolution. Runs once because [`OnceLock`] only loads
    /// the env once per process; the parser tests above cover the rest of
    /// the matrix without touching the cache.
    #[test]
    fn open_uses_env_override_via_cache() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // If a prior test in this process already initialised the cache,
        // its value (or None) is final; assert open() agrees with whatever
        // the cache resolved to.
        let expected = cached_env_override().unwrap_or(DEFAULT_CHUNK_SIZE);
        let tmp = write_temp(&[0u8; 64]);
        let r = WindowsChunkedReader::open(tmp.path()).expect("open");
        assert_eq!(r.chunk_size(), expected);
    }

    #[test]
    fn open_with_chunk_size_rejects_zero() {
        let tmp = write_temp(&[0u8; 16]);
        let err =
            WindowsChunkedReader::open_with_chunk_size(tmp.path(), 0).expect_err("zero rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn open_with_chunk_size_rejects_over_max() {
        let tmp = write_temp(&[0u8; 16]);
        let err = WindowsChunkedReader::open_with_chunk_size(tmp.path(), MAX_CHUNK_SIZE + 1)
            .expect_err("over-max rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        // Exact MAX_CHUNK_SIZE must succeed (inclusive bound).
        let r = WindowsChunkedReader::open_with_chunk_size(tmp.path(), MAX_CHUNK_SIZE)
            .expect("MAX_CHUNK_SIZE accepted");
        assert_eq!(r.chunk_size(), MAX_CHUNK_SIZE);
    }

    #[test]
    fn explicit_chunk_size_observable_via_read_boundaries() {
        // With chunk_size = 1024 and file size 2*chunk + 1 = 2049, three
        // refills must occur to cover the whole stream. Verify by reading the
        // entire file through a sink whose buffer is exactly chunk_size long:
        // each `read()` returns at most one chunk, so the sequence of return
        // values is [chunk, chunk, 1, 0].
        let chunk = 1024_usize;
        let total = 2 * chunk + 1;
        let payload: Vec<u8> = (0..total).map(|i| (i & 0xFF) as u8).collect();
        let tmp = write_temp(&payload);
        let mut r = WindowsChunkedReader::open_with_chunk_size(tmp.path(), chunk).expect("open");

        let mut sizes = Vec::new();
        let mut buf = vec![0u8; chunk];
        let mut collected = Vec::with_capacity(total);
        loop {
            let n = r.read(&mut buf).unwrap();
            sizes.push(n);
            if n == 0 {
                break;
            }
            collected.extend_from_slice(&buf[..n]);
        }
        assert_eq!(sizes, vec![chunk, chunk, 1, 0]);
        assert_eq!(collected, payload);
    }

    /// Restores the prior `OC_RSYNC_WIN_CHUNK_BYTES` value (or removes it
    /// when none was set) so leaking env state cannot affect downstream
    /// tests in the same process.
    fn restore_env(prev: Option<String>) {
        unsafe {
            match prev {
                Some(v) => std::env::set_var(CHUNK_SIZE_ENV, v),
                None => std::env::remove_var(CHUNK_SIZE_ENV),
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

    /// Cross-platform smoke test: `WindowsChunkedReader` is nameable and
    /// openable on the current platform. The same test runs against the
    /// non-Windows alias in `windows_chunked_reader_stub::tests`.
    #[test]
    fn nameable_and_openable() {
        let tmp = write_temp(b"hello");
        let _reader = WindowsChunkedReader::open(tmp.path()).expect("open through real reader");
    }

    /// Cross-platform parity test: `len()` agrees with `size()` for a
    /// known-size fixture. The non-Windows alias verifies the same length
    /// through `metadata().len()` in `windows_chunked_reader_stub::tests`.
    #[test]
    fn len_matches_fixture_size() {
        let payload = b"0123456789abcdef";
        let tmp = write_temp(payload);
        let reader = WindowsChunkedReader::open(tmp.path()).expect("open");
        assert_eq!(reader.len(), payload.len() as u64);
        assert_eq!(reader.len(), reader.size());
        assert!(!reader.is_empty());
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
                byte[0], payload[offset as usize],
                "wrong byte at offset {offset}"
            );
        }
    }
}
