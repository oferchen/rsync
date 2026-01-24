# md5-simd Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Create a SIMD-accelerated parallel MD5 library that provides 4-8x throughput for batch hashing workloads.

**Architecture:** Lane-based SIMD where multiple MD5 states are processed in parallel using wide registers. Adaptive dispatch selects AVX-512 (16 lanes), AVX2 (8 lanes), NEON (4 lanes), or scalar based on CPU and workload size.

**Tech Stack:** Rust, std::arch intrinsics (AVX2/AVX-512/NEON), rayon for parallel iteration, criterion for benchmarks.

**Working Directory:** `/home/ofer/rsync/.worktrees/md5-simd`

---

## Phase 1: Crate Scaffold & Scalar Implementation

### Task 1: Create crate skeleton

**Files:**
- Create: `crates/md5-simd/Cargo.toml`
- Create: `crates/md5-simd/src/lib.rs`

**Step 1: Create the Cargo.toml**

```toml
[package]
name = "md5-simd"
version = "0.1.0"
edition = "2021"
rust-version = "1.75"
authors = ["rsync-rust contributors"]
license = "MIT OR Apache-2.0"
description = "SIMD-accelerated parallel MD5 hashing"
repository = "https://github.com/rsync-rust/rsync"

[package.metadata.docs.rs]
all-features = true
rustdoc-args = ["--cfg", "docsrs"]
targets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]

[features]
default = ["rayon"]
rayon = ["dep:rayon"]

[dependencies]
rayon = { version = "1.10", optional = true }

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
md-5 = "0.10"
rand = "0.8"

[[bench]]
name = "throughput"
harness = false
```

**Step 2: Create minimal lib.rs**

```rust
//! SIMD-accelerated parallel MD5 hashing.
//!
//! This crate provides high-throughput MD5 hashing by processing multiple
//! independent inputs in parallel using SIMD instructions.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod scalar;

/// MD5 digest type (16 bytes / 128 bits).
pub type Digest = [u8; 16];

/// Compute MD5 digests for multiple inputs in parallel.
///
/// Returns digests in the same order as inputs.
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<Digest> {
    inputs.iter().map(|i| scalar::digest(i.as_ref())).collect()
}

/// Compute MD5 digest for a single input.
pub fn digest(input: &[u8]) -> Digest {
    scalar::digest(input)
}
```

**Step 3: Add crate to workspace**

Edit `/home/ofer/rsync/.worktrees/md5-simd/Cargo.toml` (workspace root) and add to `[workspace] members`:
```toml
members = [
    # ... existing members ...
    "crates/md5-simd",
]
```

**Step 4: Verify it compiles**

Run: `cargo check -p md5-simd`
Expected: Compiles (with warning about missing scalar module)

**Step 5: Commit**

```bash
git add crates/md5-simd/Cargo.toml crates/md5-simd/src/lib.rs Cargo.toml
git commit -m "feat(md5-simd): scaffold crate with public API"
```

---

### Task 2: Implement scalar MD5 backend

**Files:**
- Create: `crates/md5-simd/src/scalar.rs`

**Step 1: Write the test first**

Create `crates/md5-simd/src/scalar.rs`:

