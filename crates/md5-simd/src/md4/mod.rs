//! MD4 hashing implementations.
//!
//! MD4 is a predecessor to MD5 with a simpler structure:
//! - 3 rounds of 16 operations each (vs MD5's 4 rounds of 16)
//! - Only 3 constants (vs MD5's 64)
//! - Simpler round functions
//!
//! # Example
//!
//! ```
//! use md5_simd::md4;
//!
//! // Single hash
//! let hash = md4::digest(b"hello world");
//!
//! // Batch hash (uses SIMD when available)
//! let inputs = [b"input1".as_slice(), b"input2", b"input3"];
//! let hashes = md4::digest_batch(&inputs);
//! ```

pub mod scalar;

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "wasm32"
))]
pub mod simd;

use crate::{Backend, Digest};

/// Compute MD4 digests for multiple inputs in parallel.
///
/// Uses SIMD instructions when available to process multiple hashes
/// simultaneously. Returns digests in the same order as inputs.
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<Digest> {
    md4_dispatcher().digest_batch(inputs)
}

/// Compute MD4 digest for a single input.
pub fn digest(input: &[u8]) -> Digest {
    scalar::digest(input)
}

/// MD4 dispatcher that selects the optimal backend at runtime.
struct Md4Dispatcher {
    backend: Backend,
}

impl Md4Dispatcher {
    /// Detect CPU features and select the best available backend.
    fn detect() -> Self {
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
            // SSE2 is baseline for x86_64, always available
            Backend::Sse2
        }

