//! Runtime CPU detection and backend dispatch.

use crate::scalar;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
use crate::simd;
use crate::Digest;

/// Available SIMD backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// AVX-512 with 16 parallel lanes.
    Avx512,
    /// AVX2 with 8 parallel lanes.
    Avx2,
    /// SSE4.1 with 4 parallel lanes (blendv optimization).
    Sse41,
    /// SSSE3 with 4 parallel lanes (pshufb optimization).
    Ssse3,
    /// SSE2 with 4 parallel lanes (baseline x86_64).
    Sse2,
    /// ARM NEON with 4 parallel lanes.
    Neon,
    /// WebAssembly SIMD with 4 parallel lanes.
    Wasm,
    /// Scalar fallback (1 lane).
    Scalar,
}

impl Backend {
    /// Number of parallel lanes for this backend.
    pub const fn lanes(self) -> usize {
        match self {
            Backend::Avx512 => 16,
            Backend::Avx2 => 8,
            Backend::Sse41 => 4,
            Backend::Ssse3 => 4,
            Backend::Sse2 => 4,
            Backend::Neon => 4,
            Backend::Wasm => 4,
            Backend::Scalar => 1,
        }
    }

    /// Human-readable name of the backend.
    pub const fn name(self) -> &'static str {
        match self {
            Backend::Avx512 => "AVX-512",
            Backend::Avx2 => "AVX2",
            Backend::Sse41 => "SSE4.1",
            Backend::Ssse3 => "SSSE3",
            Backend::Sse2 => "SSE2",
            Backend::Neon => "NEON",
            Backend::Wasm => "WASM SIMD",
            Backend::Scalar => "Scalar",
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
            if is_x86_feature_detected!("sse4.1") {
                return Backend::Sse41;
            }
            if is_x86_feature_detected!("ssse3") {
                return Backend::Ssse3;
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

    /// Get the selected backend.
    pub const fn backend(&self) -> Backend {
        self.backend
    }

    /// Compute MD5 digests for multiple inputs.
    pub fn digest_batch<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        if inputs.is_empty() {
            return Vec::new();
        }

        match self.backend {
            #[cfg(target_arch = "x86_64")]
            Backend::Avx512 => self.digest_batch_avx512(inputs),
            #[cfg(target_arch = "x86_64")]
            Backend::Avx2 => self.digest_batch_avx2(inputs),
            #[cfg(target_arch = "x86_64")]
            Backend::Sse41 => self.digest_batch_sse41(inputs),
            #[cfg(target_arch = "x86_64")]
            Backend::Ssse3 => self.digest_batch_ssse3(inputs),
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
    ///
    /// Processes inputs in batches of 16 using AVX-512 SIMD.
    #[cfg(target_arch = "x86_64")]
    fn digest_batch_avx512<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        let mut results = Vec::with_capacity(inputs.len());
        let chunks = inputs.chunks(16);

        for chunk in chunks {
            if chunk.len() == 16 {
                // Full batch of 16 - use AVX-512
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
                // Full batch of 8 - use SIMD
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
    ///
    /// Processes inputs in batches of 4 using SSE2 SIMD.
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

    /// SSSE3 batched digest implementation.
    ///
    /// Processes inputs in batches of 4 using SSSE3 SIMD.
    #[cfg(target_arch = "x86_64")]
    fn digest_batch_ssse3<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
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
                // SAFETY: We verified SSSE3 is available in detect_backend()
                let digests = unsafe { simd::ssse3::digest_x4(&batch) };
                results.extend_from_slice(&digests);
            } else {
                let mut batch: [&[u8]; 4] = [&[]; 4];
                for (i, input) in chunk.iter().enumerate() {
                    batch[i] = input.as_ref();
                }
                // SAFETY: We verified SSSE3 is available in detect_backend()
                let digests = unsafe { simd::ssse3::digest_x4(&batch) };
                results.extend_from_slice(&digests[..chunk.len()]);
            }
        }

        results
    }

    /// SSE4.1 batched digest implementation.
    ///
    /// Processes inputs in batches of 4 using SSE4.1 SIMD with blendv optimization.
    #[cfg(target_arch = "x86_64")]
    fn digest_batch_sse41<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
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
                // SAFETY: We verified SSE4.1 is available in detect_backend()
                let digests = unsafe { simd::sse41::digest_x4(&batch) };
                results.extend_from_slice(&digests);
            } else {
                let mut batch: [&[u8]; 4] = [&[]; 4];
                for (i, input) in chunk.iter().enumerate() {
                    batch[i] = input.as_ref();
                }
                // SAFETY: We verified SSE4.1 is available in detect_backend()
                let digests = unsafe { simd::sse41::digest_x4(&batch) };
                results.extend_from_slice(&digests[..chunk.len()]);
            }
        }

        results
    }

    /// NEON batched digest implementation.
    ///
    /// Processes inputs in batches of 4 using NEON SIMD.
    /// Currently falls back to scalar while NEON MD5 is implemented.
    #[cfg(target_arch = "aarch64")]
    fn digest_batch_neon<T: AsRef<[u8]>>(&self, inputs: &[T]) -> Vec<Digest> {
        let mut results = Vec::with_capacity(inputs.len());
        let chunks = inputs.chunks(4);

        for chunk in chunks {
            if chunk.len() == 4 {
                // Full batch of 4 - use NEON
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

    #[test]
    fn digest_batch_matches_scalar() {
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

        let dispatcher = Dispatcher::detect();
        let results = dispatcher.digest_batch(&inputs);

        // Verify each result matches scalar
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
    fn digest_batch_partial_batch() {
        // Test with exactly 3 inputs (partial batch)
        let inputs: Vec<&[u8]> = vec![b"one", b"two", b"three"];
        let dispatcher = Dispatcher::detect();
        let results = dispatcher.digest_batch(&inputs);

        assert_eq!(results.len(), 3);
        for (i, input) in inputs.iter().enumerate() {
            assert_eq!(results[i], scalar::digest(input));
        }
    }
}
