use super::common::*;
use super::*;

#[test]
fn long_archive_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--archive"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.archive);
}
