use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_blocking_io_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--blocking-io"),
        OsString::from("src"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.blocking_io, Some(true));
}

#[test]
fn parse_args_recognises_no_blocking_io_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-blocking-io"),
        OsString::from("src"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.blocking_io, Some(false));
}
