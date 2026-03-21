//! Filename encoding converter between local and remote character sets.

use std::borrow::Cow;

use super::error::{ConversionError, EncodingError};

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
pub(super) fn normalize_encoding_name(name: &str) -> Cow<'_, str> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Cow::Borrowed("utf-8");
    }
    Cow::Borrowed(trimmed)
}

/// Checks if an encoding name refers to UTF-8.
#[cfg(not(feature = "iconv"))]
pub(super) fn is_utf8_name(name: &str) -> bool {
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
