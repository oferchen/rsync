//! Runtime CPU detection and backend dispatch.

use crate::Digest;
use crate::scalar;
#[cfg(target_arch = "x86_64")]
use crate::simd;

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

        match self.backend {
            #[cfg(target_arch = "x86_64")]
            Backend::Avx512 => {
                // AVX-512 falls back to scalar for now (requires nightly)
                inputs.iter().map(|i| scalar::digest(i.as_ref())).collect()
            }
            #[cfg(target_arch = "x86_64")]
            Backend::Avx2 => self.digest_batch_avx2(inputs),
            #[cfg(target_arch = "aarch64")]
            Backend::Neon => {
                // NEON falls back to scalar for now
                inputs.iter().map(|i| scalar::digest(i.as_ref())).collect()
            }
            #[allow(unreachable_patterns)]
            Backend::Scalar | _ => {
                inputs.iter().map(|i| scalar::digest(i.as_ref())).collect()
            }
        }
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
                results[i], expected,
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
