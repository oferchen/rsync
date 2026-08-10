//! Error types for encoding operations.

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
