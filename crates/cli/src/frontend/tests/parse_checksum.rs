use super::common::*;
use super::*;

#[test]
fn parse_checksum_seed_argument_accepts_zero() {
    let seed = parse_checksum_seed_argument(OsStr::new("0")).expect("parse checksum seed");
    assert_eq!(seed, 0);
}

#[test]
fn parse_checksum_seed_argument_accepts_max_u32() {
    let seed = parse_checksum_seed_argument(OsStr::new("4294967295")).expect("parse checksum seed");
    assert_eq!(seed, u32::MAX);
}

#[test]
fn parse_checksum_seed_argument_rejects_negative() {
    let error =
        parse_checksum_seed_argument(OsStr::new("-1")).expect_err("negative seed should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("must be non-negative"),
        "diagnostic missing negativity detail: {rendered}"
    );
}

#[test]
fn parse_checksum_seed_argument_rejects_non_numeric() {
    let error =
        parse_checksum_seed_argument(OsStr::new("seed")).expect_err("non-numeric seed should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("invalid --checksum-seed value"),
        "diagnostic missing invalid message: {rendered}"
    );
}
