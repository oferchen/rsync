//! Filename encoding converter between local and remote character sets.
//!
//! Provides both strict and lossy conversion modes:
//!
//! - **Strict** (`remote_to_local`, `local_to_remote`): returns `Err` on
//!   unconvertible bytes. Used by callers that need to handle failures
//!   explicitly (e.g., flist reader/writer which skip the entry and set
//!   `io_error`).
//!
//! - **Lossy** (`remote_to_local_lossy`, `local_to_remote_lossy`): replaces
//!   unconvertible bytes with a replacement character and returns a
//!   [`ConversionOutcome`] indicating whether any replacements occurred.
//!   Used by callers that pass bad bytes through verbatim (e.g.,
//!   `send_protected_args`, `--files-from` exchange).
//!
//! # Upstream Reference
//!
//! - `rsync.c:179` `iconvbufs()` - central conversion with `ICB_INCLUDE_BAD`
//!   flag controlling whether invalid bytes are passed through or cause errors.
//! - `flist.c:745` - recv uses `ICB_INIT` only (strict).
//! - `io.c:1283` - `read_line(RL_CONVERT)` uses `ICB_INCLUDE_BAD` (lossy).
//! - `rsync.c:305` - `send_protected_args` uses `ICB_INCLUDE_BAD` (lossy).

use std::borrow::Cow;

use super::error::{ConversionError, EncodingError};

/// Character encoding converter for filename conversion.
///
/// This is the new API that provides string-based conversions with [`EncodingError`].
/// For the legacy byte-oriented API, see [`FilenameConverter`].
///
/// When the `iconv` feature is disabled, only UTF-8 is supported.
pub type EncodingConverter = FilenameConverter;

/// Result of a lossy encoding conversion.
///
/// Contains the converted bytes and metadata about whether any bytes
/// were replaced during conversion. This mirrors upstream rsync's
/// `iconvbufs()` with `ICB_INCLUDE_BAD` set, where invalid bytes are
/// included verbatim rather than causing an error.
#[derive(Debug, Clone)]
pub struct ConversionOutcome<'a> {
    /// The converted bytes. When `had_replacements` is true, some bytes
    /// were replaced with a substitution character.
    pub output: Cow<'a, [u8]>,
    /// Whether any bytes were replaced during conversion.
    pub had_replacements: bool,
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

    /// Converts a filename from remote encoding to local encoding, replacing
    /// unconvertible bytes rather than failing.
    ///
    /// Invalid sequences in the remote encoding are decoded as U+FFFD
    /// (replacement character). Characters that cannot be represented in the
    /// local encoding are replaced with `?`. The returned
    /// [`ConversionOutcome`] indicates whether any replacements occurred.
    ///
    /// This mirrors upstream `iconvbufs()` with `ICB_INCLUDE_BAD` set
    /// (rsync.c:229-231), where invalid bytes are passed through verbatim
    /// rather than causing `EILSEQ`.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1283` - `read_line(RL_CONVERT)` uses `ICB_INCLUDE_BAD`
    /// - `rsync.c:305` - `send_protected_args` uses `ICB_INCLUDE_BAD`
    #[cfg(feature = "iconv")]
    pub fn remote_to_local_lossy<'a>(&self, bytes: &'a [u8]) -> ConversionOutcome<'a> {
        if self.is_identity() {
            return ConversionOutcome {
                output: Cow::Borrowed(bytes),
                had_replacements: false,
            };
        }

        // Decode from remote encoding to UTF-8. encoding_rs replaces
        // invalid sequences with U+FFFD and reports via had_errors.
        let (decoded, _, decode_had_errors) = self.remote_encoding.decode(bytes);

        // Encode from UTF-8 to local encoding with replacement.
        let (encoded, had_encode_errors) = encode_with_replacement(&decoded, self.local_encoding);

        ConversionOutcome {
            output: Cow::Owned(encoded),
            had_replacements: decode_had_errors || had_encode_errors,
        }
    }

    /// Converts a filename from local encoding to remote encoding, replacing
    /// unconvertible bytes rather than failing.
    ///
    /// Invalid sequences in the local encoding are decoded as U+FFFD
    /// (replacement character). Characters that cannot be represented in the
    /// remote encoding are replaced with `?`. The returned
    /// [`ConversionOutcome`] indicates whether any replacements occurred.
    ///
    /// This mirrors upstream `iconvbufs()` with `ICB_INCLUDE_BAD` set
    /// (rsync.c:229-231).
    #[cfg(feature = "iconv")]
    pub fn local_to_remote_lossy<'a>(&self, bytes: &'a [u8]) -> ConversionOutcome<'a> {
        if self.is_identity() {
            return ConversionOutcome {
                output: Cow::Borrowed(bytes),
                had_replacements: false,
            };
        }

        // Decode from local encoding to UTF-8.
        let (decoded, _, decode_had_errors) = self.local_encoding.decode(bytes);

        // Encode from UTF-8 to remote encoding with replacement.
        let (encoded, had_encode_errors) = encode_with_replacement(&decoded, self.remote_encoding);

        ConversionOutcome {
            output: Cow::Owned(encoded),
            had_replacements: decode_had_errors || had_encode_errors,
        }
    }

    /// No-op lossy conversion when iconv feature is disabled.
    #[cfg(not(feature = "iconv"))]
    pub fn remote_to_local_lossy<'a>(&self, bytes: &'a [u8]) -> ConversionOutcome<'a> {
        ConversionOutcome {
            output: Cow::Borrowed(bytes),
            had_replacements: false,
        }
    }

    /// No-op lossy conversion when iconv feature is disabled.
    #[cfg(not(feature = "iconv"))]
    pub fn local_to_remote_lossy<'a>(&self, bytes: &'a [u8]) -> ConversionOutcome<'a> {
        ConversionOutcome {
            output: Cow::Borrowed(bytes),
            had_replacements: false,
        }
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

