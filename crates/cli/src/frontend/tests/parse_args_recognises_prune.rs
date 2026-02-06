use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_prune_empty_dirs_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--prune-empty-dirs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.prune_empty_dirs, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-prune-empty-dirs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.prune_empty_dirs, Some(false));
}

#[test]
fn parse_args_recognises_prune_empty_dirs_short_m_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-m"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.prune_empty_dirs, Some(true));
}

#[test]
fn parse_args_prune_empty_dirs_short_m_combined_with_other_flags() {
    // -m combined with -r (recursive) and -v (verbose)
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-rmv"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.prune_empty_dirs, Some(true));
    assert!(parsed.recursive, "recursive should be enabled by -r");
}

#[test]
fn parse_args_no_prune_empty_dirs_overrides_short_m() {
    // --no-prune-empty-dirs should override -m
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-m"),
        OsString::from("--no-prune-empty-dirs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.prune_empty_dirs, Some(false));
}

#[test]
fn parse_args_prune_empty_dirs_default_is_none() {
    // When neither -m nor --prune-empty-dirs is specified, the field should be None
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.prune_empty_dirs, None);
}