```rust
//! Scalar (single-lane) MD5 implementation.
//!
//! Used as fallback when SIMD is unavailable or for single-hash workloads.

use crate::Digest;

/// MD5 initial state constants (RFC 1321).
const INIT_A: u32 = 0x67452301;
const INIT_B: u32 = 0xefcdab89;
const INIT_C: u32 = 0x98badcfe;
const INIT_D: u32 = 0x10325476;

/// Per-round shift amounts (RFC 1321).
const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
    5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// Pre-computed T[i] = floor(2^32 * |sin(i + 1)|) constants.
const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
    0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
    0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
    0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
    0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
    0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
    0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
    0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
    0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// Compute MD5 digest for input data.
pub fn digest(input: &[u8]) -> Digest {
    let mut state = [INIT_A, INIT_B, INIT_C, INIT_D];

    // Process complete 64-byte blocks
    let mut offset = 0;
    while offset + 64 <= input.len() {
        process_block(&mut state, &input[offset..offset + 64]);
        offset += 64;
    }

    // Pad and process final block(s)
    let remaining = &input[offset..];
    let bit_len = (input.len() as u64) * 8;

    // Padding: append 1 bit, then zeros, then 64-bit length
    let mut padded = [0u8; 128]; // Max 2 blocks needed
    padded[..remaining.len()].copy_from_slice(remaining);
    padded[remaining.len()] = 0x80;

    let pad_len = if remaining.len() < 56 { 64 } else { 128 };
    padded[pad_len - 8..pad_len].copy_from_slice(&bit_len.to_le_bytes());

    process_block(&mut state, &padded[..64]);
    if pad_len == 128 {
        process_block(&mut state, &padded[64..128]);
    }

    // Convert state to bytes (little-endian)
    let mut output = [0u8; 16];
    output[0..4].copy_from_slice(&state[0].to_le_bytes());
    output[4..8].copy_from_slice(&state[1].to_le_bytes());
    output[8..12].copy_from_slice(&state[2].to_le_bytes());
    output[12..16].copy_from_slice(&state[3].to_le_bytes());
    output
}

/// Process a single 64-byte block.
fn process_block(state: &mut [u32; 4], block: &[u8]) {
    debug_assert_eq!(block.len(), 64);

    // Parse block into 16 little-endian u32 words
    let mut m = [0u32; 16];
    for (i, chunk) in block.chunks_exact(4).enumerate() {
        m[i] = u32::from_le_bytes(chunk.try_into().unwrap());
    }

    let [mut a, mut b, mut c, mut d] = *state;

    for i in 0..64 {
        let (f, g) = match i {
            0..=15 => ((b & c) | ((!b) & d), i),
            16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
            32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | (!d)), (7 * i) % 16),
        };

        let temp = d;
        d = c;
        c = b;
        b = b.wrapping_add(
            (a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]))
                .rotate_left(S[i])
        );
        a = temp;
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn rfc1321_test_vectors() {
        // RFC 1321 Appendix A.5 test vectors
        let vectors: &[(&[u8], &str)] = &[
            (b"", "d41d8cd98f00b204e9800998ecf8427e"),
            (b"a", "0cc175b9c0f1b6a831c399e269772661"),
            (b"abc", "900150983cd24fb0d6963f7d28e17f72"),
            (b"message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (b"abcdefghijklmnopqrstuvwxyz", "c3fcd3d76192e4007dfb496cca67e13b"),
            (
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];

        for (input, expected) in vectors {
            let result = digest(input);
            assert_eq!(
                to_hex(&result),
                *expected,
                "Failed for input: {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn block_boundary_edge_cases() {
        // Test inputs at various block boundaries
        for len in [55, 56, 57, 63, 64, 65, 119, 120, 121] {
            let input: Vec<u8> = (0..len).map(|i| (i % 256) as u8).collect();
            let result = digest(&input);
            // Just verify it doesn't panic and returns 16 bytes
            assert_eq!(result.len(), 16);
        }
    }
}
```

**Step 2: Run the tests**

Run: `cargo test -p md5-simd`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/md5-simd/src/scalar.rs
git commit -m "feat(md5-simd): implement scalar MD5 backend with RFC 1321 tests"
```

---

### Task 3: Add public API tests

**Files:**
- Create: `crates/md5-simd/tests/correctness.rs`

**Step 1: Write integration tests**

```rust
//! Correctness tests for md5-simd public API.

use md5_simd::{digest, digest_batch};

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn single_digest_matches_rfc1321() {
    assert_eq!(
        to_hex(&digest(b"")),
        "d41d8cd98f00b204e9800998ecf8427e"
    );
    assert_eq!(
        to_hex(&digest(b"abc")),
        "900150983cd24fb0d6963f7d28e17f72"
    );
}

#[test]
fn batch_digest_matches_sequential() {
    let inputs: Vec<Vec<u8>> = (0..16)
        .map(|i| format!("test input {i}").into_bytes())
        .collect();

    let batch_results = digest_batch(&inputs);
    let sequential_results: Vec<_> = inputs.iter().map(|i| digest(i)).collect();

    assert_eq!(batch_results, sequential_results);
}

#[test]
fn batch_empty_returns_empty() {
    let empty: &[&[u8]] = &[];
    assert!(digest_batch(empty).is_empty());
}

#[test]
fn batch_single_matches_digest() {
    let input = b"single input";
    let batch = digest_batch(&[input]);
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0], digest(input));
}

#[test]
fn batch_with_different_lengths() {
    let inputs: &[&[u8]] = &[
        b"",
        b"a",
        b"short",
        b"a medium length string for testing",
        &[0u8; 1000],
    ];

    let batch = digest_batch(inputs);
    for (i, input) in inputs.iter().enumerate() {
        assert_eq!(batch[i], digest(input), "Mismatch at index {i}");
    }
}
```

**Step 2: Run tests**

Run: `cargo test -p md5-simd`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/md5-simd/tests/correctness.rs
git commit -m "test(md5-simd): add public API correctness tests"
```

