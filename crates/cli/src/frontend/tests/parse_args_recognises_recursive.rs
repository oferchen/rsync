use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_recursive_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse recursive");

    assert!(parsed.recursive);
    assert!(!parsed.archive);
}

#[test]
fn parse_args_expands_short_option_clusters() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-arvz"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse cluster");

    assert!(parsed.archive);
    assert!(parsed.recursive);
    assert!(parsed.compress);
    assert_eq!(parsed.verbosity, 1);
    assert_eq!(
        parsed.remainder,
        vec![OsString::from("source"), OsString::from("dest")]
    );
}
