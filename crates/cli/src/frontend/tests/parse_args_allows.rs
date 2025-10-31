use super::common::*;
use super::*;

#[test]
fn parse_args_allows_no_partial_to_clear_partial_dir() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--partial-dir=.rsync-partial"),
        OsString::from("--no-partial"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.partial);
    assert!(parsed.partial_dir.is_none());
}