---

## Phase 2: Dispatcher & AVX2 Implementation

### Task 4: Add dispatcher infrastructure

**Files:**
- Create: `crates/md5-simd/src/dispatcher.rs`
- Modify: `crates/md5-simd/src/lib.rs`

**Step 1: Create dispatcher module**

```rust
//! Runtime CPU detection and backend dispatch.

use crate::Digest;
use crate::scalar;

/// Available SIMD backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// AVX-512 with 16 parallel lanes.
    Avx512,
    /// AVX2 with 8 parallel lanes.
    Avx2,
    /// ARM NEON with 4 parallel lanes.
    Neon,
    /// Scalar fallback (1 lane).
    Scalar,
}

impl Backend {
    /// Number of parallel lanes for this backend.
    pub const fn lanes(self) -> usize {
        match self {
            Backend::Avx512 => 16,
            Backend::Avx2 => 8,
            Backend::Neon => 4,
            Backend::Scalar => 1,
        }
    }
}

/// Dispatcher that selects the optimal backend at runtime.
pub struct Dispatcher {
    backend: Backend,
}

impl Dispatcher {
    /// Detect CPU features and select the best available backend.
    pub fn detect() -> Self {
        let backend = Self::detect_backend();
        Self { backend }
    }

    fn detect_backend() -> Backend {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx512f") && is_x86_feature_detected!("avx512bw") {
                return Backend::Avx512;
            }
            if is_x86_feature_detected!("avx2") {
                return Backend::Avx2;
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            // NEON is mandatory on aarch64
            return Backend::Neon;
        }

        Backend::Scalar
    }

    /// Get the selected backend.
    pub const fn backend(&self) -> Backend {
        self.backend
    }

    /// Compute MD5 digests for multiple inputs.
    pub fn digest_batch<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        if inputs.is_empty() {
            return Vec::new();
        }

        // For now, always use scalar. SIMD backends added in later tasks.
        match self.backend {
            Backend::Avx512 | Backend::Avx2 | Backend::Neon | Backend::Scalar => {
                inputs.iter().map(|i| scalar::digest(i.as_ref())).collect()
            }
        }
    }

    /// Compute MD5 digest for a single input.
    pub fn digest(&self, input: &[u8]) -> Digest {
        scalar::digest(input)
    }
}

/// Global dispatcher instance, initialized on first use.
pub fn global() -> &'static Dispatcher {
    use std::sync::OnceLock;
    static DISPATCHER: OnceLock<Dispatcher> = OnceLock::new();
    DISPATCHER.get_or_init(Dispatcher::detect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatcher_detects_backend() {
        let dispatcher = Dispatcher::detect();
        // Just verify it doesn't panic and returns a valid backend
        let _ = dispatcher.backend();
    }

    #[test]
    fn global_dispatcher_is_consistent() {
        let d1 = global();
        let d2 = global();
        assert_eq!(d1.backend(), d2.backend());
    }
}
```

**Step 2: Update lib.rs to use dispatcher**

Replace `crates/md5-simd/src/lib.rs` with:

```rust
//! SIMD-accelerated parallel MD5 hashing.
//!
//! This crate provides high-throughput MD5 hashing by processing multiple
//! independent inputs in parallel using SIMD instructions.
//!
//! # Example
//!
//! ```
//! use md5_simd::{digest, digest_batch};
//!
//! // Single hash
//! let hash = digest(b"hello world");
//!
//! // Batch hash (uses SIMD when available)
//! let inputs = [b"input1".as_slice(), b"input2", b"input3"];
//! let hashes = digest_batch(&inputs);
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

mod dispatcher;
mod scalar;

pub use dispatcher::Backend;

/// MD5 digest type (16 bytes / 128 bits).
pub type Digest = [u8; 16];

/// Compute MD5 digests for multiple inputs in parallel.
///
/// Uses SIMD instructions when available to process multiple hashes
/// simultaneously. Returns digests in the same order as inputs.
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<Digest> {
    dispatcher::global().digest_batch(inputs)
}

/// Compute MD5 digest for a single input.
pub fn digest(input: &[u8]) -> Digest {
    dispatcher::global().digest(input)
}

