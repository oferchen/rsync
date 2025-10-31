use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_mkpath_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--mkpath"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.mkpath);
}
