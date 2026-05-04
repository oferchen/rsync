use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_mkpath_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--mkpath"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.mkpath);
}

#[test]
fn parse_args_recognises_no_mkpath_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--mkpath"),
        OsString::from("--no-mkpath"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.mkpath);
}

#[test]
fn parse_args_recognises_old_dirs_alias() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--old-dirs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.mkpath);
}

#[test]
fn parse_args_recognises_old_d_alias() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--old-d"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.mkpath);
}

#[test]
fn parse_args_no_mkpath_allows_later_mkpath_override() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-mkpath"),
        OsString::from("--mkpath"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.mkpath);
}
