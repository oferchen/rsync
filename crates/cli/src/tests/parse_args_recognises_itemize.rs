use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_itemize_changes_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--itemize-changes"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.itemize_changes);
}
