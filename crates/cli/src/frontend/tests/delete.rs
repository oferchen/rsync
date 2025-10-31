use super::common::*;
use super::*;

#[test]
fn delete_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::During);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_alias_del_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--del"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::During);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_after_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-after"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::After);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_before_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-before"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::Before);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_during_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-during"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::During);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_delay_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-delay"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::Delay);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_excluded_flag_implies_delete() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-excluded"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.delete_mode.is_enabled());
    assert!(parsed.delete_excluded);
}
