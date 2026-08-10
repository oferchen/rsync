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

    /// Known valid SSH cipher names accepted by russh and OpenSSH.
    const VALID_SSH_CIPHERS: &[&str] = &[
        "aes128-gcm@openssh.com",
        "aes256-gcm@openssh.com",
        "chacha20-poly1305@openssh.com",
        "aes256-ctr",
        "aes192-ctr",
        "aes128-ctr",
    ];

    /// Verifies that `has_aes_ni()` returns a consistent cached result across
    /// multiple invocations, confirming `OnceLock` stability.
    #[test]
    fn has_aes_ni_returns_stable_result() {
        let first = has_aes_ni();
        let second = has_aes_ni();
        assert_eq!(first, second, "cached result must be stable");
    }

    /// Verifies that `has_aes_ni()` returns a bool without panicking.
    /// On x86_64 this exercises the CPUID path; on other architectures
    /// it exercises the fallback path.
    #[test]
    fn has_aes_ni_does_not_panic() {
        let _result: bool = has_aes_ni();
    }

    /// Verifies `default_ciphers()` returns a non-empty list.
    #[test]
    fn default_ciphers_is_non_empty() {
        assert!(
            !default_ciphers().is_empty(),
            "cipher list must not be empty"
        );
    }

    /// Verifies the cipher list contains exactly three entries.
    #[test]
    fn default_ciphers_returns_three() {
        let ciphers = default_ciphers();
        assert_eq!(ciphers.len(), 3);
    }

    /// Verifies every cipher name in the default list is a recognized SSH
    /// cipher string compatible with russh and OpenSSH.
    #[test]
    fn default_ciphers_all_valid_names() {
        for cipher in default_ciphers() {
            assert!(
                VALID_SSH_CIPHERS.contains(&cipher.as_str()),
                "unexpected cipher not in valid set: {cipher}"
            );
        }
    }

    /// Verifies the cipher list contains no duplicate entries.
    #[test]
    fn default_ciphers_no_duplicates() {
        let ciphers = default_ciphers();
        let mut seen = std::collections::HashSet::new();
        for c in &ciphers {
            assert!(seen.insert(c), "duplicate cipher: {c}");
        }
    }

    /// Verifies that `chacha20-poly1305@openssh.com` is always present in the
    /// cipher list regardless of hardware AES support, since it serves as the
    /// universal software fallback.
    #[test]
    fn chacha20_poly1305_always_present() {
        let ciphers = default_ciphers();
        assert!(
            ciphers.iter().any(|c| c == "chacha20-poly1305@openssh.com"),
            "ChaCha20-Poly1305 must always be in the cipher list as fallback"
        );
    }

    /// Verifies that at least one AES-GCM cipher is always present, ensuring
    /// hardware-accelerated paths are available when the server supports them.
    #[test]
    fn aes_gcm_always_present() {
        let ciphers = default_ciphers();
        assert!(
            ciphers
                .iter()
                .any(|c| c.contains("aes") && c.contains("gcm")),
            "at least one AES-GCM cipher must be in the list"
        );
    }

    /// Verifies that the cipher order reflects hardware AES detection:
    /// AES-GCM first when AES-NI is available, ChaCha20 first otherwise.
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

    /// When hardware AES is detected, verifies that both AES-GCM variants
    /// appear before ChaCha20-Poly1305 in the preference list.
    #[test]
    fn with_aes_ni_both_gcm_before_chacha() {
        let ciphers = default_ciphers();
        if has_aes_ni() {
            let chacha_pos = ciphers
                .iter()
                .position(|c| c.contains("chacha"))
                .expect("chacha must be present");
            for (i, c) in ciphers.iter().enumerate() {
                if c.contains("gcm") {
                    assert!(
                        i < chacha_pos,
                        "AES-GCM cipher {c} at index {i} should precede ChaCha at index {chacha_pos}"
                    );
                }
            }
        }
    }

    /// When hardware AES is absent, verifies that ChaCha20-Poly1305 is the
    /// first cipher in the preference list.
    #[test]
    fn without_aes_ni_chacha_is_first() {
        let ciphers = default_ciphers();
        if !has_aes_ni() {
            assert_eq!(
                ciphers[0], "chacha20-poly1305@openssh.com",
                "ChaCha20 must be first without hardware AES"
            );
        }
    }

    /// Verifies that all cipher names follow the SSH naming convention:
    /// lowercase alphanumeric with hyphens, optionally suffixed with
    /// `@openssh.com` for extension ciphers.
    #[test]
    fn cipher_names_follow_ssh_naming_convention() {
        for cipher in default_ciphers() {
            let base = cipher.strip_suffix("@openssh.com").unwrap_or(&cipher);
            assert!(
                base.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'),
                "cipher name contains invalid characters: {cipher}"
            );
            assert!(
                !base.is_empty(),
                "cipher base name must not be empty: {cipher}"
            );
        }
    }

    /// Verifies that `default_ciphers()` returns owned `String` values (not
    /// empty strings), confirming proper allocation.
    #[test]
    fn cipher_strings_are_non_empty() {
        for cipher in default_ciphers() {
            assert!(!cipher.is_empty(), "cipher string must not be empty");
            assert!(cipher.len() > 5, "cipher name suspiciously short: {cipher}");
        }
    }

    /// Verifies the exact cipher set matches the three ciphers documented in
    /// `default_ciphers()` - no more, no less.
    #[test]
    fn exact_cipher_set() {
        let ciphers: std::collections::HashSet<String> = default_ciphers().into_iter().collect();
        let expected: std::collections::HashSet<String> = [
            "aes128-gcm@openssh.com",
            "aes256-gcm@openssh.com",
            "chacha20-poly1305@openssh.com",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(ciphers, expected, "cipher set must match expected ciphers");
    }
}
