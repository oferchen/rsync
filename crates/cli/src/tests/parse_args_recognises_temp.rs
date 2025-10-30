use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_temp_dir_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--temp-dir=.rsync-tmp"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.temp_dir.as_deref(), Some(Path::new(".rsync-tmp")));
}