/// Get the currently active SIMD backend.
///
/// Useful for logging or diagnostics.
pub fn active_backend() -> Backend {
    dispatcher::global().backend()
}
```

**Step 3: Run tests**

Run: `cargo test -p md5-simd`
Expected: All tests pass

**Step 4: Commit**

```bash
git add crates/md5-simd/src/dispatcher.rs crates/md5-simd/src/lib.rs
git commit -m "feat(md5-simd): add dispatcher with runtime CPU detection"
```

---

### Task 5: Create SIMD module structure

**Files:**
- Create: `crates/md5-simd/src/simd/mod.rs`
- Create: `crates/md5-simd/src/simd/avx2.rs`
- Modify: `crates/md5-simd/src/lib.rs`

**Step 1: Create simd module**

Create `crates/md5-simd/src/simd/mod.rs`:

```rust
//! SIMD backend implementations.

#[cfg(target_arch = "x86_64")]
pub mod avx2;

#[cfg(target_arch = "x86_64")]
pub mod avx512;

#[cfg(target_arch = "aarch64")]
pub mod neon;
```

**Step 2: Create AVX2 stub**

Create `crates/md5-simd/src/simd/avx2.rs`:

```rust
//! AVX2 8-lane parallel MD5 implementation.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::Digest;

/// MD5 state for 8 parallel computations.
#[repr(C, align(32))]
pub struct Md5x8 {
    a: __m256i,
    b: __m256i,
    c: __m256i,
    d: __m256i,
}

