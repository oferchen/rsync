use super::common::*;
use super::*;

#[test]
fn parse_args_records_compress_level_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("5")));
}
