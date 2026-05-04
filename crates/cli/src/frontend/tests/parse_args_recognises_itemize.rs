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

#[test]
fn parse_args_recognises_no_itemize_changes_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-itemize-changes"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.itemize_changes);
    assert!(parsed.name_overridden);
}

#[test]
fn parse_args_prefers_last_itemize_toggle() {
    let disabled = parse_args([
        OsString::from(RSYNC),
        OsString::from("--itemize-changes"),
        OsString::from("--no-itemize-changes"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!disabled.itemize_changes);
    assert!(disabled.name_overridden);

    let enabled = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-itemize-changes"),
        OsString::from("--itemize-changes"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(enabled.itemize_changes);
    assert!(enabled.name_overridden);
}
