//! Encoding pair for local and remote character sets.

/// A pair of encoding names for local and remote character sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodingPair {
    local_encoding: String,
    remote_encoding: String,
}

impl EncodingPair {
    /// Creates a new encoding pair.
    ///
    /// # Arguments
    ///
    /// * `local` - The local character encoding name
    /// * `remote` - The remote character encoding name
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::iconv::EncodingPair;
    ///
    /// let pair = EncodingPair::new("utf-8", "iso-8859-1");
    /// assert_eq!(pair.local(), "utf-8");
    /// assert_eq!(pair.remote(), "iso-8859-1");
    /// ```
    #[must_use]
    pub fn new(local: &str, remote: &str) -> Self {
        Self {
            local_encoding: local.to_string(),
            remote_encoding: remote.to_string(),
        }
    }

    /// Returns the local encoding name.
    #[must_use]
    pub fn local(&self) -> &str {
        &self.local_encoding
    }

    /// Returns the remote encoding name.
    #[must_use]
    pub fn remote(&self) -> &str {
        &self.remote_encoding
    }
}
