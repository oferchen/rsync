use super::common::*;
use super::*;

#[test]
fn parse_modify_window_argument_accepts_positive_values() {
    let value = parse_modify_window_argument(OsStr::new("  42 ")).expect("parse modify-window");
    assert_eq!(value, 42);
}

#[test]
fn parse_modify_window_argument_accepts_negative_values() {
    // WHY: upstream options.c parses `--modify-window` as a signed int; a
    // negative value requests nanosecond-exact comparison (util1.c:1482), so it
    // must be accepted rather than rejected.
    assert_eq!(
        parse_modify_window_argument(OsStr::new("-1")).expect("parse -1"),
        -1
    );
    assert_eq!(
        parse_modify_window_argument(OsStr::new(" -2 ")).expect("parse -2"),
        -2
    );
}

#[test]
fn parse_modify_window_argument_rejects_invalid_values() {
    let error = parse_modify_window_argument(OsStr::new("abc"))
        .expect_err("non-numeric modify-window should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("window must be an integer"),
        "diagnostic missing numeric detail: {rendered}"
    );
}

#[test]
fn parse_modify_window_argument_rejects_empty_values() {
    let error = parse_modify_window_argument(OsStr::new("   "))
        .expect_err("empty modify-window should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("value must not be empty"),
        "diagnostic missing emptiness detail: {rendered}"
    );
}
