use super::common::*;
use super::*;

#[test]
fn parse_max_delete_argument_accepts_zero() {
    let limit = parse_max_delete_argument(OsStr::new("0")).expect("parse max-delete");
    assert_eq!(limit, 0);
}

#[test]
fn parse_max_delete_argument_rejects_negative() {
    let error =
        parse_max_delete_argument(OsStr::new("-4")).expect_err("negative limit should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("deletion limit must be non-negative"),
        "diagnostic missing detail: {rendered}"
    );
}

#[test]
fn parse_max_delete_argument_rejects_non_numeric() {
    let error =
        parse_max_delete_argument(OsStr::new("abc")).expect_err("non-numeric limit should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("deletion limit must be an unsigned integer"),
        "diagnostic missing unsigned message: {rendered}"
    );
}
