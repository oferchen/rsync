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
    let parsed = parse_block_size_argument(OsStr::new("4096")).expect("block-size parses");
    assert_eq!(parsed.get(), 4096);
}

#[test]
fn parse_block_size_argument_rejects_zero() {
    let error = parse_block_size_argument(OsStr::new("0")).expect_err("zero rejected");
    assert!(error.to_string().contains("size must be positive"));
}

#[test]
fn parse_block_size_argument_rejects_large_value() {
    let error = parse_block_size_argument(OsStr::new("5000000000")).expect_err("overflow rejected");
    assert!(
        error
            .to_string()
            .contains("size exceeds the supported 32-bit range")
    );
}
