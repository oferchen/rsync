use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_modify_window() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.modify_window, Some(OsString::from("5")));
}
