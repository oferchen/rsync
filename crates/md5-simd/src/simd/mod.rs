//! SIMD backend implementations for MD5.
//!
//! This module provides architecture-specific SIMD implementations for computing
//! multiple MD5 hashes in parallel. Each backend processes multiple independent
//! inputs simultaneously using SIMD instructions.
//!
//! # Available Backends
//!
//! ## x86_64 Implementations
//!
//! - **SSE2**: 4-lane parallel, baseline for x86_64, always available
//! - **SSSE3**: 4-lane parallel, adds byte shuffle instructions (2006+)
//! - **SSE4.1**: 4-lane parallel, efficient blending for mixed lengths (2007+)
//! - **AVX2**: 8-lane parallel, 256-bit operations (2013+)
//! - **AVX-512**: 16-lane parallel, 512-bit operations (2017+)
//!
//! ## ARM Implementations
//!
//! - **NEON**: 4-lane parallel, mandatory on aarch64
//!
//! ## WebAssembly Implementations
//!
//! - **WASM SIMD**: 4-lane parallel, portable across architectures
//!
//! # Backend Selection
//!
//! The appropriate backend is selected at runtime by the parent module's
//! dispatcher based on CPU feature detection. Applications should not call
//! these implementations directly - use the public API instead.
//!
//! # Performance Strategy
//!
//! All implementations use a transposed data layout where each SIMD lane
//! processes one complete MD5 computation. State variables (A, B, C, D) are
//! stored with all lanes' values in a single SIMD register, allowing the
//! MD5 rounds to execute in parallel across all lanes.
//!
//! # Safety
//!
//! All functions in this module (except WASM) are marked `unsafe` because they
//! use architecture-specific intrinsics. Callers must verify CPU feature support
//! before invoking these functions.

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
