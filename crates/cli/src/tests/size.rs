use super::common::*;
use super::*;

#[test]
fn size_only_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--size-only"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.size_only);
}
