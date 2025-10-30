use super::common::*;
use super::*;

#[test]
fn parse_args_sets_protect_args_flag() {
    let parsed =
        parse_args([OsString::from(RSYNC), OsString::from("--protect-args")]).expect("parse");

    assert_eq!(parsed.protect_args, Some(true));
}

#[test]
fn parse_args_sets_protect_args_alias() {
    let parsed =
        parse_args([OsString::from(RSYNC), OsString::from("--secluded-args")]).expect("parse");

    assert_eq!(parsed.protect_args, Some(true));
}

#[test]
fn parse_args_sets_no_protect_args_flag() {
    let parsed =
        parse_args([OsString::from(RSYNC), OsString::from("--no-protect-args")]).expect("parse");

    assert_eq!(parsed.protect_args, Some(false));
}

#[test]
fn parse_args_sets_no_protect_args_alias() {
    let parsed =
        parse_args([OsString::from(RSYNC), OsString::from("--no-secluded-args")]).expect("parse");

    assert_eq!(parsed.protect_args, Some(false));
}

#[test]
fn parse_args_sets_ipv4_address_mode() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--ipv4"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.address_mode, AddressMode::Ipv4);
}

#[test]
fn parse_args_sets_ipv6_address_mode() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--ipv6"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.address_mode, AddressMode::Ipv6);
}

#[test]
fn parse_args_sets_from0_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--from0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.from0);
}

#[test]
fn parse_args_sets_compress_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.compress);
}
