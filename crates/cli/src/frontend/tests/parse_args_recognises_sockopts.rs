use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_sockopts_option() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--sockopts=SO_SNDBUF=8192"),
        OsString::from("src"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.sockopts, Some(OsString::from("SO_SNDBUF=8192")));
}
