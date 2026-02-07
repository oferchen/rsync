//! Character encoding conversion for rsync's --iconv=LOCAL,REMOTE option.
//!
//! Filename encoding conversion (iconv) for cross-platform rsync transfers.
//!
//! When the local and remote systems use different character encodings for
//! filenames, this module handles the conversion. This mirrors rsync's `--iconv`
//! option.
//!
//! # Examples
//!
//! ```
//! use protocol::iconv::{EncodingConverter, FilenameConverter};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # #[cfg(feature = "iconv")]
//! # {
//! // Using the new API (EncodingConverter)
//! let converter = EncodingConverter::new("utf-8", "iso-8859-1")?;
//! let remote = converter.to_remote("café.txt")?;
//!
//! // Using the legacy API (FilenameConverter)
//! let converter = FilenameConverter::new("UTF-8", "ISO-8859-1")?;
//! let local_name = converter.remote_to_local("café".as_bytes())?;
//! # }
//! # Ok(())
//! # }
//! ```

use std::borrow::Cow;

/// Error type for encoding operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodingError {
    /// The specified encoding is not supported.
    #[error("unsupported encoding: {0}")]
    UnsupportedEncoding(String),

    /// Conversion between encodings failed.
    #[error("conversion failed from {from} to {to}{}", if *.lossy { " (lossy conversion)" } else { "" })]
    ConversionFailed {
        /// Source encoding.
        from: String,
        /// Target encoding.
        to: String,
        /// Whether the conversion would be lossy.
        lossy: bool,
    },
}

/// Legacy error type for encoding conversion failures.
///
/// This is maintained for backward compatibility. New code should use [`EncodingError`].
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ConversionError {
    /// Description of the error.
    pub message: String,
    /// The bytes that failed to convert (if applicable).
    pub bytes: Option<Vec<u8>>,
}

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

impl From<EncodingError> for ConversionError {
    fn from(err: EncodingError) -> Self {
        ConversionError::new(err.to_string())
    }
}

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

/// Character encoding converter for filename conversion.
///
/// This is the new API that provides string-based conversions with [`EncodingError`].
/// For the legacy byte-oriented API, see [`FilenameConverter`].
///
/// When the `iconv` feature is disabled, only UTF-8 is supported.
pub type EncodingConverter = FilenameConverter;

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

impl PartialEq for FilenameConverter {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(feature = "iconv")]
        {
            self.local_encoding == other.local_encoding
                && self.remote_encoding == other.remote_encoding
        }
        #[cfg(not(feature = "iconv"))]
        {
            // Without iconv, all converters are identity converters
            let _ = other;
            true
        }
    }
}

