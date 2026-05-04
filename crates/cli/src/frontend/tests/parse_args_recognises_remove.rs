use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_remove_sent_files_alias() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--remove-sent-files"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.remove_source_files);
}
