use super::common::*;
use super::*;

#[test]
fn parse_size_limit_argument_accepts_fractional_units() {
    let value =
        parse_size_limit_argument(OsStr::new("1.5K"), "--min-size").expect("parse size limit");
    assert_eq!(value, 1536);
}

#[test]
fn parse_size_limit_argument_rejects_negative() {
    let error =
        parse_size_limit_argument(OsStr::new("-2"), "--min-size").expect_err("negative rejected");
    let rendered = error.to_string();
    assert!(
        rendered.contains("size must be non-negative"),
        "missing detail: {rendered}"
    );
}

#[test]
fn parse_size_limit_argument_rejects_invalid_suffix() {
    let error = parse_size_limit_argument(OsStr::new("10QB"), "--max-size")
        .expect_err("invalid suffix rejected");
    let rendered = error.to_string();
    assert!(
        rendered.contains("expected a size with an optional"),
        "missing message: {rendered}"
    );
}

#[test]
fn parse_block_size_argument_accepts_valid_value() {
    let parsed = parse_block_size_argument(OsStr::new("4096"))
        .expect("block-size parses")
        .expect("non-zero override");
    assert_eq!(parsed.get(), 4096);
}

#[test]
fn parse_block_size_argument_zero_falls_back_to_default() {
    // upstream: options.c:1708-1711 - `--block-size=0` is accepted (min_value 0)
    // and falls back to the default block size, represented here as `None`.
    let parsed = parse_block_size_argument(OsStr::new("0")).expect("zero accepted");
    assert_eq!(parsed, None);
}

#[test]
fn parse_block_size_argument_rejects_large_value() {
    // upstream: options.c:1708-1711 - a value above MAX_BLOCK_SIZE (131072) is
    // "too large (max: 128.00K)".
    let error = parse_block_size_argument(OsStr::new("5000000000")).expect_err("overflow rejected");
    assert!(
        error.to_string().contains("is too large (max: 128.00K)"),
        "got: {error}"
    );
}
