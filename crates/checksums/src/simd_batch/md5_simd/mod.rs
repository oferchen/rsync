//! SIMD backend implementations for MD5.
//!
//! This module provides architecture-specific SIMD implementations for computing
//! multiple MD5 hashes in parallel. Each backend processes multiple independent
//! inputs simultaneously using SIMD instructions.

#[cfg(target_arch = "x86_64")]
pub mod sse2;

/// SSSE3 4-lane parallel MD5 implementation.
#[cfg(target_arch = "x86_64")]
pub mod ssse3;

/// SSE4.1 4-lane parallel MD5 implementation.
#[cfg(target_arch = "x86_64")]
pub mod sse41;

/// AVX2 8-lane parallel MD5 implementation.
#[cfg(target_arch = "x86_64")]
pub mod avx2;

/// AVX-512 16-lane parallel MD5 implementation.
#[cfg(target_arch = "x86_64")]
pub mod avx512;

/// ARM NEON 4-lane parallel MD5 implementation.
#[cfg(target_arch = "aarch64")]
pub mod neon;

/// WebAssembly SIMD 4-lane parallel MD5 implementation.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
