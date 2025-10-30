use super::common::*;
use super::*;

#[test]
fn parse_args_captures_skip_compress_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--skip-compress=gz/mp3"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.skip_compress, Some(OsString::from("gz/mp3")));
}