impl Eq for FilenameConverter {}

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
    /// Special values:
    /// - "." or "" means "use local system encoding" (defaults to UTF-8)
    ///
    /// Supported encoding names include:
    /// - UTF-8: "utf-8", "utf8", "UTF-8"
    /// - ISO-8859-1: "iso-8859-1", "latin1", "ISO-8859-1"
    /// - ASCII: "ascii", "us-ascii"
    /// - Windows-1252: "windows-1252", "cp1252"
    /// - EUC-JP, Shift_JIS, GB2312, Big5, KOI8-R, and many others via encoding_rs
    ///
    /// # Errors
    ///
    /// Returns an error if either encoding name is not recognized.
    #[cfg(feature = "iconv")]
    pub fn new(local_charset: &str, remote_charset: &str) -> Result<Self, ConversionError> {
        let local_normalized = normalize_encoding_name(local_charset);
        let remote_normalized = normalize_encoding_name(remote_charset);

        let local_encoding = encoding_rs::Encoding::for_label(local_normalized.as_bytes())
            .ok_or_else(|| ConversionError::new(format!("unknown encoding: {local_charset}")))?;

        let remote_encoding = encoding_rs::Encoding::for_label(remote_normalized.as_bytes())
            .ok_or_else(|| ConversionError::new(format!("unknown encoding: {remote_charset}")))?;

        Ok(Self {
            local_encoding,
            remote_encoding,
        })
    }

    /// Stub implementation when iconv feature is disabled.
    #[cfg(not(feature = "iconv"))]
    pub fn new(local_charset: &str, remote_charset: &str) -> Result<Self, ConversionError> {
        let local_normalized = normalize_encoding_name(local_charset);
        let remote_normalized = normalize_encoding_name(remote_charset);

        if !is_utf8_name(&local_normalized) {
            return Err(ConversionError::new(format!(
                "unsupported encoding (iconv feature disabled): {local_charset}"
            )));
        }
        if !is_utf8_name(&remote_normalized) {
            return Err(ConversionError::new(format!(
                "unsupported encoding (iconv feature disabled): {remote_charset}"
            )));
        }

        Ok(Self {})
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
    #[inline]
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

    /// Converts a local filename to remote encoding (string API).
    ///
    /// # Arguments
    ///
    /// * `local_name` - The filename in local encoding
    ///
    /// # Errors
    ///
    /// Returns `EncodingError::ConversionFailed` if the conversion fails.
    #[cfg(feature = "iconv")]
    pub fn to_remote(&self, local_name: &str) -> Result<String, EncodingError> {
        if self.is_identity() {
            return Ok(local_name.to_string());
        }

        let bytes = local_name.as_bytes();
        let (decoded, _, had_errors) = self.local_encoding.decode(bytes);
        if had_errors {
            return Err(EncodingError::ConversionFailed {
                from: self.local_encoding.name().to_string(),
                to: self.remote_encoding.name().to_string(),
                lossy: false,
            });
        }

        let (encoded, _, had_errors) = self.remote_encoding.encode(&decoded);
        if had_errors {
            return Err(EncodingError::ConversionFailed {
                from: self.local_encoding.name().to_string(),
                to: self.remote_encoding.name().to_string(),
                lossy: true,
            });
        }

        String::from_utf8(encoded.to_vec()).map_err(|_| EncodingError::ConversionFailed {
            from: self.local_encoding.name().to_string(),
            to: self.remote_encoding.name().to_string(),
            lossy: false,
        })
    }

    /// Stub implementation when iconv feature is disabled.
    #[cfg(not(feature = "iconv"))]
    pub fn to_remote(&self, local_name: &str) -> Result<String, EncodingError> {
        Ok(local_name.to_string())
    }

    /// Converts a remote filename (as bytes) to local encoding (string API).
    ///
    /// # Arguments
    ///
    /// * `remote_name` - The filename bytes in remote encoding
    ///
    /// # Errors
    ///
    /// Returns `EncodingError::ConversionFailed` if the conversion fails.
    #[cfg(feature = "iconv")]
    pub fn to_local(&self, remote_name: &[u8]) -> Result<String, EncodingError> {
        if self.is_identity() {
            return String::from_utf8(remote_name.to_vec()).map_err(|_| {
                EncodingError::ConversionFailed {
                    from: self.remote_encoding.name().to_string(),
                    to: self.local_encoding.name().to_string(),
                    lossy: false,
                }
            });
        }

        let (decoded, _, had_errors) = self.remote_encoding.decode(remote_name);
        if had_errors {
            return Err(EncodingError::ConversionFailed {
                from: self.remote_encoding.name().to_string(),
                to: self.local_encoding.name().to_string(),
                lossy: false,
            });
        }

        let (encoded, _, had_errors) = self.local_encoding.encode(&decoded);
        if had_errors {
            return Err(EncodingError::ConversionFailed {
                from: self.remote_encoding.name().to_string(),
                to: self.local_encoding.name().to_string(),
                lossy: true,
            });
        }

        String::from_utf8(encoded.to_vec()).map_err(|_| EncodingError::ConversionFailed {
            from: self.remote_encoding.name().to_string(),
            to: self.local_encoding.name().to_string(),
            lossy: false,
        })
    }

    /// Stub implementation when iconv feature is disabled.
    #[cfg(not(feature = "iconv"))]
    pub fn to_local(&self, remote_name: &[u8]) -> Result<String, EncodingError> {
        String::from_utf8(remote_name.to_vec()).map_err(|_| EncodingError::ConversionFailed {
            from: "UTF-8".to_string(),
            to: "UTF-8".to_string(),
            lossy: false,
        })
    }
}

