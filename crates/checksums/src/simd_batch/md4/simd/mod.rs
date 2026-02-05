//! SIMD backend implementations for MD4.
//!
//! Provides architecture-specific SIMD implementations:
//! - **x86_64**: SSE2 (4 lanes), AVX2 (8 lanes), AVX-512 (16 lanes)
//! - **aarch64**: NEON (4 lanes)
//! - **wasm32**: WASM SIMD (4 lanes)

#[cfg(target_arch = "x86_64")]
pub mod sse2;

#[cfg(target_arch = "x86_64")]
pub mod avx2;

#[cfg(target_arch = "x86_64")]
pub mod avx512;

#[cfg(target_arch = "aarch64")]
pub mod neon;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
