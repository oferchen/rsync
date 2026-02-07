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

// ===== Additional edge case tests (task #99) =====

#[test]
fn parse_checksum_seed_argument_rejects_overflow_u32() {
    // u32::MAX + 1 = 4294967296 should fail
    let error =
        parse_checksum_seed_argument(OsStr::new("4294967296")).expect_err("overflow should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("invalid --checksum-seed value"),
        "diagnostic should mention invalid value: {rendered}"
    );
}

#[test]
fn parse_checksum_seed_argument_rejects_large_overflow() {
    // Way beyond u32 range
    let error = parse_checksum_seed_argument(OsStr::new("99999999999"))
        .expect_err("large overflow should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("invalid --checksum-seed value"),
        "diagnostic should mention invalid value: {rendered}"
    );
}

#[test]
fn parse_checksum_seed_argument_accepts_one() {
    let seed = parse_checksum_seed_argument(OsStr::new("1")).expect("parse checksum seed");
    assert_eq!(seed, 1);
}

#[test]
fn parse_checksum_seed_argument_accepts_typical_value() {
    let seed = parse_checksum_seed_argument(OsStr::new("12345")).expect("parse checksum seed");
    assert_eq!(seed, 12345);
}

#[test]
fn parse_checksum_seed_argument_handles_whitespace() {
    let seed =
        parse_checksum_seed_argument(OsStr::new("  42  ")).expect("whitespace should be trimmed");
    assert_eq!(seed, 42);
}

#[test]
fn parse_checksum_seed_argument_rejects_empty() {
    let error = parse_checksum_seed_argument(OsStr::new("")).expect_err("empty should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("must not be empty"),
        "diagnostic should mention empty: {rendered}"
    );
}

#[test]
fn parse_checksum_seed_argument_accepts_with_plus_prefix() {
    // Upstream rsync allows +NUM
    let seed = parse_checksum_seed_argument(OsStr::new("+999")).expect("plus prefix should work");
    assert_eq!(seed, 999);
}

#[test]
fn parse_checksum_seed_argument_rejects_float() {
    let error = parse_checksum_seed_argument(OsStr::new("3.14")).expect_err("float should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("invalid --checksum-seed value"),
        "diagnostic should mention invalid value: {rendered}"
    );
}

#[test]
fn parse_checksum_seed_argument_rejects_hex() {
    let error = parse_checksum_seed_argument(OsStr::new("0xFF")).expect_err("hex should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("invalid --checksum-seed value"),
        "diagnostic should mention invalid value: {rendered}"
    );
}
