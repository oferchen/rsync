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
