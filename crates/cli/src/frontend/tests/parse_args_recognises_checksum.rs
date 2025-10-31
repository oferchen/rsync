use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_checksum_choice() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--checksum-choice=XXH128"),
    ])
    .expect("parse");

    let expected = StrongChecksumChoice::parse("xxh128").expect("choice");
    assert_eq!(parsed.checksum_choice, Some(expected));
    assert_eq!(parsed.checksum_choice_arg, Some(OsString::from("xxh128")));
}
