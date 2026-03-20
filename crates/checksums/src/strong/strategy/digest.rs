//! Fixed-capacity checksum digest container.

use std::fmt;

/// Maximum digest length for any supported algorithm (SHA-512 = 64 bytes).
pub const MAX_DIGEST_LEN: usize = 64;

/// A checksum digest with a fixed maximum capacity.
///
/// Avoids heap allocation for digest storage by using a fixed-size buffer
/// that can hold any supported digest. The actual digest length varies
/// by algorithm.
#[derive(Clone, Copy)]
pub struct ChecksumDigest {
    buffer: [u8; MAX_DIGEST_LEN],
    len: usize,
}

impl ChecksumDigest {
    /// Creates a new digest from a byte slice.
    ///
    /// # Panics
    ///
    /// Panics if `bytes.len() > MAX_DIGEST_LEN`.
    #[must_use]
    pub fn new(bytes: &[u8]) -> Self {
        assert!(
            bytes.len() <= MAX_DIGEST_LEN,
            "digest length {} exceeds maximum {}",
            bytes.len(),
            MAX_DIGEST_LEN
        );
        let mut buffer = [0u8; MAX_DIGEST_LEN];
        buffer[..bytes.len()].copy_from_slice(bytes);
        Self {
            buffer,
            len: bytes.len(),
        }
    }

    /// Returns the digest length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the digest is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the digest as a byte slice.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buffer[..self.len]
    }

    /// Copies the digest into the provided buffer.
    ///
    /// # Panics
    ///
    /// Panics if `out.len() < self.len()`.
    pub fn copy_to(&self, out: &mut [u8]) {
        assert!(
            out.len() >= self.len,
            "output buffer too small: {} < {}",
            out.len(),
            self.len
        );
        out[..self.len].copy_from_slice(&self.buffer[..self.len]);
    }

    /// Returns the digest truncated to the specified length.
    ///
    /// If `len >= self.len()`, returns a copy of the full digest.
    #[must_use]
    pub fn truncated(&self, len: usize) -> Self {
        let actual_len = len.min(self.len);
        let mut buffer = [0u8; MAX_DIGEST_LEN];
        buffer[..actual_len].copy_from_slice(&self.buffer[..actual_len]);
        Self {
            buffer,
            len: actual_len,
        }
    }
}

impl AsRef<[u8]> for ChecksumDigest {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl PartialEq for ChecksumDigest {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for ChecksumDigest {}

impl fmt::Debug for ChecksumDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChecksumDigest({:02x?})", self.as_bytes())
    }
}

impl fmt::Display for ChecksumDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.as_bytes() {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
