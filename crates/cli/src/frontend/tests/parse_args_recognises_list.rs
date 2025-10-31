use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_list_only_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.list_only);
    assert!(parsed.dry_run);
}
