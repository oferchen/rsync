use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_outbuf_option() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--outbuf=L"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.outbuf, Some(OsString::from("L")));
}
