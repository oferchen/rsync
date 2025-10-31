use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_safe_links_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--safe-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.safe_links);
}
