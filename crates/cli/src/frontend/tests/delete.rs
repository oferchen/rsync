use super::common::*;
use super::*;

#[test]
fn delete_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete"),
        OsString::from("--dirs"), // Mirror upstream: --delete requires --recursive or --dirs
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
        OsString::from("--dirs"), // Mirror upstream: --delete requires --recursive or --dirs
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
        OsString::from("--dirs"), // Mirror upstream: --delete requires --recursive or --dirs
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
        OsString::from("--dirs"), // Mirror upstream: --delete requires --recursive or --dirs
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
        OsString::from("--dirs"), // Mirror upstream: --delete requires --recursive or --dirs
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
        OsString::from("--dirs"), // Mirror upstream: --delete requires --recursive or --dirs
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::Delay);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_missing_args_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-missing-args"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.delete_missing_args);
    assert!(parsed.ignore_missing_args);
}

#[test]
fn delete_excluded_flag_implies_delete() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-excluded"),
        OsString::from("--dirs"), // Mirror upstream: --delete requires --recursive or --dirs
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.delete_mode.is_enabled());
    assert!(parsed.delete_excluded);
}