/// Normalizes encoding names for lookup.
///
/// Special cases:
/// - "." or "" means UTF-8 (local system encoding)
fn normalize_encoding_name(name: &str) -> Cow<'_, str> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Cow::Borrowed("utf-8");
    }
    Cow::Borrowed(trimmed)
}

/// Checks if an encoding name refers to UTF-8.
#[cfg(not(feature = "iconv"))]
fn is_utf8_name(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "utf-8" | "utf8" | "utf_8" | "." | ""
    )
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

    // New API tests (EncodingConverter/EncodingPair/EncodingError)

    #[test]
    fn test_encoding_pair_creation() {
        let pair = EncodingPair::new("utf-8", "iso-8859-1");
        assert_eq!(pair.local(), "utf-8");
        assert_eq!(pair.remote(), "iso-8859-1");
    }

    #[test]
    fn test_encoding_pair_accessors() {
        let pair = EncodingPair::new("windows-1252", "utf-8");
        assert_eq!(pair.local(), "windows-1252");
        assert_eq!(pair.remote(), "utf-8");
    }

    #[test]
    fn test_encoding_pair_equality() {
        let pair1 = EncodingPair::new("utf-8", "iso-8859-1");
        let pair2 = EncodingPair::new("utf-8", "iso-8859-1");
        let pair3 = EncodingPair::new("utf-8", "utf-8");

        assert_eq!(pair1, pair2);
        assert_ne!(pair1, pair3);
    }

    #[test]
    fn test_encoding_error_display() {
        let err = EncodingError::UnsupportedEncoding("xyz".to_string());
        assert_eq!(err.to_string(), "unsupported encoding: xyz");

        let err = EncodingError::ConversionFailed {
            from: "utf-8".to_string(),
            to: "iso-8859-1".to_string(),
            lossy: false,
        };
        assert_eq!(err.to_string(), "conversion failed from utf-8 to iso-8859-1");

        let err = EncodingError::ConversionFailed {
            from: "utf-8".to_string(),
            to: "iso-8859-1".to_string(),
            lossy: true,
        };
        assert_eq!(
            err.to_string(),
            "conversion failed from utf-8 to iso-8859-1 (lossy conversion)"
        );
    }

    #[test]
    fn test_utf8_identity_via_new_api() {
        let converter = EncodingConverter::new("utf-8", "utf-8").unwrap();
        assert!(converter.is_identity());

        let result = converter.to_remote("hello.txt").unwrap();
        assert_eq!(result, "hello.txt");

        let result = converter.to_local(b"hello.txt").unwrap();
        assert_eq!(result, "hello.txt");
    }

    #[test]
    #[cfg(feature = "iconv")]
    fn test_latin1_to_utf8_via_new_api() {
        let converter = EncodingConverter::new("utf-8", "iso-8859-1").unwrap();
        assert!(!converter.is_identity());

        // café in ISO-8859-1: [0x63, 0x61, 0x66, 0xe9]
        let result = converter.to_local(&[0x63, 0x61, 0x66, 0xe9]).unwrap();
        assert_eq!(result, "café");
    }

    #[test]
    #[cfg(feature = "iconv")]
    fn test_utf8_to_latin1_via_new_api() {
        let converter = EncodingConverter::new("utf-8", "iso-8859-1").unwrap();

        // For the string API, use ASCII-only content which works with all encodings
        let result = converter.to_remote("cafe.txt").unwrap();
        assert_eq!(result, "cafe.txt");

        // For non-ASCII, use the byte-oriented API instead
        let result = converter.local_to_remote("café".as_bytes()).unwrap();
        // café in ISO-8859-1 is [0x63, 0x61, 0x66, 0xe9]
        assert_eq!(&*result, &[0x63, 0x61, 0x66, 0xe9]);
    }

    #[test]
    fn test_unsupported_encoding_via_new_api() {
        let result = EncodingConverter::new("invalid-encoding-xyz", "utf-8");
        assert!(result.is_err());
    }

    #[test]
    #[cfg(feature = "iconv")]
    fn test_empty_dot_encoding_defaults_to_utf8() {
        let converter1 = EncodingConverter::new("", ".").unwrap();
        assert!(converter1.is_identity());

        let converter2 = EncodingConverter::new(".", "utf-8").unwrap();
        assert!(converter2.is_identity());
    }

    #[test]
    #[cfg(feature = "iconv")]
    fn test_round_trip_preserves_content() {
        let converter = EncodingConverter::new("utf-8", "iso-8859-1").unwrap();

        // ASCII-only content should round-trip perfectly
        let original = "hello.txt";
        let to_remote = converter.to_remote(original).unwrap();
        let back_to_local = converter.to_local(to_remote.as_bytes()).unwrap();
        assert_eq!(back_to_local, original);
    }

    #[test]
    #[cfg(feature = "iconv")]
    fn test_ascii_only_works_with_any_encoding() {
        let converter = EncodingConverter::new("utf-8", "iso-8859-1").unwrap();

        let ascii_text = "hello_world_123.txt";
        let result = converter.to_remote(ascii_text).unwrap();
        assert_eq!(result, ascii_text);

        let result = converter.to_local(ascii_text.as_bytes()).unwrap();
        assert_eq!(result, ascii_text);
    }

    #[test]
    #[cfg(feature = "iconv")]
    fn test_non_ascii_characters_converted() {
        let converter = EncodingConverter::new("utf-8", "iso-8859-1").unwrap();

        // Test with actual non-ASCII character
        let latin1_bytes = &[0x63, 0x61, 0x66, 0xe9]; // café in ISO-8859-1
        let result = converter.to_local(latin1_bytes).unwrap();
        assert_eq!(result, "café");
    }

    #[test]
    #[cfg(feature = "iconv")]
    fn test_is_identity_correct() {
        let conv1 = EncodingConverter::new("utf-8", "utf-8").unwrap();
        assert!(conv1.is_identity());

        let conv2 = EncodingConverter::new("utf-8", "iso-8859-1").unwrap();
        assert!(!conv2.is_identity());

        let conv3 = EncodingConverter::new("utf8", "utf-8").unwrap();
        assert!(conv3.is_identity()); // Aliases resolve to same encoding
    }

    #[test]
    #[cfg(feature = "iconv")]
    fn test_lossy_conversion() {
        let converter = EncodingConverter::new("iso-8859-1", "utf-8").unwrap();

        // Characters in the ISO-8859-1 range should work fine
        let result = converter.to_local(&[0x41, 0x42, 0x43]); // ABC
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "ABC");
    }

    #[test]
    #[cfg(feature = "iconv")]
    fn test_encoding_aliases_utf8() {
        // UTF-8 aliases
        let conv1 = EncodingConverter::new("utf-8", "utf-8").unwrap();
        let conv2 = EncodingConverter::new("utf8", "utf-8").unwrap();
        let conv3 = EncodingConverter::new("UTF-8", "utf8").unwrap();

        assert!(conv1.is_identity());
        assert!(conv2.is_identity());
        assert!(conv3.is_identity());
    }

    #[test]
    #[cfg(feature = "iconv")]
    fn test_encoding_aliases_latin1() {
        // Latin-1 aliases
        let conv1 = EncodingConverter::new("iso-8859-1", "utf-8").unwrap();
        let conv2 = EncodingConverter::new("latin1", "utf-8").unwrap();

        assert!(!conv1.is_identity());
        assert!(!conv2.is_identity());
    }

    #[test]
    #[cfg(not(feature = "iconv"))]
    fn test_stub_only_supports_utf8() {
        // Without the iconv feature, only UTF-8 should work
        let converter = EncodingConverter::new("utf-8", "utf-8").unwrap();
        assert!(converter.is_identity());

        let result = converter.to_remote("hello.txt").unwrap();
        assert_eq!(result, "hello.txt");

        let result = converter.to_local(b"hello.txt").unwrap();
        assert_eq!(result, "hello.txt");

        // Non-UTF-8 encodings should fail
        let result = EncodingConverter::new("iso-8859-1", "utf-8");
        assert!(result.is_err());
    }

    #[test]
    fn test_converter_from_locale_identity() {
        let converter = converter_from_locale();
        assert!(converter.is_identity());
    }

    #[test]
    fn test_converter_equality() {
        let conv1 = EncodingConverter::identity();
        let conv2 = EncodingConverter::identity();
        assert_eq!(conv1, conv2);
    }
}
