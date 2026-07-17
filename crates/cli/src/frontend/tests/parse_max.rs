use super::common::*;
use super::*;

#[test]
fn parse_max_delete_argument_accepts_zero() {
    let limit = parse_max_delete_argument(OsStr::new("0")).expect("parse max-delete");
    assert_eq!(limit, 0);
}

#[test]
fn parse_max_delete_argument_clamps_negative_to_zero() {
    // upstream: options.c:2182-2185 - a negative `--max-delete` is clamped to a
    // 0 cap ("no deletions") rather than rejected.
    let limit = parse_max_delete_argument(OsStr::new("-4")).expect("negative clamps to zero");
    assert_eq!(limit, 0);
}

#[test]
fn parse_max_delete_argument_rejects_non_numeric() {
    let error =
        parse_max_delete_argument(OsStr::new("abc")).expect_err("non-numeric limit should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("deletion limit must be an integer"),
        "diagnostic missing integer message: {rendered}"
    );
}
