use super::common::*;
use super::*;

#[test]
fn parse_compress_level_argument_rejects_invalid_value() {
    let error = parse_compress_level_argument(OsStr::new("fast")).unwrap_err();
    let rendered = error.to_string();
    assert!(rendered.contains("invalid compression level"));
    assert!(rendered.contains("integer"));

    let range_error = parse_compress_level_argument(OsStr::new("12")).unwrap_err();
    let rendered_range = range_error.to_string();
    assert!(rendered_range.contains("outside the supported range"));
}
