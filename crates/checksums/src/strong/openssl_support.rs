//! OpenSSL-backed MD4 and MD5 hasher creation for improved throughput.
//!
//! When the `openssl` feature is enabled, this module provides factory functions
//! that create OpenSSL-backed hashers for MD4 and MD5. OpenSSL's hand-optimised
//! assembly provides roughly 2x throughput over pure-Rust implementations for
//! these legacy algorithms.
//!
//! Availability is detected once via `OnceLock` and cached for the process lifetime.
//!
//! upstream: `checksum.c:csum_evp_md()` likewise routes MD4/MD5 through OpenSSL EVP when
//! built with `USE_OPENSSL`, falling back to built-in code if EVP init fails.

#![allow(clippy::module_name_repetitions)]

use openssl::hash::{Hasher, MessageDigest};
use std::sync::OnceLock;

static DETECTED: OnceLock<Result<(), ()>> = OnceLock::new();
static MD4_DIGEST: OnceLock<Option<MessageDigest>> = OnceLock::new();

#[cfg(test)]
static MD4_PROBES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn detect() -> Result<(), ()> {
    Hasher::new(MessageDigest::md5()).map(drop).map_err(drop)
}

/// Probes whether OpenSSL can initialise an MD4 digest.
///
/// OpenSSL 3 ships MD4 only in the legacy provider, so on a stock install the
/// name resolves but `EVP_DigestInit_ex` fails with "unsupported". Probing
/// once per process routes every later MD4 hasher to the built-in
/// implementation instead of retrying OpenSSL, and draining its error queue,
/// for every block.
///
/// upstream: checksum.c:175-186 csum_evp_md() - a failed EVP_DigestInit_ex
/// clears NNI_EVP for the process and falls back to the built-in MD4.
fn probe_md4() -> Option<MessageDigest> {
    #[cfg(test)]
    MD4_PROBES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let digest = MessageDigest::from_name("md4")?;
    Hasher::new(digest).ok().map(|_| digest)
}

fn md4_digest() -> Option<MessageDigest> {
    *MD4_DIGEST.get_or_init(probe_md4)
}

/// Returns whether OpenSSL-backed hashing is available for MD4/MD5.
#[must_use]
pub fn openssl_acceleration_available() -> bool {
    DETECTED.get_or_init(detect).is_ok()
}

/// Creates an MD5 hasher backed by OpenSSL when available.
pub fn new_md5_hasher() -> Option<Hasher> {
    if !openssl_acceleration_available() {
        return None;
    }

    Hasher::new(MessageDigest::md5()).ok()
}

/// Creates an MD4 hasher backed by OpenSSL when available.
pub fn new_md4_hasher() -> Option<Hasher> {
    if !openssl_acceleration_available() {
        return None;
    }

    Hasher::new(md4_digest()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md4_availability_is_probed_once_per_process() {
        for _ in 0..64 {
            let _ = new_md4_hasher();
            let _ = crate::strong::Md4::new();
        }
        let probes = MD4_PROBES.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            probes <= 1,
            "OpenSSL MD4 must be probed at most once per process, saw {probes} probes"
        );
    }

    #[test]
    fn detection_is_deterministic() {
        // Multiple calls should return the same result
        let first = openssl_acceleration_available();
        let second = openssl_acceleration_available();
        let third = openssl_acceleration_available();
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    #[test]
    fn md5_hasher_creation_consistent_with_detection() {
        // If detection says available, MD5 should work
        if openssl_acceleration_available() {
            assert!(
                new_md5_hasher().is_some(),
                "MD5 hasher should be available when OpenSSL is detected"
            );
        }
    }

    #[test]
    fn md5_hasher_produces_correct_digest_length() {
        if let Some(mut hasher) = new_md5_hasher() {
            hasher.update(b"test data").expect("update should succeed");
            let digest = hasher.finish().expect("finish should succeed");
            // MD5 produces 16-byte (128-bit) digests
            assert_eq!(digest.len(), 16);
        }
    }

    #[test]
    fn md5_hasher_produces_known_digest() {
        if let Some(mut hasher) = new_md5_hasher() {
            hasher.update(b"hello").expect("update should succeed");
            let digest = hasher.finish().expect("finish should succeed");
            // MD5("hello") = 5d41402abc4b2a76b9719d911017c592
            let expected: [u8; 16] = [
                0x5d, 0x41, 0x40, 0x2a, 0xbc, 0x4b, 0x2a, 0x76, 0xb9, 0x71, 0x9d, 0x91, 0x10, 0x17,
                0xc5, 0x92,
            ];
            assert_eq!(digest.as_ref(), &expected);
        }
    }

    #[test]
    fn md4_hasher_produces_correct_digest_length() {
        // MD4 may not be available in all OpenSSL builds (deprecated)
        if let Some(mut hasher) = new_md4_hasher() {
            hasher.update(b"test data").expect("update should succeed");
            let digest = hasher.finish().expect("finish should succeed");
            // MD4 produces 16-byte (128-bit) digests
            assert_eq!(digest.len(), 16);
        }
    }

    #[test]
    fn md4_hasher_produces_known_digest() {
        // MD4 may not be available in all OpenSSL builds (deprecated)
        if let Some(mut hasher) = new_md4_hasher() {
            hasher.update(b"hello").expect("update should succeed");
            let digest = hasher.finish().expect("finish should succeed");
            // MD4("hello") = 866437cb7a794bce2b727acc0362ee27
            let expected: [u8; 16] = [
                0x86, 0x64, 0x37, 0xcb, 0x7a, 0x79, 0x4b, 0xce, 0x2b, 0x72, 0x7a, 0xcc, 0x03, 0x62,
                0xee, 0x27,
            ];
            assert_eq!(digest.as_ref(), &expected);
        }
    }

    #[test]
    fn md5_hasher_allows_multiple_updates() {
        if let Some(mut hasher) = new_md5_hasher() {
            hasher.update(b"hel").expect("first update");
            hasher.update(b"lo").expect("second update");
            let digest = hasher.finish().expect("finish should succeed");
            // Should be same as MD5("hello")
            let expected: [u8; 16] = [
                0x5d, 0x41, 0x40, 0x2a, 0xbc, 0x4b, 0x2a, 0x76, 0xb9, 0x71, 0x9d, 0x91, 0x10, 0x17,
                0xc5, 0x92,
            ];
            assert_eq!(digest.as_ref(), &expected);
        }
    }

    #[test]
    fn md5_hasher_empty_input() {
        if let Some(mut hasher) = new_md5_hasher() {
            // Don't call update - digest of empty string
            let digest = hasher.finish().expect("finish should succeed");
            // MD5("") = d41d8cd98f00b204e9800998ecf8427e
            let expected: [u8; 16] = [
                0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
                0x42, 0x7e,
            ];
            assert_eq!(digest.as_ref(), &expected);
        }
    }
}