        #[cfg(target_arch = "aarch64")]
        {
            // NEON is mandatory on aarch64
            return Backend::Neon;
        }

        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            return Backend::Wasm;
        }

        #[cfg(not(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            all(target_arch = "wasm32", target_feature = "simd128")
        )))]
        Backend::Scalar
    }

    /// Compute MD4 digests for multiple inputs.
    fn digest_batch<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        if inputs.is_empty() {
            return Vec::new();
        }

        match self.backend {
            #[cfg(target_arch = "x86_64")]
            Backend::Avx512 => self.digest_batch_avx512(inputs),
            #[cfg(target_arch = "x86_64")]
            Backend::Avx2 => self.digest_batch_avx2(inputs),
            #[cfg(target_arch = "x86_64")]
            Backend::Sse2 => self.digest_batch_sse2(inputs),
            #[cfg(target_arch = "aarch64")]
            Backend::Neon => self.digest_batch_neon(inputs),
            #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
            Backend::Wasm => self.digest_batch_wasm(inputs),
            // Scalar fallback
            _ => inputs.iter().map(|i| scalar::digest(i.as_ref())).collect(),
        }
    }

    /// AVX-512 batched digest implementation.
    #[cfg(target_arch = "x86_64")]
    fn digest_batch_avx512<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        let mut results = Vec::with_capacity(inputs.len());
        let chunks = inputs.chunks(16);

        for chunk in chunks {
            if chunk.len() == 16 {
                let batch: [&[u8]; 16] = [
                    chunk[0].as_ref(),
                    chunk[1].as_ref(),
                    chunk[2].as_ref(),
                    chunk[3].as_ref(),
                    chunk[4].as_ref(),
                    chunk[5].as_ref(),
                    chunk[6].as_ref(),
                    chunk[7].as_ref(),
                    chunk[8].as_ref(),
                    chunk[9].as_ref(),
                    chunk[10].as_ref(),
                    chunk[11].as_ref(),
                    chunk[12].as_ref(),
                    chunk[13].as_ref(),
                    chunk[14].as_ref(),
                    chunk[15].as_ref(),
                ];
                // SAFETY: We verified AVX-512 is available in detect_backend()
                let digests = unsafe { simd::avx512::digest_x16(&batch) };
                results.extend_from_slice(&digests);
            } else {
                // Partial batch - pad with empty inputs
                let mut batch: [&[u8]; 16] = [&[]; 16];
                for (i, input) in chunk.iter().enumerate() {
                    batch[i] = input.as_ref();
                }
                // SAFETY: We verified AVX-512 is available in detect_backend()
                let digests = unsafe { simd::avx512::digest_x16(&batch) };
                results.extend_from_slice(&digests[..chunk.len()]);
            }
        }

        results
    }

    /// AVX2 batched digest implementation.
    #[cfg(target_arch = "x86_64")]
    fn digest_batch_avx2<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        let mut results = Vec::with_capacity(inputs.len());
        let chunks = inputs.chunks(8);

        for chunk in chunks {
            if chunk.len() == 8 {
                let batch: [&[u8]; 8] = [
                    chunk[0].as_ref(),
                    chunk[1].as_ref(),
                    chunk[2].as_ref(),
                    chunk[3].as_ref(),
                    chunk[4].as_ref(),
                    chunk[5].as_ref(),
                    chunk[6].as_ref(),
                    chunk[7].as_ref(),
                ];
                // SAFETY: We verified AVX2 is available in detect_backend()
                let digests = unsafe { simd::avx2::digest_x8(&batch) };
                results.extend_from_slice(&digests);
            } else {
                // Partial batch - pad with empty inputs
                let mut batch: [&[u8]; 8] = [&[]; 8];
                for (i, input) in chunk.iter().enumerate() {
                    batch[i] = input.as_ref();
                }
                // SAFETY: We verified AVX2 is available in detect_backend()
                let digests = unsafe { simd::avx2::digest_x8(&batch) };
                results.extend_from_slice(&digests[..chunk.len()]);
            }
        }

        results
    }

    /// SSE2 batched digest implementation.
    #[cfg(target_arch = "x86_64")]
    fn digest_batch_sse2<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        let mut results = Vec::with_capacity(inputs.len());
        let chunks = inputs.chunks(4);

        for chunk in chunks {
            if chunk.len() == 4 {
                let batch: [&[u8]; 4] = [
                    chunk[0].as_ref(),
                    chunk[1].as_ref(),
                    chunk[2].as_ref(),
                    chunk[3].as_ref(),
                ];
                // SAFETY: SSE2 is baseline for x86_64
                let digests = unsafe { simd::sse2::digest_x4(&batch) };
                results.extend_from_slice(&digests);
            } else {
                let mut batch: [&[u8]; 4] = [&[]; 4];
                for (i, input) in chunk.iter().enumerate() {
                    batch[i] = input.as_ref();
                }
                // SAFETY: SSE2 is baseline for x86_64
                let digests = unsafe { simd::sse2::digest_x4(&batch) };
                results.extend_from_slice(&digests[..chunk.len()]);
            }
        }

        results
    }

    /// NEON batched digest implementation.
    #[cfg(target_arch = "aarch64")]
    fn digest_batch_neon<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        let mut results = Vec::with_capacity(inputs.len());
        let chunks = inputs.chunks(4);

        for chunk in chunks {
            if chunk.len() == 4 {
                let batch: [&[u8]; 4] = [
                    chunk[0].as_ref(),
                    chunk[1].as_ref(),
                    chunk[2].as_ref(),
                    chunk[3].as_ref(),
                ];
                // SAFETY: NEON is mandatory on aarch64
                let digests = unsafe { simd::neon::digest_x4(&batch) };
                results.extend_from_slice(&digests);
            } else {
                // Partial batch - pad with empty inputs
                let mut batch: [&[u8]; 4] = [&[]; 4];
                for (i, input) in chunk.iter().enumerate() {
                    batch[i] = input.as_ref();
                }
                // SAFETY: NEON is mandatory on aarch64
                let digests = unsafe { simd::neon::digest_x4(&batch) };
                results.extend_from_slice(&digests[..chunk.len()]);
            }
        }

        results
    }

    /// WASM SIMD batched digest implementation.
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    fn digest_batch_wasm<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        let mut results = Vec::with_capacity(inputs.len());
        let chunks = inputs.chunks(4);

        for chunk in chunks {
            if chunk.len() == 4 {
                let batch: [&[u8]; 4] = [
                    chunk[0].as_ref(),
                    chunk[1].as_ref(),
                    chunk[2].as_ref(),
                    chunk[3].as_ref(),
                ];
                let digests = simd::wasm::digest_x4(&batch);
                results.extend_from_slice(&digests);
            } else {
                let mut batch: [&[u8]; 4] = [&[]; 4];
                for (i, input) in chunk.iter().enumerate() {
                    batch[i] = input.as_ref();
                }
                let digests = simd::wasm::digest_x4(&batch);
                results.extend_from_slice(&digests[..chunk.len()]);
            }
        }

        results
    }
}

/// Global MD4 dispatcher instance, initialized on first use.
fn md4_dispatcher() -> &'static Md4Dispatcher {
    use std::sync::OnceLock;
    static DISPATCHER: OnceLock<Md4Dispatcher> = OnceLock::new();
    DISPATCHER.get_or_init(Md4Dispatcher::detect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            write!(s, "{b:02x}").unwrap();
        }
        s
    }

    #[test]
    fn md4_digest_batch_matches_scalar() {
        let inputs: Vec<&[u8]> = vec![
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            b"test",
            b"another test",
            b"third test",
        ];

        let results = digest_batch(&inputs);

        for (i, input) in inputs.iter().enumerate() {
            let expected = scalar::digest(input);
            assert_eq!(
                results[i],
                expected,
                "Mismatch at index {i} for input {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn md4_single_digest_works() {
        let result = digest(b"abc");
        assert_eq!(to_hex(&result), "a448017aaf21d8525fc10ae87aa6729d");
    }
}
