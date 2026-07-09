use super::common::*;
use super::*;

use compress::algorithm::CompressionAlgorithm;

#[test]
fn parse_compress_level_argument_rejects_non_numeric_value() {
    let error =
        parse_compress_level_argument(OsStr::new("fast"), CompressionAlgorithm::Zlib).unwrap_err();
    let rendered = error.to_string();
    assert!(rendered.contains("invalid compression level"));
    assert!(rendered.contains("integer"));
}

#[test]
fn parse_compress_level_argument_clamps_out_of_range_value() {
    // upstream: token.c:init_compression_level() clamps rather than rejecting.
    let setting =
        parse_compress_level_argument(OsStr::new("12"), CompressionAlgorithm::Zlib).unwrap();
    assert!(
        setting.is_enabled(),
        "level 12 should clamp to a valid level"
    );
}
