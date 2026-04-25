use digest::Digest;

use super::StrongDigest;

/// Streaming SHA-256 hasher used by rsync when peers negotiate stronger daemon authentication digests.
///
/// SHA-256 produces a 256-bit (32-byte) digest and is cryptographically
/// secure. It is used for daemon authentication and high-security transfers.
///
/// # Hardware Acceleration
///
/// The `sha2` crate auto-detects hardware SHA acceleration at runtime via
/// the `cpufeatures` crate:
///
/// - **x86_64**: SHA-NI (Intel SHA Extensions, AMD Zen) when the `sha`,
///   `sse2`, `ssse3`, and `sse4.1` CPU features are present.
/// - **aarch64**: ARMv8 Cryptography Extensions (`sha2`) when present.
/// - **Other architectures**: software fallback only.
///
/// Detection happens automatically on the first hash operation and is cached
/// internally by `cpufeatures`. No `RUSTFLAGS` or compile-time target features
/// are required: a single binary picks the fastest backend at runtime on every
/// host. On Unix targets the `asm` feature is enabled to provide an additional
/// hand-tuned assembly fallback for hosts without SHA-NI; Windows builds use
/// the pure-Rust backend because the `sha2-asm` build script depends on NASM
/// and fails under MSVC.
///
/// Runtime availability can be queried via
/// [`sha256_hardware_acceleration_available`].
///
/// # Examples
///
/// One-shot hashing:
///
/// ```
/// use checksums::strong::Sha256;
///
/// let digest = Sha256::digest(b"secure data");
/// assert_eq!(digest.len(), 32);
/// ```
///
/// Incremental hashing:
///
/// ```
/// use checksums::strong::Sha256;
///
/// let mut hasher = Sha256::new();
/// hasher.update(b"part one");
/// hasher.update(b"part two");
/// let digest = hasher.finalize();
/// assert_eq!(digest, Sha256::digest(b"part onepart two"));
/// ```
#[derive(Clone, Debug)]
pub struct Sha256 {
    inner: sha2::Sha256,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// Creates a hasher with an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: sha2::Sha256::new(),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the 256-bit SHA-256 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 32] {
        self.inner.finalize().into()
    }

    /// Convenience helper that computes the SHA-256 digest for `data` in one shot.
    #[must_use]
    pub fn digest(data: &[u8]) -> [u8; 32] {
        <Self as StrongDigest>::digest(data)
    }
}

impl StrongDigest for Sha256 {
    type Seed = ();
    type Digest = [u8; 32];
    const DIGEST_LEN: usize = 32;

    fn with_seed((): Self::Seed) -> Self {
        Sha256::new()
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finalize(self) -> Self::Digest {
        self.inner.finalize().into()
    }
}

/// Returns `true` when the running CPU exposes SHA-256 hardware acceleration.
///
/// This mirrors the runtime detection performed by the `sha2` crate's
/// `cpufeatures` integration:
///
/// - **x86_64**: returns `true` if the CPU advertises Intel SHA Extensions
///   (`sha`) together with the `sse2`, `ssse3`, and `sse4.1` baseline that
///   `sha2` requires for its accelerated backend.
/// - **aarch64**: returns `true` if the CPU advertises the ARMv8 `sha2`
///   crypto extension.
/// - **Other architectures**: always returns `false`.
///
/// The result reflects the actual capability of the host CPU, regardless of
/// any compile-time target features. Use this to verify that a deployment
/// is benefiting from hardware acceleration or to select instrumentation in
/// benchmarks.
///
/// # Examples
///
/// ```
/// use checksums::strong::sha256_hardware_acceleration_available;
///
/// let _accelerated = sha256_hardware_acceleration_available();
/// ```
#[must_use]
pub fn sha256_hardware_acceleration_available() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("sha")
            && std::arch::is_x86_feature_detected!("sse2")
            && std::arch::is_x86_feature_detected!("ssse3")
            && std::arch::is_x86_feature_detected!("sse4.1")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("sha2")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;

        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(&mut out, "{byte:02x}").expect("write! to String cannot fail");
        }
        out
    }

    #[test]
    fn sha256_streaming_matches_rfc_vectors() {
        let vectors = [
            (
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc".as_slice(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"message digest".as_slice(),
                "f7846f55cf23e14eebeab5b4e1550cad5b509e3348fbc4efa3a1413d393cb650",
            ),
        ];

        for (input, expected_hex) in vectors {
            let mut hasher = Sha256::new();
            let mid = input.len() / 2;
            hasher.update(&input[..mid]);
            hasher.update(&input[mid..]);
            let digest = hasher.finalize();
            assert_eq!(to_hex(&digest), expected_hex);

            let one_shot = Sha256::digest(input);
            assert_eq!(to_hex(&one_shot), expected_hex);
        }
    }

    #[test]
    fn hardware_detection_query_is_consistent() {
        let first = sha256_hardware_acceleration_available();
        let second = sha256_hardware_acceleration_available();
        assert_eq!(
            first, second,
            "hardware detection must be deterministic across calls"
        );
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn x86_64_detection_implies_sha_extension() {
        if sha256_hardware_acceleration_available() {
            assert!(
                std::arch::is_x86_feature_detected!("sha"),
                "SHA-NI must be present whenever hardware acceleration is reported on x86_64"
            );
        }
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn aarch64_detection_implies_sha2_extension() {
        if sha256_hardware_acceleration_available() {
            assert!(
                std::arch::is_aarch64_feature_detected!("sha2"),
                "ARMv8 sha2 extension must be present whenever hardware acceleration is reported on aarch64"
            );
        }
    }

    #[test]
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    fn hardware_detection_is_unavailable_on_other_architectures() {
        assert!(
            !sha256_hardware_acceleration_available(),
            "no SHA-256 hardware acceleration is exposed on this architecture"
        );
    }

    #[test]
    fn accelerated_backend_produces_identical_digests() {
        // Regression guard: ensure the cpufeatures-selected backend (whether
        // SHA-NI, ARMv8 crypto, or scalar) produces standards-compliant
        // digests. If hardware acceleration silently miscomputed, the RFC
        // vectors above would already fail; this test additionally exercises
        // a longer input that crosses multiple SHA-256 compression blocks.
        let input = vec![0x5au8; 1024 * 16];
        let one_shot = Sha256::digest(&input);

        let mut hasher = Sha256::new();
        for chunk in input.chunks(57) {
            hasher.update(chunk);
        }
        let streamed = hasher.finalize();

        assert_eq!(
            one_shot, streamed,
            "streaming and one-shot SHA-256 must agree regardless of backend"
        );
    }
}
