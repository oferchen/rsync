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
    // upstream: options.c:2375-2376 - list_only does NOT set dry_run.
    assert!(!parsed.dry_run);
}
