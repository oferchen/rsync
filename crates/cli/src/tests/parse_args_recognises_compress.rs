use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_compress_level_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.compress);
    assert_eq!(parsed.compress_level, Some(OsString::from("5")));
}
