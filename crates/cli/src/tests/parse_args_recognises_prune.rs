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
