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
