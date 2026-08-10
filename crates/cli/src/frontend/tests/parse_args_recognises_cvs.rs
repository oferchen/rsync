use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_cvs_exclude_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--cvs-exclude"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.cvs_exclude);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-C"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.cvs_exclude);
}
