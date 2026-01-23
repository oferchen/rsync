//! # checksums
//!
//! Rolling and strong checksum primitives for the Rust rsync implementation.
//!
//! This crate provides the cryptographic and non-cryptographic hash algorithms
//! used by rsync for delta-transfer operations. All implementations are
//! byte-for-byte compatible with upstream rsync 3.4.1, ensuring interoperability
//! with the C reference implementation.
//!
//! # Quick Start
//!
//! ```rust
//! use checksums::{RollingChecksum, RollingDigest};
//! use checksums::strong::{Md5, Sha256, Xxh3, StrongDigest};
//!
//! // Rolling checksum for block matching
//! let mut rolling = RollingChecksum::new();
//! rolling.update(b"file block data");
//! let weak_hash = rolling.value();
//!
//! // Strong checksum for collision verification
//! let strong_hash = Sha256::digest(b"file block data");
//! ```
//!
//! # Modules
//!
//! - [`strong`] - Strong checksum algorithms (MD4, MD5, SHA-1, SHA-256, SHA-512, XXH64, XXH3)
//! - `parallel` - Parallel checksum computation using rayon (requires `parallel` feature)
//!
//! # Checksum Algorithms
//!
//! ## Rolling Checksum (Weak Hash)
//!
//! The [`RollingChecksum`] implements rsync's Adler-32 style weak checksum,
//! enabling O(1) sliding window updates for efficient delta detection.
//!
//! | Property | Value |
//! |----------|-------|
//! | Output size | 32 bits |
//! | Window update | O(1) |
//! | SIMD acceleration | AVX2, SSE2, NEON |
//!
//! ```rust
//! use checksums::RollingChecksum;
//!
//! let mut checksum = RollingChecksum::new();
//! checksum.update(b"ABCD");
//!
//! // O(1) window slide: remove 'A', add 'E'
//! checksum.roll(b'A', b'E').unwrap();
//! ```
//!
//! ## Strong Checksums
//!
//! Strong checksums provide collision resistance for verifying block matches.
//! The rsync protocol negotiates which algorithm to use based on version and
//! capabilities.
//!
//! ### MD4 (Legacy)
//!
//! | Property | Value |
//! |----------|-------|
//! | Output size | 128 bits (16 bytes) |
//! | Use case | rsync protocol < 30 |
//! | Security | Cryptographically broken - legacy use only |
//! | Performance | ~400 MB/s (pure Rust), ~800 MB/s (OpenSSL) |
//!
//! ```rust
//! use checksums::strong::Md4;
//!
//! let digest = Md4::digest(b"data");
//! assert_eq!(digest.len(), 16);
//! ```
//!
//! ### MD5
//!
//! | Property | Value |
//! |----------|-------|
//! | Output size | 128 bits (16 bytes) |
//! | Use case | rsync protocol < 30, file list validation |
//! | Security | Cryptographically broken - legacy use only |
//! | Performance | ~500 MB/s (pure Rust), ~1 GB/s (OpenSSL) |
//!
//! MD5 supports seeded hashing for rsync's `CHECKSUM_SEED_FIX` compatibility:
//!
//! ```rust
//! use checksums::strong::{Md5, Md5Seed, StrongDigest};
//!
//! // Unseeded (default)
//! let digest = Md5::digest(b"data");
//!
//! // Seeded with proper ordering (protocol 30+)
//! let mut seeded = Md5::with_seed(Md5Seed::proper(0x12345678));
//! seeded.update(b"data");
//! let seeded_digest = seeded.finalize();
//! ```
//!
//! ### SHA-1
//!
//! | Property | Value |
//! |----------|-------|
//! | Output size | 160 bits (20 bytes) |
//! | Use case | Negotiated for stronger security |
//! | Security | Collision attacks known - use SHA-256 for new deployments |
//! | Performance | ~600 MB/s (with SHA-NI), ~300 MB/s (scalar) |
//!
//! ```rust
//! use checksums::strong::Sha1;
//!
//! let digest = Sha1::digest(b"data");
//! assert_eq!(digest.len(), 20);
//! ```
//!
//! ### SHA-256
//!
//! | Property | Value |
//! |----------|-------|
//! | Output size | 256 bits (32 bytes) |
//! | Use case | Daemon authentication, high-security transfers |
//! | Security | Cryptographically secure |
//! | Performance | ~800 MB/s (with SHA-NI), ~200 MB/s (scalar) |
//!
//! ```rust
//! use checksums::strong::Sha256;
//!
//! let digest = Sha256::digest(b"secure data");
//! assert_eq!(digest.len(), 32);
//! ```
//!
//! ### XXH64
//!
//! | Property | Value |
//! |----------|-------|
//! | Output size | 64 bits (8 bytes) |
//! | Use case | rsync protocol >= 30, fast block matching |
//! | Security | Non-cryptographic |
//! | Performance | ~10 GB/s |
//!
//! XXHash variants support seeding for protocol-specific initialization:
//!
//! ```rust
//! use checksums::strong::Xxh64;
//!
//! let seed: u64 = 0x12345678;
//! let digest = Xxh64::digest(seed, b"data");
//! assert_eq!(digest.len(), 8);
//! ```
//!
//! ### XXH3 (64-bit and 128-bit)
//!
//! | Property | XXH3-64 | XXH3-128 |
//! |----------|---------|----------|
//! | Output size | 64 bits (8 bytes) | 128 bits (16 bytes) |
//! | Use case | Fast checksums | Reduced collision probability |
//! | Security | Non-cryptographic | Non-cryptographic |
//! | Performance | ~15 GB/s (SIMD) | ~15 GB/s (SIMD) |
//!
//! When the `xxh3-simd` feature is enabled (default), one-shot operations use
//! runtime SIMD detection for AVX2 (x86_64) and NEON (aarch64):
//!
//! ```rust
//! use checksums::strong::{Xxh3, Xxh3_128};
//!
//! // Check if runtime SIMD is available
//! if checksums::xxh3_simd_available() {
//!     println!("Using SIMD-accelerated XXH3");
//! }
//!
//! let xxh3_64 = Xxh3::digest(0, b"data");
//! let xxh3_128 = Xxh3_128::digest(0, b"data");
//! ```
//!
//! # Feature Flags
//!
//! | Feature | Default | Description |
//! |---------|---------|-------------|
//! | `xxh3-simd` | Yes | Runtime SIMD detection for XXH3 (AVX2/NEON) |
//! | `openssl` | No | OpenSSL-backed MD4/MD5 for ~2x throughput |
//! | `openssl-vendored` | No | Statically link OpenSSL (includes `openssl`) |
//! | `parallel` | No | Parallel checksum computation via rayon |
//!
//! ## Feature Details
//!
//! ### `xxh3-simd` (default)
//!
//! Enables the [`xxh3`](https://crates.io/crates/xxh3) crate for runtime SIMD
//! detection. This allows portable binaries to automatically use AVX2 or NEON
//! instructions when available, providing ~3x speedup over scalar code.
//!
//! Use [`xxh3_simd_available()`] to query if runtime SIMD detection is enabled.
//!
//! ### `openssl` / `openssl-vendored`
//!
//! Enables OpenSSL-backed implementations of MD4 and MD5. While these legacy
//! algorithms are cryptographically broken, OpenSSL's optimized implementations
//! provide approximately 2x throughput improvement, which matters for large
//! file transfers where rsync protocol compatibility requires these hashes.
//!
//! Use [`openssl_acceleration_available()`] to query OpenSSL availability at runtime.
//!
//! ### `parallel`
//!
//! Enables the `parallel` module for concurrent checksum computation using
//! [rayon](https://crates.io/crates/rayon). This is useful when computing
//! checksums for many blocks simultaneously:
//!
#![cfg_attr(feature = "parallel", doc = "```rust")]
#![cfg_attr(not(feature = "parallel"), doc = "```ignore")]
//! use checksums::parallel::{compute_block_signatures_parallel, BlockSignature};
//! use checksums::strong::Sha256;
//!
//! let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
//! let signatures = compute_block_signatures_parallel::<Sha256, _>(&blocks);
//!
//! for sig in &signatures {
//!     println!("Rolling: {:08x}", sig.rolling);
//! }
//! ```
//!
//! # The ChecksumFactory Pattern (StrongDigest Trait)
//!
//! The [`strong::StrongDigest`] trait provides a factory pattern for creating
//! and using strong checksum algorithms. This enables generic programming over
//! different hash algorithms while maintaining type safety.
//!
//! ## Trait Overview
//!
//! ```rust,ignore
//! pub trait StrongDigest: Sized {
//!     type Seed: Default;           // Initialization parameter
//!     type Digest: AsRef<[u8]>;     // Output type
//!     const DIGEST_LEN: usize;      // Output size in bytes
//!
//!     fn new() -> Self;                           // Create with default seed
//!     fn with_seed(seed: Self::Seed) -> Self;     // Create with explicit seed
//!     fn update(&mut self, data: &[u8]);          // Feed data
//!     fn finalize(self) -> Self::Digest;          // Compute hash
//!     fn digest(data: &[u8]) -> Self::Digest;     // One-shot helper
//! }
//! ```
//!
//! ## Generic Algorithm Selection
//!
//! Use the trait for algorithm-agnostic code:
//!
//! ```rust
//! use checksums::strong::{StrongDigest, Md5, Sha256, Xxh3};
//!
//! fn compute_checksum<D: StrongDigest>(data: &[u8]) -> D::Digest
//! where
//!     D::Seed: Default,
//! {
//!     D::digest(data)
//! }
//!
//! // Works with any algorithm
//! let md5_hash = compute_checksum::<Md5>(b"data");
//! let sha256_hash = compute_checksum::<Sha256>(b"data");
//! ```
//!
//! ## Streaming vs One-Shot
//!
//! All algorithms support both streaming and one-shot modes:
//!
//! ```rust
//! use checksums::strong::{Sha256, StrongDigest};
//!
//! // Streaming: process data incrementally
//! let mut hasher = Sha256::new();
//! hasher.update(b"chunk 1");
//! hasher.update(b"chunk 2");
//! let streaming_result = hasher.finalize();
//!
//! // One-shot: hash all data at once
//! let oneshot_result = Sha256::digest(b"chunk 1chunk 2");
//!
//! assert_eq!(streaming_result, oneshot_result);
//! ```
//!
//! # Performance Optimization
//!
//! ## SIMD Acceleration
//!
//! The crate automatically uses SIMD instructions when available:
//!
//! - **Rolling checksum**: AVX2/SSE2 on x86_64, NEON on aarch64
//! - **XXH3**: Runtime detection with `xxh3-simd` feature (default)
//! - **SHA-1/SHA-256**: Hardware acceleration via SHA-NI (x86_64) or
//!   crypto extensions (aarch64) when compiled with appropriate target features
//!
//! Query acceleration status at runtime:
//!
//! ```rust
//! use checksums::{simd_acceleration_available, xxh3_simd_available, openssl_acceleration_available};
//!
//! println!("Rolling checksum SIMD: {}", simd_acceleration_available());
//! println!("XXH3 runtime SIMD: {}", xxh3_simd_available());
//! println!("OpenSSL acceleration: {}", openssl_acceleration_available());
//! ```
//!
//! ## Best Practices
//!
//! 1. **Prefer one-shot methods** when hashing complete buffers - they enable
//!    better optimization and SIMD utilization.
//!
//! 2. **Use parallel computation** for multiple blocks when the `parallel`
//!    feature is enabled.
//!
//! 3. **Choose appropriate algorithms**:
//!    - XXH3 for maximum speed (non-cryptographic)
//!    - SHA-256 for cryptographic security
//!    - MD4/MD5 only for legacy rsync compatibility
//!
//! # Errors
//!
//! Rolling checksum operations can fail in specific circumstances:
//!
//! - [`RollingError::EmptyWindow`] - Attempting to roll on an empty checksum
//! - [`RollingError::WindowTooLarge`] - Window exceeds 32-bit representation
//! - [`RollingError::MismatchedSliceLength`] - Mismatched slice lengths in `roll_many`
//! - [`RollingSliceError`] - Invalid byte slice length when parsing digests
//!
//! Strong checksums never fail during computation.
//!
//! # Upstream Compatibility
//!
//! All algorithms produce output identical to upstream rsync 3.4.1:
//!
//! - Rolling checksums match `checksum.c:get_checksum1()`
//! - MD4/MD5 match rsync's checksum file validation
//! - XXH64/XXH3 match rsync's modern strong checksum paths
//!
//! # See Also
//!
//! - [`strong`] module for detailed algorithm documentation
//! - `parallel` module for concurrent computation (with `parallel` feature)
//! - [`RollingChecksum`] for sliding window checksum details
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod rolling;
pub mod strong;

#[cfg(feature = "parallel")]
#[cfg_attr(docsrs, doc(cfg(feature = "parallel")))]
pub mod parallel;

pub use rolling::{
    RollingChecksum, RollingDigest, RollingError, RollingSliceError, simd_acceleration_available,
};
pub use strong::openssl_acceleration_available;
pub use strong::xxh3_simd_available;
