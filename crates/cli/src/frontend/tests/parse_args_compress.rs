use super::common::*;
use super::*;

#[test]
fn parse_args_compress_level_zero_records_disable() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("0")));
}

#[test]
fn parse_args_compress_level_zero_disables_compress() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.compress);
    assert_eq!(parsed.compress_level, Some(OsString::from("0")));
}
