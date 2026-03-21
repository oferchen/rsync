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
//! ```no_run
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

mod converter;
mod error;
mod pair;

pub use converter::{EncodingConverter, FilenameConverter, converter_from_locale};
pub use error::{ConversionError, EncodingError};
pub use pair::EncodingPair;

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
        assert_eq!(
            err.to_string(),
            "conversion failed from utf-8 to iso-8859-1"
        );

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
