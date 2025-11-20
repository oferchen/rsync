use std::ffi::OsString;

use crate::frontend::arguments::parse_args;

#[test]
fn parse_args_recognises_executability_flag() {
    let parsed = parse_args([
        OsString::from("oc-rsync"),
        OsString::from("--executability"),
        OsString::from("/tmp/src"),
        OsString::from("/tmp/dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.executability, Some(true));
}

#[test]
fn parse_args_recognises_no_executability_flag() {
    let parsed = parse_args([
        OsString::from("oc-rsync"),
        OsString::from("--no-executability"),
        OsString::from("/tmp/src"),
        OsString::from("/tmp/dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.executability, Some(false));
}
