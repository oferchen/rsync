//! crates/protocol/src/iconv.rs
//!
//! Filename encoding conversion (iconv) for cross-platform rsync transfers.
//!
//! When the local and remote systems use different character encodings for
//! filenames, this module handles the conversion. This mirrors rsync's `--iconv`
//! option.
//!
//! # Example
//!
//! ```ignore
//! use protocol::iconv::FilenameConverter;
//!
//! // Convert filenames from UTF-8 (remote) to ISO-8859-1 (local)
//! let converter = FilenameConverter::new("UTF-8", "ISO-8859-1")?;
//! let local_name = converter.remote_to_local(b"caf\xc3\xa9")?;
//! ```

use std::borrow::Cow;

/// Error type for encoding conversion failures.
#[derive(Debug, Clone)]
pub struct ConversionError {
    /// Description of the error.
    pub message: String,
    /// The bytes that failed to convert (if applicable).
    pub bytes: Option<Vec<u8>>,
}

impl std::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ConversionError {}

impl ConversionError {
    /// Creates a new conversion error with the given message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            bytes: None,
        }
    }

    /// Creates a new conversion error with associated byte data.
    #[must_use]
    pub fn with_bytes(message: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            message: message.into(),
            bytes: Some(bytes),
        }
    }
}

/// Filename encoding converter.
///
/// Handles conversion between local and remote character encodings for
/// filenames during rsync transfers. When iconv is disabled at compile time,
/// this type still exists but performs no conversion.
#[derive(Debug, Clone)]
pub struct FilenameConverter {
    #[cfg(feature = "iconv")]
    local_encoding: &'static encoding_rs::Encoding,
    #[cfg(feature = "iconv")]
    remote_encoding: &'static encoding_rs::Encoding,
}

impl Default for FilenameConverter {
    fn default() -> Self {
        Self::identity()
    }
}

impl FilenameConverter {
    /// Creates a converter that performs no conversion (identity mapping).
    ///
    /// This is the default when no encoding conversion is needed.
    #[must_use]
    pub fn identity() -> Self {
        Self {
            #[cfg(feature = "iconv")]
            local_encoding: encoding_rs::UTF_8,
            #[cfg(feature = "iconv")]
            remote_encoding: encoding_rs::UTF_8,
        }
    }

    /// Creates a new filename converter with the specified encodings.
    ///
    /// # Arguments
    ///
    /// * `local_charset` - The character set used on the local system
    /// * `remote_charset` - The character set used on the remote system
    ///
    /// # Errors
    ///
    /// Returns an error if either encoding name is not recognized.
    #[cfg(feature = "iconv")]
    pub fn new(local_charset: &str, remote_charset: &str) -> Result<Self, ConversionError> {
        let local_encoding = encoding_rs::Encoding::for_label(local_charset.as_bytes())
            .ok_or_else(|| ConversionError::new(format!("unknown encoding: {local_charset}")))?;

        let remote_encoding = encoding_rs::Encoding::for_label(remote_charset.as_bytes())
            .ok_or_else(|| ConversionError::new(format!("unknown encoding: {remote_charset}")))?;

        Ok(Self {
            local_encoding,
            remote_encoding,
        })
    }

    /// Creates a converter from encoding labels, falling back to UTF-8 for unknown encodings.
    ///
    /// This is more lenient than [`Self::new`] and won't fail for unrecognized encodings.
    #[cfg(feature = "iconv")]
    #[must_use]
    pub fn new_lenient(local_charset: &str, remote_charset: &str) -> Self {
        let local_encoding = encoding_rs::Encoding::for_label(local_charset.as_bytes())
            .unwrap_or(encoding_rs::UTF_8);

        let remote_encoding = encoding_rs::Encoding::for_label(remote_charset.as_bytes())
            .unwrap_or(encoding_rs::UTF_8);

        Self {
            local_encoding,
            remote_encoding,
        }
    }

