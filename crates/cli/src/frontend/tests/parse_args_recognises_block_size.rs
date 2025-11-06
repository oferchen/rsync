use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_block_size_argument() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--block-size=16384"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.block_size, Some(OsString::from("16384")));
}