/// Encodes a UTF-8 string into the target encoding, replacing unmappable
/// characters with `?` instead of using `encoding_rs`'s default HTML numeric
/// character reference substitution.
///
/// Returns the encoded bytes and a flag indicating whether any replacements
/// were made. The `?` replacement matches upstream rsync's single-byte
/// pass-through behaviour for `ICB_INCLUDE_BAD` in single-byte encodings.
///
/// # Upstream Reference
///
/// - `rsync.c:261` - `*obuf++ = *ibuf++` copies the invalid byte verbatim.
///   Since we go through a Unicode intermediate representation, verbatim
///   copy is not possible; `?` is the closest portable substitute.
#[cfg(feature = "iconv")]
fn encode_with_replacement(utf8: &str, encoding: &'static encoding_rs::Encoding) -> (Vec<u8>, bool) {
    // Fast path: UTF-8 target encoding never has unmappable characters.
    if encoding == encoding_rs::UTF_8 {
        return (utf8.as_bytes().to_vec(), false);
    }

    let mut encoder = encoding.new_encoder();
    let mut output = Vec::with_capacity(utf8.len());
    let mut had_replacements = false;
    let mut remaining = utf8;

    // Process all input, replacing unmappable characters with '?'.
    while !remaining.is_empty() {
        let needed = encoder
            .max_buffer_length_from_utf8_without_replacement(remaining.len())
            .unwrap_or(remaining.len() * 4);
        let start = output.len();
        output.resize(start + needed, 0);

        let (result, consumed, written) = encoder.encode_from_utf8_without_replacement(
            remaining,
            &mut output[start..],
            false,
        );
        output.truncate(start + written);
        remaining = &remaining[consumed..];

        match result {
            encoding_rs::EncoderResult::InputEmpty => break,
            encoding_rs::EncoderResult::OutputFull => {
                // Need more output space, loop will reallocate.
            }
            encoding_rs::EncoderResult::Unmappable(ch) => {
                had_replacements = true;
                output.push(b'?');
                remaining = &remaining[ch.len_utf8()..];
            }
        }
    }

    // Flush any pending state in stateful encodings (e.g., ISO-2022-JP).
    let flush_needed = encoder
        .max_buffer_length_from_utf8_without_replacement(0)
        .unwrap_or(16);
    let start = output.len();
    output.resize(start + flush_needed, 0);
    let (_result, _consumed, written) =
        encoder.encode_from_utf8_without_replacement("", &mut output[start..], true);
    output.truncate(start + written);

    (output, had_replacements)
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
