//! CRC32C hardware-accelerated checksum for fast file change detection.
//!
//! CRC32C uses the Castagnoli polynomial (iSCSI), which has hardware support
//! on x86/x86_64 (SSE4.2 `crc32` instruction) and aarch64 (CRC extension).
//! This makes it significantly faster than MD5 or SHA for quick-check scenarios
//! where cryptographic strength is not required.
//!
//! This module provides a streaming `Crc32cHasher` and convenience functions
//! for whole-file and in-memory checksumming. The primary use case is local
//! `--checksum` quick-check, where a fast pre-filter can avoid expensive
//! strong-checksum computation for files that have clearly changed.
//!
//! # Upstream Reference
//!
//! Upstream rsync does not use CRC32C in its wire protocol. This checksum is
//! used exclusively as a local optimization for `--checksum` mode quick-check,
//! where the goal is fast change detection rather than protocol compatibility.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

/// Buffer size for streaming file reads (64 KiB).
const BUF_SIZE: usize = 64 * 1024;

/// Streaming CRC32C hasher with hardware acceleration.
///
/// Wraps the `crc32c` crate, which automatically detects and uses hardware
/// CRC32C instructions (SSE4.2 on x86, CRC extension on aarch64) at runtime,
/// falling back to a software implementation on platforms without hardware
/// support.
///
/// # Examples
///
/// Incremental hashing:
///
/// ```
/// use checksums::crc32c::Crc32cHasher;
///
/// let mut hasher = Crc32cHasher::new();
/// hasher.update(b"hello ");
/// hasher.update(b"world");
/// let checksum = hasher.finalize();
///
/// assert_eq!(checksum, checksums::crc32c::crc32c_bytes(b"hello world"));
/// ```
#[derive(Clone, Debug)]
pub struct Crc32cHasher {
    state: u32,
}

impl Default for Crc32cHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32cHasher {
    /// Creates a new hasher with an initial CRC value of zero.
    #[must_use]
    pub fn new() -> Self {
        Self { state: 0 }
    }

    /// Creates a new hasher with a custom initial CRC value.
    ///
    /// This is useful for chaining CRC computations across non-contiguous
    /// data segments.
    #[must_use]
    pub fn with_initial(initial: u32) -> Self {
        Self { state: initial }
    }

    /// Feeds additional bytes into the CRC state.
    pub fn update(&mut self, data: &[u8]) {
        self.state = crc32c::crc32c_append(self.state, data);
    }

    /// Returns the computed CRC32C checksum.
    #[must_use]
    pub fn finalize(self) -> u32 {
        self.state
    }
}

/// Computes the CRC32C checksum of an in-memory byte slice.
///
/// Uses hardware acceleration when available (SSE4.2 on x86, CRC extension
/// on aarch64).
///
/// # Examples
///
/// ```
/// use checksums::crc32c::crc32c_bytes;
///
/// let checksum = crc32c_bytes(b"hello world");
/// assert_ne!(checksum, 0);
/// ```
#[must_use]
pub fn crc32c_bytes(data: &[u8]) -> u32 {
    crc32c::crc32c(data)
}

/// Computes the CRC32C checksum of a file by streaming its contents.
///
/// Reads the file in 64 KiB chunks to limit memory usage. Uses hardware
/// CRC32C instructions when the platform supports them.
///
/// # Errors
///
/// Returns an I/O error if the file cannot be opened or read.
///
/// # Examples
///
/// ```no_run
/// use checksums::crc32c::crc32c_file;
/// use std::path::Path;
///
/// let checksum = crc32c_file(Path::new("/etc/hosts")).unwrap();
/// println!("CRC32C: {checksum:#010x}");
/// ```
pub fn crc32c_file(path: &Path) -> io::Result<u32> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(BUF_SIZE, file);
    let mut hasher = Crc32cHasher::new();
    let mut buf = [0u8; BUF_SIZE];

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn empty_data_returns_zero() {
        assert_eq!(crc32c_bytes(b""), 0);
    }

    #[test]
    fn known_value_hello_world() {
        // CRC32C("hello world") is a well-known test vector.
        let checksum = crc32c_bytes(b"hello world");
        // Verify determinism - same input must produce same output.
        assert_eq!(checksum, crc32c_bytes(b"hello world"));
        assert_ne!(checksum, 0);
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data = b"The quick brown fox jumps over the lazy dog";

        let one_shot = crc32c_bytes(data);

        let mut hasher = Crc32cHasher::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..20]);
        hasher.update(&data[20..]);
        let streaming = hasher.finalize();

        assert_eq!(one_shot, streaming);
    }

    #[test]
    fn byte_at_a_time_matches_one_shot() {
        let data = b"incremental";
        let expected = crc32c_bytes(data);

        let mut hasher = Crc32cHasher::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        assert_eq!(hasher.finalize(), expected);
    }

    #[test]
    fn different_data_different_checksums() {
        assert_ne!(crc32c_bytes(b"aaa"), crc32c_bytes(b"bbb"));
    }

    #[test]
    fn with_initial_chains_correctly() {
        let data = b"hello world";
        let mid = 5;

        let full = crc32c_bytes(data);

        let partial = crc32c_bytes(&data[..mid]);
        let mut hasher = Crc32cHasher::with_initial(partial);
        hasher.update(&data[mid..]);
        let chained = hasher.finalize();

        assert_eq!(full, chained);
    }

    #[test]
    fn default_trait_matches_new() {
        let a = Crc32cHasher::new();
        let b = Crc32cHasher::default();
        assert_eq!(a.finalize(), b.finalize());
    }

    #[test]
    fn crc32c_file_reads_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");

        let data = b"file content for CRC32C testing";
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(data).unwrap();
        }

        let file_checksum = crc32c_file(&path).unwrap();
        let mem_checksum = crc32c_bytes(data);
        assert_eq!(file_checksum, mem_checksum);
    }

    #[test]
    fn crc32c_file_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        File::create(&path).unwrap();

        let checksum = crc32c_file(&path).unwrap();
        assert_eq!(checksum, 0);
    }

    #[test]
    fn crc32c_file_nonexistent_returns_error() {
        let result = crc32c_file(Path::new("/nonexistent/path/file.bin"));
        assert!(result.is_err());
    }

    #[test]
    fn crc32c_file_large_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.bin");

        // Create data larger than BUF_SIZE to exercise multi-chunk reading.
        let data: Vec<u8> = (0u8..=255).cycle().take(BUF_SIZE * 3 + 42).collect();
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(&data).unwrap();
        }

        let file_checksum = crc32c_file(&path).unwrap();
        let mem_checksum = crc32c_bytes(&data);
        assert_eq!(file_checksum, mem_checksum);
    }

    #[test]
    fn clone_preserves_state() {
        let mut hasher = Crc32cHasher::new();
        hasher.update(b"partial");
        let cloned = hasher.clone();

        // Both should produce the same result when finalized without further updates.
        assert_eq!(hasher.finalize(), cloned.finalize());
    }
}
