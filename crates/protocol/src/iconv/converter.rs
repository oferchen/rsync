//! Filename encoding converter between local and remote character sets.
//!
//! Provides both strict and lossy conversion modes:
//!
//! - **Strict** (`remote_to_local`, `local_to_remote`): returns `Err` on
//!   unconvertible bytes. Used by callers that need to handle failures
//!   explicitly (e.g., flist reader/writer which skip the entry and set
//!   `io_error`).
//!
//! - **Lossy** (`remote_to_local_lossy`, `local_to_remote_lossy`): mirrors
//!   upstream `iconvbufs()` with `ICB_INCLUDE_BAD`, copying unconvertible
//!   bytes to the output verbatim and returning a [`ConversionOutcome`]
//!   indicating whether any verbatim pass-through occurred. Used by callers
//!   that must not corrupt bad bytes (e.g., `read_line(RL_CONVERT)`,
//!   `send_protected_args`).
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
/// Contains the converted bytes and metadata about whether any bytes were
/// passed through verbatim during conversion. This mirrors upstream rsync's
/// `iconvbufs()` with `ICB_INCLUDE_BAD` set, where unconvertible bytes are
/// copied to the output verbatim rather than causing an error.
#[derive(Debug, Clone)]
pub struct ConversionOutcome<'a> {
    /// The converted bytes. When `had_replacements` is true, some source
    /// bytes were copied through verbatim because they could not be converted.
    pub output: Cow<'a, [u8]>,
    /// Whether any source bytes were passed through verbatim (i.e. any byte
    /// was unconvertible under `ICB_INCLUDE_BAD`).
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

    /// Converts a filename from remote encoding to local encoding, copying
    /// unconvertible bytes through verbatim rather than failing.
    ///
    /// Bytes that are invalid in the remote encoding, and remote characters
    /// that the local encoding cannot represent, are copied to the output
    /// verbatim (byte-for-byte). The returned [`ConversionOutcome`] indicates
    /// whether any verbatim pass-through occurred.
    ///
    /// This mirrors upstream `iconvbufs()` with `ICB_INCLUDE_BAD` set: on an
    /// unconvertible byte it runs `*obuf++ = *ibuf++` (rsync.c:261), copying
    /// the offending source byte straight to the output rather than
    /// substituting it.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1286` - `read_line(RL_CONVERT)` uses `ICB_INCLUDE_BAD`
    /// - `rsync.c:305` - `send_protected_args` uses `ICB_INCLUDE_BAD`
    #[cfg(feature = "iconv")]
    pub fn remote_to_local_lossy<'a>(&self, bytes: &'a [u8]) -> ConversionOutcome<'a> {
        if self.is_identity() {
            return ConversionOutcome {
                output: Cow::Borrowed(bytes),
                had_replacements: false,
            };
        }

        let (output, had_bad) =
            convert_include_bad(bytes, self.remote_encoding, self.local_encoding);

        ConversionOutcome {
            output: Cow::Owned(output),
            had_replacements: had_bad,
        }
    }

    /// Converts a filename from local encoding to remote encoding, copying
    /// unconvertible bytes through verbatim rather than failing.
    ///
    /// Bytes that are invalid in the local encoding, and local characters that
    /// the remote encoding cannot represent, are copied to the output verbatim
    /// (byte-for-byte). The returned [`ConversionOutcome`] indicates whether
    /// any verbatim pass-through occurred.
    ///
    /// This mirrors upstream `iconvbufs()` with `ICB_INCLUDE_BAD` set
    /// (rsync.c:261 `*obuf++ = *ibuf++`).
    #[cfg(feature = "iconv")]
    pub fn local_to_remote_lossy<'a>(&self, bytes: &'a [u8]) -> ConversionOutcome<'a> {
        if self.is_identity() {
            return ConversionOutcome {
                output: Cow::Borrowed(bytes),
                had_replacements: false,
            };
        }

        let (output, had_bad) =
            convert_include_bad(bytes, self.local_encoding, self.remote_encoding);

        ConversionOutcome {
            output: Cow::Owned(output),
            had_replacements: had_bad,
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

/// Converts `input` from `src` to `tgt` using upstream's `ICB_INCLUDE_BAD`
/// semantics: bytes that cannot be converted are copied to the output
/// verbatim rather than substituted.
///
/// Upstream `iconvbufs()` (rsync.c:179) runs a single `iconv()` from the
/// source charset to the target charset. When `iconv()` reports `EILSEQ` (an
/// invalid input byte, or an input character that the target charset cannot
/// represent) and `ICB_INCLUDE_BAD` is set, it executes `*obuf++ = *ibuf++`
/// (rsync.c:261): it copies the offending source byte straight into the
/// output and advances one byte, then resumes conversion. Trailing incomplete
/// multibyte sequences are likewise passed through under
/// `ICB_INCLUDE_INCOMPLETE`, which both lossy callers set (io.c:1286
/// `read_line(RL_CONVERT)`, rsync.c:305 `send_protected_args`).
///
/// Because `encoding_rs` converts through a Unicode intermediate rather than a
/// direct charset-to-charset table, this walks the input one source byte at a
/// time so that each decoded scalar retains its exact source byte span. An
/// invalid source byte, and a scalar the target charset cannot encode, are
/// both emitted as their verbatim source bytes - matching upstream's
/// `*obuf++ = *ibuf++` (each byte of an unmappable multibyte scalar is itself
/// an invalid standalone lead, so upstream copies them all verbatim in turn).
///
/// Returns the converted bytes and whether any verbatim pass-through occurred.
#[cfg(feature = "iconv")]
fn convert_include_bad(
    input: &[u8],
    src: &'static encoding_rs::Encoding,
    tgt: &'static encoding_rs::Encoding,
) -> (Vec<u8>, bool) {
    let mut decoder = src.new_decoder_without_bom_handling();
    let mut encoder = tgt.new_encoder();
    let mut out = Vec::with_capacity(input.len());
    let mut had_bad = false;
    // A single fed byte completes at most one scalar (<= 4 UTF-8 bytes); 16
    // bytes leave ample headroom so `OutputFull` cannot occur.
    let mut scalar = [0u8; 16];
    // Source offset where the in-progress (not-yet-emitted) sequence began.
    let mut seq_start = 0usize;
    let mut pos = 0usize;

    while pos < input.len() {
        let last = pos + 1 == input.len();
        let (result, read, written) =
            decoder.decode_to_utf8_without_replacement(&input[pos..=pos], &mut scalar, last);
        pos += read;

        match result {
            encoding_rs::DecoderResult::InputEmpty => {
                if written == 0 {
                    // Byte buffered as part of an incomplete multibyte scalar.
                    continue;
                }
                let span = &input[seq_start..pos];
                match std::str::from_utf8(&scalar[..written]) {
                    Ok(text) if encode_scalar(text, &mut encoder, tgt, &mut out) => {
                        // Target charset cannot represent the scalar: copy its
                        // source bytes verbatim (rsync.c:261).
                        out.extend_from_slice(span);
                        had_bad = true;
                    }
                    Ok(_) => {}
                    // The decoder only ever emits valid UTF-8; this arm is
                    // unreachable but degrades to verbatim pass-through.
                    Err(_) => {
                        out.extend_from_slice(span);
                        had_bad = true;
                    }
                }
                seq_start = pos;
            }
            encoding_rs::DecoderResult::Malformed(..) => {
                // The malformed run is exactly the in-progress sequence: bytes
                // consumed since the last emitted scalar. Feeding one byte at a
                // time means any following byte is never consumed into this
                // result, so `input[seq_start..pos]` is precisely the bad run.
                // Copy it verbatim (rsync.c:261).
                out.extend_from_slice(&input[seq_start..pos]);
                had_bad = true;
                seq_start = pos;
            }
            encoding_rs::DecoderResult::OutputFull => {
                // Unreachable: one source byte completes at most one scalar,
                // which always fits in `scalar`. Guard against a stall.
                break;
            }
        }
    }

    // Flush any trailing encoder state (e.g. an ISO-2022-JP shift back to
    // ASCII) so the output is a complete byte sequence.
    flush_encoder(&mut encoder, tgt, &mut out);

    (out, had_bad)
}

/// Encodes a single decoded scalar run into the target charset via `encoder`,
/// appending the bytes to `out`.
///
/// Returns `true` when the target charset cannot represent the scalar, in
/// which case nothing is appended and the caller copies the verbatim source
/// bytes (mirroring upstream's `*obuf++ = *ibuf++`, rsync.c:261).
#[cfg(feature = "iconv")]
fn encode_scalar(
    text: &str,
    encoder: &mut encoding_rs::Encoder,
    tgt: &'static encoding_rs::Encoding,
    out: &mut Vec<u8>,
) -> bool {
    // Fast path: a UTF-8 target needs no re-encoding of already-UTF-8 scalars.
    if tgt == encoding_rs::UTF_8 {
        out.extend_from_slice(text.as_bytes());
        return false;
    }

    // 16 bytes hold any single scalar's encoding, so one call suffices.
    let mut buf = [0u8; 16];
    let (result, _read, written) =
        encoder.encode_from_utf8_without_replacement(text, &mut buf, false);
    match result {
        encoding_rs::EncoderResult::Unmappable(_) => true,
        encoding_rs::EncoderResult::InputEmpty | encoding_rs::EncoderResult::OutputFull => {
            out.extend_from_slice(&buf[..written]);
            false
        }
    }
}

/// Flushes any pending encoder state into `out` (e.g. an ISO-2022-JP escape
/// sequence resetting the charset back to ASCII at end of input).
#[cfg(feature = "iconv")]
fn flush_encoder(
    encoder: &mut encoding_rs::Encoder,
    tgt: &'static encoding_rs::Encoding,
    out: &mut Vec<u8>,
) {
    // The UTF-8 fast path in `encode_scalar` never advances encoder state.
    if tgt == encoding_rs::UTF_8 {
        return;
    }
    let mut buf = [0u8; 16];
    let (_result, _read, written) =
        encoder.encode_from_utf8_without_replacement("", &mut buf, true);
    out.extend_from_slice(&buf[..written]);
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
