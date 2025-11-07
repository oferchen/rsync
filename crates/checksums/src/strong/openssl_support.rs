#![allow(clippy::module_name_repetitions)]

use openssl::hash::{Hasher, MessageDigest};
use std::sync::OnceLock;

static DETECTED: OnceLock<Result<(), ()>> = OnceLock::new();

fn detect() -> Result<(), ()> {
    let md5 = MessageDigest::md5();

    Hasher::new(md5).map_err(|_| ())?;
    if let Some(md4) = MessageDigest::from_name("md4") {
        let _ = Hasher::new(md4);
    }
    Ok(())
}

/// Returns whether OpenSSL-backed hashing is available for MD4/MD5.
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

    let digest = MessageDigest::from_name("md4")?;
    Hasher::new(digest).ok()
}
