//! SSH cipher selection based on hardware AES capability.
//!
//! Detects AES-NI at runtime on x86/x86_64 and AES crypto extensions on
//! aarch64, then orders the cipher preference list accordingly. Systems with
//! hardware AES prefer AES-GCM ciphers for throughput; systems without prefer
//! ChaCha20-Poly1305 which has consistent performance across all platforms.

use std::sync::OnceLock;

/// Cached hardware AES detection result.
static HAS_AES_NI: OnceLock<bool> = OnceLock::new();

/// Returns `true` if the CPU supports hardware AES instructions.
///
/// On x86/x86_64, queries CPUID for AES-NI support. On aarch64, checks for
/// ARMv8 Cryptography Extensions. On all other architectures, returns `false`.
/// The result is cached in a `OnceLock` for subsequent calls.
#[must_use]
pub fn has_aes_ni() -> bool {
    *HAS_AES_NI.get_or_init(detect_aes_ni)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn detect_aes_ni() -> bool {
    std::arch::is_x86_feature_detected!("aes")
}

#[cfg(target_arch = "aarch64")]
fn detect_aes_ni() -> bool {
    std::arch::is_aarch64_feature_detected!("aes")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn detect_aes_ni() -> bool {
    false
}

/// Returns the default cipher preference list for embedded SSH connections.
///
/// With hardware AES support, AES-GCM ciphers are preferred for their
/// hardware-accelerated throughput. Without hardware AES, ChaCha20-Poly1305 is
/// preferred as it provides consistent performance in software.
///
/// # Cipher ordering
///
/// - **With AES:** `aes128-gcm@openssh.com`, `aes256-gcm@openssh.com`,
///   `chacha20-poly1305@openssh.com`
/// - **Without AES:** `chacha20-poly1305@openssh.com`,
///   `aes128-gcm@openssh.com`, `aes256-gcm@openssh.com`
#[must_use]
pub fn default_ciphers() -> Vec<String> {
    if has_aes_ni() {
        vec![
            "aes128-gcm@openssh.com".to_owned(),
            "aes256-gcm@openssh.com".to_owned(),
            "chacha20-poly1305@openssh.com".to_owned(),
        ]
    } else {
        vec![
            "chacha20-poly1305@openssh.com".to_owned(),
            "aes128-gcm@openssh.com".to_owned(),
            "aes256-gcm@openssh.com".to_owned(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_aes_ni_returns_stable_result() {
        let first = has_aes_ni();
        let second = has_aes_ni();
        assert_eq!(first, second, "cached result must be stable");
    }

    #[test]
    fn default_ciphers_returns_three() {
        let ciphers = default_ciphers();
        assert_eq!(ciphers.len(), 3);
    }

    #[test]
    fn default_ciphers_all_valid_names() {
        for cipher in default_ciphers() {
            assert!(
                cipher == "aes128-gcm@openssh.com"
                    || cipher == "aes256-gcm@openssh.com"
                    || cipher == "chacha20-poly1305@openssh.com",
                "unexpected cipher: {cipher}"
            );
        }
    }

    #[test]
    fn default_ciphers_no_duplicates() {
        let ciphers = default_ciphers();
        let mut seen = std::collections::HashSet::new();
        for c in &ciphers {
            assert!(seen.insert(c), "duplicate cipher: {c}");
        }
    }

    #[test]
    fn aes_ni_affects_cipher_order() {
        let ciphers = default_ciphers();
        if has_aes_ni() {
            assert!(
                ciphers[0].contains("aes"),
                "AES should be first with hardware AES"
            );
        } else {
            assert!(
                ciphers[0].contains("chacha"),
                "ChaCha should be first without hardware AES"
            );
        }
    }
}
