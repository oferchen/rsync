use super::common::*;
use super::*;

#[test]
fn parse_args_no_bwlimit_overrides_bwlimit_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--bwlimit=2M"),
        OsString::from("--no-bwlimit"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(matches!(parsed.bwlimit, Some(BandwidthArgument::Disabled)));
}

#[test]
fn parse_args_no_compress_overrides_compress_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        OsString::from("--no-compress"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.compress);
    assert!(parsed.no_compress);
}