/// Compute MD5 digests for up to 8 inputs in parallel using AVX2.
///
/// # Safety
/// Caller must ensure AVX2 is available (use `is_x86_feature_detected!`).
#[target_feature(enable = "avx2")]
pub unsafe fn digest_x8(inputs: &[&[u8]; 8]) -> [Digest; 8] {
    // TODO: Implement SIMD MD5
    // For now, fall back to scalar
    std::array::from_fn(|i| crate::scalar::digest(inputs[i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avx2_matches_scalar() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        let inputs: [&[u8]; 8] = [
            b"input 0",
            b"input 1",
            b"input 2",
            b"input 3",
            b"input 4",
            b"input 5",
            b"input 6",
            b"input 7",
        ];

        let results = unsafe { digest_x8(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = crate::scalar::digest(input);
            assert_eq!(results[i], expected, "Mismatch at lane {i}");
        }
    }
}
```

**Step 3: Create AVX-512 stub**

Create `crates/md5-simd/src/simd/avx512.rs`:

```rust
//! AVX-512 16-lane parallel MD5 implementation.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::Digest;

/// Compute MD5 digests for up to 16 inputs in parallel using AVX-512.
///
/// # Safety
/// Caller must ensure AVX-512F and AVX-512BW are available.
#[target_feature(enable = "avx512f", enable = "avx512bw")]
pub unsafe fn digest_x16(inputs: &[&[u8]; 16]) -> [Digest; 16] {
    // TODO: Implement SIMD MD5
    // For now, fall back to scalar
    std::array::from_fn(|i| crate::scalar::digest(inputs[i]))
}
```

**Step 4: Create NEON stub**

Create `crates/md5-simd/src/simd/neon.rs`:

```rust
//! ARM NEON 4-lane parallel MD5 implementation.

use crate::Digest;

/// Compute MD5 digests for up to 4 inputs in parallel using NEON.
///
/// # Safety
/// Caller must ensure NEON is available (always true on aarch64).
#[cfg(target_arch = "aarch64")]
pub unsafe fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
    // TODO: Implement SIMD MD5
    // For now, fall back to scalar
    std::array::from_fn(|i| crate::scalar::digest(inputs[i]))
}
```

**Step 5: Add simd module to lib.rs**

Add after `mod scalar;`:
```rust
mod simd;
```

**Step 6: Run tests**

Run: `cargo test -p md5-simd`
Expected: All tests pass

**Step 7: Commit**

```bash
git add crates/md5-simd/src/simd/
git add crates/md5-simd/src/lib.rs
git commit -m "feat(md5-simd): add SIMD module structure with stubs"
```

---

### Task 6: Implement AVX2 MD5 core

**Files:**
- Modify: `crates/md5-simd/src/simd/avx2.rs`

**Step 1: Implement the AVX2 MD5 algorithm**

Replace `crates/md5-simd/src/simd/avx2.rs` with the full implementation:

```rust
//! AVX2 8-lane parallel MD5 implementation.
//!
//! Processes 8 independent MD5 computations simultaneously using 256-bit YMM registers.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::Digest;

/// MD5 initial state constants broadcast to 8 lanes.
const INIT_A: u32 = 0x67452301;
const INIT_B: u32 = 0xefcdab89;
const INIT_C: u32 = 0x98badcfe;
const INIT_D: u32 = 0x10325476;

/// Per-round shift amounts.
const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
    5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// Pre-computed K constants.
const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
    0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
    0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
    0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
    0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
    0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
    0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
    0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
    0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// Compute MD5 digests for up to 8 inputs in parallel using AVX2.
///
/// # Safety
/// Caller must ensure AVX2 is available.
#[target_feature(enable = "avx2")]
pub unsafe fn digest_x8(inputs: &[&[u8]; 8]) -> [Digest; 8] {
    // Find the maximum length to determine block count
    let max_len = inputs.iter().map(|i| i.len()).max().unwrap_or(0);

    // Calculate padded length (must be multiple of 64)
    let padded_len = ((max_len + 9 + 63) / 64) * 64;

    // Prepare padded buffers for each input
    let mut padded: [[u8; 256]; 8] = [[0u8; 256]; 8]; // Support up to 256 bytes

    for (i, input) in inputs.iter().enumerate() {
        let len = input.len();
        padded[i][..len].copy_from_slice(input);
        padded[i][len] = 0x80;
        let bit_len = (len as u64) * 8;
        let pad_end = ((len + 9 + 63) / 64) * 64;
        padded[i][pad_end - 8..pad_end].copy_from_slice(&bit_len.to_le_bytes());
    }

    // Initialize state (8 lanes)
    let mut a = _mm256_set1_epi32(INIT_A as i32);
    let mut b = _mm256_set1_epi32(INIT_B as i32);
    let mut c = _mm256_set1_epi32(INIT_C as i32);
    let mut d = _mm256_set1_epi32(INIT_D as i32);

    // Process blocks
    let num_blocks = padded_len / 64;
    for block_idx in 0..num_blocks {
        let block_offset = block_idx * 64;

        // Load message words (transposed: word i from all 8 inputs)
        let mut m = [_mm256_setzero_si256(); 16];
        for word_idx in 0..16 {
            let word_offset = block_offset + word_idx * 4;
            let words: [i32; 8] = std::array::from_fn(|lane| {
                if word_offset + 4 <= padded[lane].len() {
                    i32::from_le_bytes(padded[lane][word_offset..word_offset + 4].try_into().unwrap())
                } else {
                    0
                }
            });
            m[word_idx] = _mm256_setr_epi32(
                words[0], words[1], words[2], words[3],
                words[4], words[5], words[6], words[7],
            );
        }

        // Save state for this block
        let aa = a;
        let bb = b;
        let cc = c;
        let dd = d;

        // 64 rounds
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => {
                    // F = (B & C) | (~B & D)
                    let f = _mm256_or_si256(
                        _mm256_and_si256(b, c),
                        _mm256_andnot_si256(b, d),
                    );
                    (f, i)
                }
                16..=31 => {
                    // G = (D & B) | (~D & C)
                    let f = _mm256_or_si256(
                        _mm256_and_si256(d, b),
                        _mm256_andnot_si256(d, c),
                    );
                    (f, (5 * i + 1) % 16)
                }
                32..=47 => {
                    // H = B ^ C ^ D
                    let f = _mm256_xor_si256(_mm256_xor_si256(b, c), d);
                    (f, (3 * i + 5) % 16)
                }
                _ => {
                    // I = C ^ (B | ~D)
                    let not_d = _mm256_xor_si256(d, _mm256_set1_epi32(-1));
                    let f = _mm256_xor_si256(c, _mm256_or_si256(b, not_d));
                    (f, (7 * i) % 16)
                }
            };

            // temp = A + F + K[i] + M[g]
            let k_i = _mm256_set1_epi32(K[i] as i32);
            let temp = _mm256_add_epi32(
                _mm256_add_epi32(a, f),
                _mm256_add_epi32(k_i, m[g]),
            );

            // Rotate left by S[i]
            let s = S[i];
            let rotated = _mm256_or_si256(
                _mm256_slli_epi32(temp, s as i32),
                _mm256_srli_epi32(temp, (32 - s) as i32),
            );

            // Update state
            a = d;
            d = c;
            c = b;
            b = _mm256_add_epi32(b, rotated);
        }

        // Add saved state
        a = _mm256_add_epi32(a, aa);
        b = _mm256_add_epi32(b, bb);
        c = _mm256_add_epi32(c, cc);
        d = _mm256_add_epi32(d, dd);
    }

    // Extract results
    let mut results = [[0u8; 16]; 8];

    #[repr(C, align(32))]
    struct Aligned([i32; 8]);

    let mut a_out = Aligned([0; 8]);
    let mut b_out = Aligned([0; 8]);
    let mut c_out = Aligned([0; 8]);
    let mut d_out = Aligned([0; 8]);

    _mm256_store_si256(a_out.0.as_mut_ptr() as *mut __m256i, a);
    _mm256_store_si256(b_out.0.as_mut_ptr() as *mut __m256i, b);
    _mm256_store_si256(c_out.0.as_mut_ptr() as *mut __m256i, c);
    _mm256_store_si256(d_out.0.as_mut_ptr() as *mut __m256i, d);

    for lane in 0..8 {
        results[lane][0..4].copy_from_slice(&(a_out.0[lane] as u32).to_le_bytes());
        results[lane][4..8].copy_from_slice(&(b_out.0[lane] as u32).to_le_bytes());
        results[lane][8..12].copy_from_slice(&(c_out.0[lane] as u32).to_le_bytes());
        results[lane][12..16].copy_from_slice(&(d_out.0[lane] as u32).to_le_bytes());
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn avx2_matches_scalar() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        let inputs: [&[u8]; 8] = [
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"test input 5",
            b"test input 6",
            b"test input 7",
        ];

        let results = unsafe { digest_x8(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = crate::scalar::digest(input);
            assert_eq!(
                to_hex(&results[i]),
                to_hex(&expected),
                "Mismatch at lane {i} for input {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn avx2_rfc1321_vectors() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        let inputs: [&[u8]; 8] = [
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            b"",
        ];

        let expected = [
            "d41d8cd98f00b204e9800998ecf8427e",
            "0cc175b9c0f1b6a831c399e269772661",
            "900150983cd24fb0d6963f7d28e17f72",
            "f96b697d7cb7938d525a2f31aaf161d0",
            "c3fcd3d76192e4007dfb496cca67e13b",
            "d174ab98d277d9f5a5611c2c9f419d9f",
            "57edf4a22be3c955ac49da2e2107b67a",
            "d41d8cd98f00b204e9800998ecf8427e",
        ];

        let results = unsafe { digest_x8(&inputs) };

        for i in 0..8 {
            assert_eq!(
                to_hex(&results[i]),
                expected[i],
                "RFC 1321 vector mismatch at lane {i}"
            );
        }
    }
}
```

**Step 2: Run tests**

Run: `cargo test -p md5-simd`
Expected: All tests pass (AVX2 tests run if hardware supports it)

**Step 3: Commit**

```bash
git add crates/md5-simd/src/simd/avx2.rs
git commit -m "feat(md5-simd): implement AVX2 8-lane parallel MD5"
```

---

### Task 7: Wire AVX2 into dispatcher

**Files:**
- Modify: `crates/md5-simd/src/dispatcher.rs`

**Step 1: Update dispatcher to use AVX2**

Add import at top of dispatcher.rs:
```rust
#[cfg(target_arch = "x86_64")]
use crate::simd::avx2;
```

Replace the `digest_batch` method:

```rust
    /// Compute MD5 digests for multiple inputs.
    pub fn digest_batch<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        if inputs.is_empty() {
            return Vec::new();
        }

        match self.backend {
            #[cfg(target_arch = "x86_64")]
            Backend::Avx2 => self.digest_batch_avx2(inputs),
            _ => inputs.iter().map(|i| scalar::digest(i.as_ref())).collect(),
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn digest_batch_avx2<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        let mut results = Vec::with_capacity(inputs.len());
        let mut i = 0;

        // Process in batches of 8
        while i + 8 <= inputs.len() {
            let batch: [&[u8]; 8] = std::array::from_fn(|j| inputs[i + j].as_ref());
            let digests = unsafe { avx2::digest_x8(&batch) };
            results.extend_from_slice(&digests);
            i += 8;
        }

        // Handle remainder with scalar
        while i < inputs.len() {
            results.push(scalar::digest(inputs[i].as_ref()));
            i += 1;
        }

        results
    }
```

**Step 2: Run tests**

Run: `cargo test -p md5-simd`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/md5-simd/src/dispatcher.rs
git commit -m "feat(md5-simd): wire AVX2 backend into dispatcher"
```

---

## Phase 3: Benchmarks & Validation

### Task 8: Add criterion benchmarks

**Files:**
- Create: `crates/md5-simd/benches/throughput.rs`

**Step 1: Create benchmark file**

```rust
//! Throughput benchmarks for md5-simd.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use md5_simd::{active_backend, digest, digest_batch};

fn single_hash(c: &mut Criterion) {
    let input = vec![0u8; 8192]; // 8KB

    let mut group = c.benchmark_group("single_hash");
    group.throughput(Throughput::Bytes(input.len() as u64));

    group.bench_function("md5_simd", |b| {
        b.iter(|| digest(black_box(&input)))
    });

    group.bench_function("md5_crate", |b| {
        use md5::{Md5, Digest};
        b.iter(|| {
            let mut hasher = Md5::new();
            hasher.update(black_box(&input));
            hasher.finalize()
        })
    });

    group.finish();
}

fn batch_hash(c: &mut Criterion) {
    let inputs: Vec<Vec<u8>> = (0..8).map(|_| vec![0u8; 8192]).collect();
    let total_bytes = inputs.len() * 8192;

    let mut group = c.benchmark_group("batch_hash_8x8kb");
    group.throughput(Throughput::Bytes(total_bytes as u64));

    group.bench_function("md5_simd_batch", |b| {
        b.iter(|| digest_batch(black_box(&inputs)))
    });

    group.bench_function("md5_simd_sequential", |b| {
        b.iter(|| {
            inputs.iter().map(|i| digest(black_box(i))).collect::<Vec<_>>()
        })
    });

    group.bench_function("md5_crate_sequential", |b| {
        use md5::{Md5, Digest};
        b.iter(|| {
            inputs.iter().map(|i| {
                let mut hasher = Md5::new();
                hasher.update(black_box(i));
                hasher.finalize()
            }).collect::<Vec<_>>()
        })
    });

    group.finish();
}

fn batch_sizes(c: &mut Criterion) {
    for count in [1, 2, 4, 8, 16, 32] {
        let inputs: Vec<Vec<u8>> = (0..count).map(|_| vec![0u8; 8192]).collect();
        let total_bytes = count * 8192;

        let mut group = c.benchmark_group(format!("batch_{count}x8kb"));
        group.throughput(Throughput::Bytes(total_bytes as u64));

        group.bench_function("md5_simd", |b| {
            b.iter(|| digest_batch(black_box(&inputs)))
        });

        group.finish();
    }
}

fn report_backend(c: &mut Criterion) {
    println!("Active backend: {:?}", active_backend());

    c.bench_function("backend_check", |b| {
        b.iter(|| active_backend())
    });
}

criterion_group!(benches, report_backend, single_hash, batch_hash, batch_sizes);
criterion_main!(benches);
```

**Step 2: Add md5 dev-dependency**

Update `crates/md5-simd/Cargo.toml` dev-dependencies:
```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
md-5 = "0.10"
md5 = "0.7"  # For comparison benchmarks
rand = "0.8"
```

**Step 3: Run benchmarks**

Run: `cargo bench -p md5-simd -- --quick`
Expected: Benchmarks run and show throughput in MiB/s or GiB/s

**Step 4: Commit**

```bash
git add crates/md5-simd/benches/throughput.rs crates/md5-simd/Cargo.toml
git commit -m "bench(md5-simd): add criterion throughput benchmarks"
```

---

## Phase 4: AVX-512 and NEON (tasks 9-12 follow same pattern)

### Task 9: Implement AVX-512 backend

**Files:**
- Modify: `crates/md5-simd/src/simd/avx512.rs`

Follow the same pattern as AVX2 but with:
- 16 lanes instead of 8
- `__m512i` registers
- `_mm512_*` intrinsics
- `vpternlogd` for F/G/H/I functions (single instruction)
- `vprold` for rotation (single instruction)

### Task 10: Wire AVX-512 into dispatcher

Similar to Task 7 but for AVX-512.

### Task 11: Implement NEON backend

**Files:**
- Modify: `crates/md5-simd/src/simd/neon.rs`

Follow same pattern with:
- 4 lanes
- `uint32x4_t` registers
- `vaddq_u32`, `vandq_u32`, `vorrq_u32`, `veorq_u32`
- `vshlq_n_u32` / `vshrq_n_u32` for rotation

### Task 12: Wire NEON into dispatcher

Similar to Task 7 but for NEON.

---

## Phase 5: Rayon Integration & rsync Integration

### Task 13: Add rayon parallel iterator support

**Files:**
- Create: `crates/md5-simd/src/rayon.rs`
- Modify: `crates/md5-simd/src/lib.rs`

**Step 1: Create rayon module**

```rust
//! Rayon integration for parallel MD5 hashing.

use rayon::prelude::*;
use std::path::Path;
use std::io;
use std::fs;

use crate::{Digest, digest, digest_batch};

/// Extension trait for parallel MD5 hashing.
pub trait ParallelMd5<T> {
    /// Compute MD5 digests in parallel using SIMD when beneficial.
    fn md5_digest(self) -> Vec<Digest>;
}

impl<I, T> ParallelMd5<T> for I
where
    I: ParallelIterator<Item = T>,
    T: AsRef<[u8]> + Send,
{
    fn md5_digest(self) -> Vec<Digest> {
        // Collect and batch for SIMD
        let items: Vec<T> = self.collect();
        digest_batch(&items)
    }
}

/// Compute MD5 digests for multiple files in parallel.
pub fn digest_files<P: AsRef<Path> + Sync>(paths: &[P]) -> Vec<io::Result<Digest>> {
    paths
        .par_iter()
        .map(|path| {
            let data = fs::read(path.as_ref())?;
            Ok(digest(&data))
        })
        .collect()
}
```

**Step 2: Export from lib.rs**

```rust
#[cfg(feature = "rayon")]
#[cfg_attr(docsrs, doc(cfg(feature = "rayon")))]
mod rayon_support;

#[cfg(feature = "rayon")]
#[cfg_attr(docsrs, doc(cfg(feature = "rayon")))]
pub use rayon_support::{ParallelMd5, digest_files};
```

**Step 3: Test**

Run: `cargo test -p md5-simd --features rayon`

**Step 4: Commit**

```bash
git add crates/md5-simd/src/rayon_support.rs crates/md5-simd/src/lib.rs
git commit -m "feat(md5-simd): add rayon parallel iterator support"
```

---

### Task 14: Integrate into checksums crate

**Files:**
- Modify: `crates/checksums/Cargo.toml`
- Modify: `crates/checksums/src/strong/md5.rs`

**Step 1: Add feature and dependency**

In `crates/checksums/Cargo.toml`:
```toml
[features]
simd = ["md5-simd"]

[dependencies]
md5-simd = { path = "../md5-simd", optional = true }
```

**Step 2: Create batch API in md5.rs**

Add to `crates/checksums/src/strong/md5.rs`:
```rust
/// Batch compute MD5 digests using SIMD when available.
#[cfg(feature = "simd")]
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<[u8; 16]> {
    md5_simd::digest_batch(inputs)
}
```

**Step 3: Commit**

```bash
git add crates/checksums/Cargo.toml crates/checksums/src/strong/md5.rs
git commit -m "feat(checksums): integrate md5-simd with simd feature"
```

---

### Task 15: Final validation

**Step 1: Run full test suite**

```bash
cargo nextest run --workspace --all-features
```

**Step 2: Run benchmarks**

```bash
cargo bench -p md5-simd
```

**Step 3: Profile rsync checksum sync**

```bash
# Create test data
mkdir -p /tmp/bench_src
for i in $(seq 1 100); do dd if=/dev/urandom of=/tmp/bench_src/file_$i bs=1M count=1 2>/dev/null; done

# Profile with simd feature
cargo build --release --features simd
perf record -g ./target/release/oc-rsync -c /tmp/bench_src/ /tmp/bench_dst/
perf report
```

**Step 4: Commit summary**

```bash
git add -A
git commit -m "feat(md5-simd): complete implementation with all backends"
```

---

## Summary

| Task | Description | Est. Complexity |
|------|-------------|-----------------|
| 1 | Create crate skeleton | Simple |
| 2 | Implement scalar backend | Medium |
| 3 | Add public API tests | Simple |
| 4 | Add dispatcher infrastructure | Medium |
| 5 | Create SIMD module structure | Simple |
| 6 | Implement AVX2 MD5 core | Complex |
| 7 | Wire AVX2 into dispatcher | Simple |
| 8 | Add criterion benchmarks | Medium |
| 9 | Implement AVX-512 backend | Complex |
| 10 | Wire AVX-512 into dispatcher | Simple |
| 11 | Implement NEON backend | Complex |
| 12 | Wire NEON into dispatcher | Simple |
| 13 | Add rayon integration | Medium |
| 14 | Integrate into checksums | Simple |
| 15 | Final validation | Medium |
