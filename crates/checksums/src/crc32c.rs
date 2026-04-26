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
//! # Runtime Dispatch Ladder
//!
//! Hardware acceleration is provided by the [`crc32c`] crate, which performs
//! runtime CPU feature detection on the first call and caches the result. The
//! dispatch order is:
//!
//! 1. **x86_64 SSE4.2** - `crc32` instruction (8 bytes/iteration on x86_64,
//!    4 bytes/iteration on x86) when `is_x86_feature_detected!("sse4.2")`.
//! 2. **aarch64 CRC extension** - `crc32cb`/`crc32ch`/`crc32cw`/`crc32cx`
//!    instructions when `is_aarch64_feature_detected!("crc")`.
//! 3. **Software fallback** - portable byte-at-a-time table lookup using the
//!    Castagnoli polynomial.
//!
//! All paths produce byte-identical output. Parity is exercised by the
//! `streaming_random_buffer_matches_one_shot` and `streaming_chunk_sizes_match_one_shot`
//! tests below, which feed the same data through both single-shot and chunked
//! streaming paths regardless of which backend the runtime selects.
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
/// ```
/// use checksums::crc32c::{crc32c_file, crc32c_bytes};
///
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("test.txt");
/// std::fs::write(&path, b"hello world").unwrap();
///
/// let file_checksum = crc32c_file(&path).unwrap();
/// assert_eq!(file_checksum, crc32c_bytes(b"hello world"));
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

    // --- RFC 3720 / iSCSI standard CRC32C test vectors ---

    #[test]
    fn empty_data_returns_zero() {
        assert_eq!(crc32c_bytes(b""), 0);
    }

    #[test]
    fn known_32_bytes_of_zeros() {
        // 32 bytes of zeros - verified against crc32c crate reference.
        let data = [0u8; 32];
        assert_eq!(crc32c_bytes(&data), 0x8A9136AA);
    }

    #[test]
    fn known_32_bytes_of_0xff() {
        // 32 bytes of 0xFF - verified against crc32c crate reference.
        let data = [0xFFu8; 32];
        assert_eq!(crc32c_bytes(&data), 0x62A8AB43);
    }

    #[test]
    fn known_ascending_bytes() {
        // 32 bytes ascending 0x00..=0x1F - verified against crc32c crate reference.
        let data: Vec<u8> = (0x00u8..=0x1F).collect();
        assert_eq!(crc32c_bytes(&data), 0x46DD794E);
    }

    #[test]
    fn known_descending_bytes() {
        // 32 bytes descending 0x1F..=0x00 - verified against crc32c crate reference.
        let data: Vec<u8> = (0x00u8..=0x1F).rev().collect();
        assert_eq!(crc32c_bytes(&data), 0x113FDB5C);
    }

    // --- Single byte tests ---

    #[test]
    fn single_byte_zero() {
        let checksum = crc32c_bytes(&[0x00]);
        assert_ne!(checksum, 0);
        // Verify determinism.
        assert_eq!(checksum, crc32c_bytes(&[0x00]));
    }

    #[test]
    fn single_byte_0xff() {
        let checksum = crc32c_bytes(&[0xFF]);
        assert_ne!(checksum, 0);
        assert_ne!(checksum, crc32c_bytes(&[0x00]));
    }

    #[test]
    fn each_single_byte_is_unique() {
        // Every distinct single byte must produce a distinct CRC32C.
        let checksums: Vec<u32> = (0u8..=255).map(|b| crc32c_bytes(&[b])).collect();
        let mut sorted = checksums.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 256);
    }

    // --- Known string values ---

    #[test]
    fn known_value_hello_world() {
        // CRC32C("hello world") - verified against reference implementations.
        assert_eq!(crc32c_bytes(b"hello world"), 0xC99465AA);
    }

    #[test]
    fn known_value_123456789() {
        // CRC32C of the ASCII digits "123456789" - the classic check value
        // for the Castagnoli polynomial.
        assert_eq!(crc32c_bytes(b"123456789"), 0xE3069283);
    }

    // --- Streaming hasher ---

    #[test]
    fn streaming_matches_one_shot() {
        let data = b"The quick brown fox jumps over the lazy dog";
        let one_shot = crc32c_bytes(data);

        let mut hasher = Crc32cHasher::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..20]);
        hasher.update(&data[20..]);
        assert_eq!(hasher.finalize(), one_shot);
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
    fn streaming_empty_updates_are_noop() {
        let mut hasher = Crc32cHasher::new();
        hasher.update(b"data");
        let before = hasher.clone().finalize();
        hasher.update(b"");
        hasher.update(b"");
        assert_eq!(hasher.finalize(), before);
    }

    #[test]
    fn streaming_all_empty_equals_zero() {
        let mut hasher = Crc32cHasher::new();
        hasher.update(b"");
        hasher.update(b"");
        assert_eq!(hasher.finalize(), 0);
    }

    // --- Differentiation ---

    #[test]
    fn different_data_different_checksums() {
        assert_ne!(crc32c_bytes(b"aaa"), crc32c_bytes(b"bbb"));
    }

    #[test]
    fn order_matters() {
        assert_ne!(crc32c_bytes(b"ab"), crc32c_bytes(b"ba"));
    }

    #[test]
    fn length_matters() {
        // "a" vs "aa" - CRC32C distinguishes different lengths.
        assert_ne!(crc32c_bytes(b"a"), crc32c_bytes(b"aa"));
    }

    // --- with_initial ---

    #[test]
    fn with_initial_chains_correctly() {
        let data = b"hello world";
        let mid = 5;

        let full = crc32c_bytes(data);

        let partial = crc32c_bytes(&data[..mid]);
        let mut hasher = Crc32cHasher::with_initial(partial);
        hasher.update(&data[mid..]);
        assert_eq!(hasher.finalize(), full);
    }

    #[test]
    fn with_initial_zero_equals_new() {
        let a = Crc32cHasher::new();
        let b = Crc32cHasher::with_initial(0);
        assert_eq!(a.finalize(), b.finalize());
    }

    #[test]
    fn with_initial_nonzero_differs_from_new() {
        let mut a = Crc32cHasher::new();
        a.update(b"test");

        let mut b = Crc32cHasher::with_initial(0xDEADBEEF);
        b.update(b"test");

        assert_ne!(a.finalize(), b.finalize());
    }

    #[test]
    fn with_initial_no_update_returns_initial() {
        let initial = 0x12345678;
        let hasher = Crc32cHasher::with_initial(initial);
        // Finalize without any update should return the initial value since
        // CRC32C of zero-length data appended to a state preserves the state.
        assert_eq!(hasher.finalize(), initial);
    }

    // --- Default / Clone / Debug trait coverage ---

    #[test]
    fn default_trait_matches_new() {
        let a = Crc32cHasher::new();
        let b = Crc32cHasher::default();
        assert_eq!(a.finalize(), b.finalize());
    }

    #[test]
    fn clone_preserves_state() {
        let mut hasher = Crc32cHasher::new();
        hasher.update(b"partial");
        let cloned = hasher.clone();
        assert_eq!(hasher.finalize(), cloned.finalize());
    }

    #[test]
    fn clone_divergence() {
        let mut hasher = Crc32cHasher::new();
        hasher.update(b"shared");

        let mut fork_a = hasher.clone();
        let mut fork_b = hasher;

        fork_a.update(b"path_a");
        fork_b.update(b"path_b");

        assert_ne!(fork_a.finalize(), fork_b.finalize());
    }

    #[test]
    fn debug_format_contains_state() {
        let hasher = Crc32cHasher::new();
        let debug = format!("{hasher:?}");
        assert!(debug.contains("Crc32cHasher"));
        assert!(debug.contains("state"));
    }

    // --- File checksum ---

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
        assert_eq!(file_checksum, crc32c_bytes(data));
    }

    #[test]
    fn crc32c_file_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        File::create(&path).unwrap();

        assert_eq!(crc32c_file(&path).unwrap(), 0);
    }

    #[test]
    fn crc32c_file_nonexistent_returns_error() {
        let result = crc32c_file(Path::new("/nonexistent/path/file.bin"));
        assert!(result.is_err());
    }

    #[test]
    fn crc32c_file_single_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.bin");
        std::fs::write(&path, [0x42]).unwrap();

        assert_eq!(crc32c_file(&path).unwrap(), crc32c_bytes(&[0x42]));
    }

    #[test]
    fn crc32c_file_large_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.bin");

        // Data larger than BUF_SIZE to exercise multi-chunk reading.
        let data: Vec<u8> = (0u8..=255).cycle().take(BUF_SIZE * 3 + 42).collect();
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(&data).unwrap();
        }

        assert_eq!(crc32c_file(&path).unwrap(), crc32c_bytes(&data));
    }

    #[test]
    fn crc32c_file_exactly_buf_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exact.bin");

        // Exactly BUF_SIZE bytes - boundary condition for the read loop.
        let data: Vec<u8> = (0u8..=255).cycle().take(BUF_SIZE).collect();
        std::fs::write(&path, &data).unwrap();

        assert_eq!(crc32c_file(&path).unwrap(), crc32c_bytes(&data));
    }

    #[test]
    fn crc32c_file_buf_size_plus_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plus1.bin");

        // BUF_SIZE + 1 bytes - just past the first chunk boundary.
        let data: Vec<u8> = (0u8..=255).cycle().take(BUF_SIZE + 1).collect();
        std::fs::write(&path, &data).unwrap();

        assert_eq!(crc32c_file(&path).unwrap(), crc32c_bytes(&data));
    }

    // --- Edge case patterns ---

    #[test]
    fn all_zeros_pattern() {
        let data = [0u8; 1024];
        let checksum = crc32c_bytes(&data);
        assert_ne!(checksum, 0);
        // Must differ from shorter all-zeros.
        assert_ne!(checksum, crc32c_bytes(&[0u8; 32]));
    }

    #[test]
    fn all_ones_pattern() {
        let data = [0xFFu8; 1024];
        let checksum = crc32c_bytes(&data);
        assert_ne!(checksum, 0);
        assert_ne!(checksum, crc32c_bytes(&[0xFFu8; 32]));
    }

    #[test]
    fn repeating_pattern_sensitivity() {
        // CRC32C should distinguish different repeating patterns.
        let a: Vec<u8> = [0xAA, 0x55].iter().copied().cycle().take(128).collect();
        let b: Vec<u8> = [0x55, 0xAA].iter().copied().cycle().take(128).collect();
        assert_ne!(crc32c_bytes(&a), crc32c_bytes(&b));
    }

    #[test]
    fn determinism_across_calls() {
        let data = b"determinism check";
        let first = crc32c_bytes(data);
        for _ in 0..100 {
            assert_eq!(crc32c_bytes(data), first);
        }
    }

    #[test]
    fn large_input_4mb() {
        // 4 MiB of data - exercises multiple BUF_SIZE iterations and ensures
        // no overflow or accumulation errors in the CRC state.
        let data: Vec<u8> = (0u8..=255).cycle().take(4 * 1024 * 1024).collect();

        let one_shot = crc32c_bytes(&data);

        let mut hasher = Crc32cHasher::new();
        for chunk in data.chunks(BUF_SIZE) {
            hasher.update(chunk);
        }
        assert_eq!(hasher.finalize(), one_shot);
    }

    /// Dispatch parity: a 16 KiB pseudo-random buffer must produce identical
    /// digests via the streaming path and the one-shot path regardless of
    /// which backend (SSE4.2, aarch64 CRC, or software) the runtime selects.
    /// Both paths funnel through the same `crc32c` crate dispatcher; this
    /// test guards against regressions where the streaming wrapper diverges
    /// from the one-shot implementation.
    #[test]
    fn streaming_random_buffer_matches_one_shot() {
        // Deterministic pseudo-random pattern - reproducible across CI runs.
        let data: Vec<u8> = (0u32..16 * 1024)
            .map(|i| (i.wrapping_mul(2654435761) >> 16) as u8)
            .collect();

        let one_shot = crc32c_bytes(&data);

        let mut hasher = Crc32cHasher::new();
        hasher.update(&data);
        let streamed_single = hasher.finalize();
        assert_eq!(streamed_single, one_shot);
    }

    /// Dispatch parity across chunk sizes: feed the same 16 KiB buffer through
    /// the streaming hasher in chunks of varying sizes (1 byte up to several
    /// KiB) and assert every chunking produces the same digest as the one-shot
    /// call. Catches state-machine bugs in the streaming wrapper that the
    /// fixed-size `large_input_4mb` test cannot reach.
    #[test]
    fn streaming_chunk_sizes_match_one_shot() {
        let data: Vec<u8> = (0u32..16 * 1024)
            .map(|i| (i.wrapping_mul(2246822519) >> 8) as u8)
            .collect();

        let expected = crc32c_bytes(&data);

        for chunk_size in [1usize, 3, 7, 16, 64, 128, 1023, 1024, 4096, 8191] {
            let mut hasher = Crc32cHasher::new();
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            assert_eq!(
                hasher.finalize(),
                expected,
                "CRC32C streaming/one-shot mismatch at chunk_size={chunk_size}"
            );
        }
    }

    /// RFC 3720 / iSCSI canonical test vectors exercised through the streaming
    /// API. Pairs with `known_value_123456789` (which uses the one-shot path)
    /// to confirm the streaming wrapper agrees with the canonical check value.
    #[test]
    fn streaming_canonical_vectors_match() {
        let vectors: &[(&[u8], u32)] = &[
            (b"", 0),
            (b"123456789", 0xE3069283),
            (b"hello world", 0xC99465AA),
        ];

        for (input, expected) in vectors {
            let mut hasher = Crc32cHasher::new();
            hasher.update(input);
            assert_eq!(
                hasher.finalize(),
                *expected,
                "CRC32C streaming canonical mismatch for {input:?}"
            );
        }
    }
}
