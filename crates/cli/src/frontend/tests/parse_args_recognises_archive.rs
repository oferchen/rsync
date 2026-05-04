use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_archive_devices_combo() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-D"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.devices, Some(true));
    assert_eq!(parsed.specials, Some(true));
}