    /// Returns true if this converter performs actual conversion.
    ///
    /// When local and remote encodings are the same, no conversion is needed.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        #[cfg(feature = "iconv")]
        {
            self.local_encoding == self.remote_encoding
        }
        #[cfg(not(feature = "iconv"))]
        {
            true
        }
    }

    /// Converts a filename from remote encoding to local encoding.
    ///
    /// This is used when receiving file lists from a remote rsync.
    ///
    /// # Arguments
    ///
    /// * `bytes` - The filename bytes in remote encoding
    ///
    /// # Returns
    ///
    /// The filename bytes converted to local encoding. Returns the input
    /// unchanged if no conversion is needed or the iconv feature is disabled.
    #[cfg(feature = "iconv")]
    pub fn remote_to_local<'a>(&self, bytes: &'a [u8]) -> Result<Cow<'a, [u8]>, ConversionError> {
        if self.is_identity() {
            return Ok(Cow::Borrowed(bytes));
        }

        // First decode from remote encoding to UTF-8 internal representation
        let (decoded, _, had_errors) = self.remote_encoding.decode(bytes);
        if had_errors {
            return Err(ConversionError::with_bytes(
                format!(
                    "invalid {} sequence in filename",
                    self.remote_encoding.name()
                ),
                bytes.to_vec(),
            ));
        }

        // Then encode to local encoding
        let (encoded, _, had_errors) = self.local_encoding.encode(&decoded);
        if had_errors {
            return Err(ConversionError::with_bytes(
                format!(
                    "cannot represent filename in {}",
                    self.local_encoding.name()
                ),
                bytes.to_vec(),
            ));
        }

        Ok(Cow::Owned(encoded.into_owned()))
    }

    /// Converts a filename from local encoding to remote encoding.
    ///
    /// This is used when sending file lists to a remote rsync.
    ///
    /// # Arguments
    ///
    /// * `bytes` - The filename bytes in local encoding
    ///
    /// # Returns
    ///
    /// The filename bytes converted to remote encoding. Returns the input
    /// unchanged if no conversion is needed or the iconv feature is disabled.
    #[cfg(feature = "iconv")]
    pub fn local_to_remote<'a>(&self, bytes: &'a [u8]) -> Result<Cow<'a, [u8]>, ConversionError> {
        if self.is_identity() {
            return Ok(Cow::Borrowed(bytes));
        }

        // First decode from local encoding to UTF-8 internal representation
        let (decoded, _, had_errors) = self.local_encoding.decode(bytes);
        if had_errors {
            return Err(ConversionError::with_bytes(
                format!(
                    "invalid {} sequence in filename",
                    self.local_encoding.name()
                ),
                bytes.to_vec(),
            ));
        }

        // Then encode to remote encoding
        let (encoded, _, had_errors) = self.remote_encoding.encode(&decoded);
        if had_errors {
            return Err(ConversionError::with_bytes(
                format!(
                    "cannot represent filename in {}",
                    self.remote_encoding.name()
                ),
                bytes.to_vec(),
            ));
        }

        Ok(Cow::Owned(encoded.into_owned()))
    }

    /// No-op conversion when iconv feature is disabled.
    #[cfg(not(feature = "iconv"))]
    #[allow(clippy::unnecessary_wraps)]
    pub fn remote_to_local<'a>(&self, bytes: &'a [u8]) -> Result<Cow<'a, [u8]>, ConversionError> {
        Ok(Cow::Borrowed(bytes))
    }

    /// No-op conversion when iconv feature is disabled.
    #[cfg(not(feature = "iconv"))]
    #[allow(clippy::unnecessary_wraps)]
    pub fn local_to_remote<'a>(&self, bytes: &'a [u8]) -> Result<Cow<'a, [u8]>, ConversionError> {
        Ok(Cow::Borrowed(bytes))
    }

    /// Returns the local encoding name.
    #[must_use]
    pub fn local_encoding_name(&self) -> &'static str {
        #[cfg(feature = "iconv")]
        {
            self.local_encoding.name()
        }
        #[cfg(not(feature = "iconv"))]
        {
            "UTF-8"
        }
    }

    /// Returns the remote encoding name.
    #[must_use]
    pub fn remote_encoding_name(&self) -> &'static str {
        #[cfg(feature = "iconv")]
        {
            self.remote_encoding.name()
        }
        #[cfg(not(feature = "iconv"))]
        {
            "UTF-8"
        }
    }
}

/// Creates a [`FilenameConverter`] from the locale, using UTF-8 as the remote encoding.
///
/// This is used when `--iconv=.` is specified (locale on one side, UTF-8 on the other).
#[cfg(feature = "iconv")]
#[must_use]
pub fn converter_from_locale() -> FilenameConverter {
    // On most modern systems, the locale is UTF-8, so this is often a no-op.
    // For truly accurate locale detection, we would need to query LC_CTYPE,
    // but UTF-8 is a reasonable default for modern systems.
    FilenameConverter::identity()
}

/// Creates a [`FilenameConverter`] from the locale (no-op when iconv is disabled).
#[cfg(not(feature = "iconv"))]
#[must_use]
pub fn converter_from_locale() -> FilenameConverter {
    FilenameConverter::identity()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_converter_passes_through() {
        let conv = FilenameConverter::identity();
        assert!(conv.is_identity());

        let input = b"hello.txt";
        let result = conv.remote_to_local(input).unwrap();
        assert_eq!(&*result, input);

        let result = conv.local_to_remote(input).unwrap();
        assert_eq!(&*result, input);
    }

    #[test]
    fn default_is_identity() {
        let conv = FilenameConverter::default();
        assert!(conv.is_identity());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn utf8_to_latin1_conversion() {
        let conv = FilenameConverter::new("ISO-8859-1", "UTF-8").unwrap();
        assert!(!conv.is_identity());

        // UTF-8 "café" -> ISO-8859-1
        let utf8_bytes = "café".as_bytes();
        let result = conv.remote_to_local(utf8_bytes).unwrap();

        // In ISO-8859-1, "café" is: c(0x63) a(0x61) f(0x66) é(0xe9)
        assert_eq!(&*result, &[0x63, 0x61, 0x66, 0xe9]);
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn latin1_to_utf8_conversion() {
        let conv = FilenameConverter::new("ISO-8859-1", "UTF-8").unwrap();

        // ISO-8859-1 "café" -> UTF-8
        let latin1_bytes = &[0x63, 0x61, 0x66, 0xe9];
        let result = conv.local_to_remote(latin1_bytes).unwrap();

        // In UTF-8, "café" is the standard UTF-8 encoding
        assert_eq!(&*result, "café".as_bytes());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn unknown_encoding_returns_error() {
        let result = FilenameConverter::new("INVALID-ENCODING", "UTF-8");
        assert!(result.is_err());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn lenient_constructor_falls_back_to_utf8() {
        let conv = FilenameConverter::new_lenient("INVALID-ENCODING", "ALSO-INVALID");
        // Both fall back to UTF-8, so it's an identity converter
        assert!(conv.is_identity());
    }

    #[test]
    fn encoding_names_reported() {
        let conv = FilenameConverter::identity();
        assert_eq!(conv.local_encoding_name(), "UTF-8");
        assert_eq!(conv.remote_encoding_name(), "UTF-8");
    }
}
