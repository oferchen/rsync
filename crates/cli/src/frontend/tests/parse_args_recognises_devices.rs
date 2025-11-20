use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_devices_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--devices"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.devices, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-devices"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.devices, Some(false));
}

#[test]
fn parse_args_recognises_copy_devices_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--copy-devices"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.copy_devices);
}

#[test]
fn parse_args_recognises_write_devices_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--write-devices"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.write_devices, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-write-devices"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.write_devices, Some(false));
}

#[test]
fn parse_args_recognises_no_d_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-D"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.devices, Some(false));
    assert_eq!(parsed.specials, Some(false));
}
