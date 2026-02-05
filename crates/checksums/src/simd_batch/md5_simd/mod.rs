//! SIMD backend implementations for MD5.
//!
//! This module provides architecture-specific SIMD implementations for computing
//! multiple MD5 hashes in parallel. Each backend processes multiple independent
//! inputs simultaneously using SIMD instructions.

#[cfg(target_arch = "x86_64")]
pub mod sse2;

#[cfg(target_arch = "x86_64")]
pub mod ssse3;

#[cfg(target_arch = "x86_64")]
pub mod sse41;

#[cfg(target_arch = "x86_64")]
pub mod avx2;

#[cfg(target_arch = "x86_64")]
pub mod avx512;

#[cfg(target_arch = "aarch64")]
pub mod neon;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
