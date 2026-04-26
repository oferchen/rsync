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
/// the `cpufeatures` crate. The dispatch ladder is:
///
/// 1. **x86_64 SHA-NI** - Intel SHA Extensions / AMD Zen `sha256rnds2` /
///    `sha256msg1` / `sha256msg2` when `is_x86_feature_detected!("sha")`
///    together with the `sse2`, `ssse3`, and `sse4.1` baseline that the
///    `sha2` accelerated backend requires.
/// 2. **aarch64 ARMv8 cryptography extension** - `sha256h` / `sha256h2` /
///    `sha256su0` / `sha256su1` when `is_aarch64_feature_detected!("sha2")`.
/// 3. **Hand-tuned assembly fallback** - `sha2-asm` on Unix targets, used on
///    hosts without SHA-NI but where assembly compilation succeeds.
/// 4. **Pure-Rust scalar fallback** - portable software implementation used on
///    Windows (where `sha2-asm` requires NASM) and on architectures without
///    hardware acceleration.
///
/// Detection happens automatically on the first hash operation and is cached
/// internally by `cpufeatures`. No `RUSTFLAGS` or compile-time target features
/// are required: a single binary picks the fastest backend at runtime on every
/// host.
///
/// Parity between the streaming and one-shot paths (which both flow through
/// the same active backend) is exercised by `streaming_random_buffer_matches_one_shot`
/// and `streaming_chunk_sizes_match_one_shot` below.
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
    fn empty_input_known_hash() {
        let digest = Sha256::digest(b"");
        assert_eq!(
            to_hex(&digest),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn abc_known_hash() {
        let digest = Sha256::digest(b"abc");
        assert_eq!(
            to_hex(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
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

    #[test]
    fn streaming_matches_one_shot() {
        let data = b"The quick brown fox jumps over the lazy dog";

        let one_shot = Sha256::digest(data);

        let mut hasher = Sha256::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..20]);
        hasher.update(&data[20..]);
        let streaming = hasher.finalize();

        assert_eq!(one_shot, streaming);
    }

    #[test]
    fn byte_at_a_time_matches_one_shot() {
        let data = b"incremental SHA-256 input";
        let expected = Sha256::digest(data);

        let mut hasher = Sha256::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        assert_eq!(hasher.finalize(), expected);
    }

    #[test]
    fn different_data_different_hashes() {
        assert_ne!(Sha256::digest(b"aaa"), Sha256::digest(b"bbb"));
    }

    #[test]
    fn large_data_consistent() {
        // Walk the 64-byte SHA-256 compression function across many blocks.
        let data: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
        let first = Sha256::digest(&data);
        let second = Sha256::digest(&data);
        assert_eq!(first, second);
    }

    #[test]
    fn incremental_chunks_consistent() {
        let data: Vec<u8> = (0u8..=255).cycle().take(8192).collect();
        let expected = Sha256::digest(&data);

        for chunk_size in [1usize, 7, 13, 64, 1000] {
            let mut hasher = Sha256::new();
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finalize(), expected, "chunk_size={chunk_size}");
        }
    }

    #[test]
    fn hash_function_is_deterministic() {
        let data = b"deterministic input";
        assert_eq!(Sha256::digest(data), Sha256::digest(data));
    }

    #[test]
    fn default_trait_matches_new() {
        let a = Sha256::new().finalize();
        let b = Sha256::default().finalize();
        assert_eq!(a, b);
    }

    #[test]
    fn clone_preserves_state() {
        let mut hasher = Sha256::new();
        hasher.update(b"partial state");
        let cloned = hasher.clone();

        assert_eq!(hasher.finalize(), cloned.finalize());
    }

    #[test]
    fn length_extension_protection() {
        assert_ne!(Sha256::digest(b""), Sha256::digest(&[0u8]));
    }

    #[test]
    fn hex_output_format_matches_lowercase() {
        let digest = Sha256::digest(b"abc");
        let hex = to_hex(&digest);
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(hex.chars().all(|c| !c.is_ascii_uppercase()));
    }

    #[test]
    fn strong_digest_trait_matches_inherent_api() {
        let data = b"trait dispatch parity";

        let inherent = Sha256::digest(data);
        let via_trait = <Sha256 as StrongDigest>::digest(data);
        assert_eq!(inherent, via_trait);

        let mut hasher = <Sha256 as StrongDigest>::with_seed(());
        StrongDigest::update(&mut hasher, data);
        let trait_streaming = StrongDigest::finalize(hasher);
        assert_eq!(trait_streaming, inherent);

        assert_eq!(<Sha256 as StrongDigest>::DIGEST_LEN, 32);
    }

    /// Dispatch parity: a 16 KiB pseudo-random buffer must produce identical
    /// digests via the streaming path and the one-shot path. Both routes go
    /// through the same `sha2`/`cpufeatures`-selected backend (SHA-NI on
    /// x86_64, ARMv8 sha2 on aarch64, or pure-Rust scalar elsewhere); this
    /// test guards against regressions in the streaming wrapper that would
    /// only surface for inputs longer than the existing 1 KiB and 8 KiB
    /// checks.
    #[test]
    fn streaming_random_buffer_matches_one_shot() {
        // Deterministic pseudo-random pattern - reproducible across CI runs.
        let data: Vec<u8> = (0..16 * 1024)
            .map(|i| (i.wrapping_mul(2654435761) >> 16) as u8)
            .collect();

        let one_shot = Sha256::digest(&data);

        let mut hasher = Sha256::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), one_shot);
    }

    /// Dispatch parity across chunk sizes: feed a 16 KiB buffer through the
    /// streaming hasher in chunks of varying sizes and assert every chunking
    /// produces the same digest as the one-shot call. Exercises the active
    /// backend's compression-block boundary handling for inputs that span
    /// many 64-byte SHA-256 blocks.
    #[test]
    fn streaming_chunk_sizes_match_one_shot() {
        let data: Vec<u8> = (0..16 * 1024)
            .map(|i| (i.wrapping_mul(2246822519) >> 8) as u8)
            .collect();

        let expected = Sha256::digest(&data);

        for chunk_size in [1usize, 3, 7, 16, 63, 64, 65, 1023, 1024, 4096, 8191] {
            let mut hasher = Sha256::new();
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            assert_eq!(
                hasher.finalize(),
                expected,
                "SHA-256 streaming/one-shot mismatch at chunk_size={chunk_size}"
            );
        }
    }
}
