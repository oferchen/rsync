use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_sparse_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--sparse"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.sparse, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-sparse"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.sparse, Some(false));
}
