use super::common::*;
use super::*;

#[test]
fn parse_checksum_seed_argument_accepts_zero() {
    let seed = parse_checksum_seed_argument(OsStr::new("0")).expect("parse checksum seed");
    assert_eq!(seed, 0);
}

#[test]
fn parse_checksum_seed_argument_accepts_max_i32() {
    let seed = parse_checksum_seed_argument(OsStr::new("2147483647")).expect("parse checksum seed");
    assert_eq!(seed, i32::MAX);
}

/// upstream: options.c:861 uses `POPT_ARG_INT`, which popt bounds to
/// `INT_MIN..=INT_MAX` (popt/popt.c poptSaveArg returns `POPT_ERROR_OVERFLOW`
/// otherwise). Accepting a larger value would have us forward
/// `--checksum-seed=4294967295` (options.c:3047) to a peer that answers
/// "number too large or too small" and exits 1.
#[test]
fn parse_checksum_seed_argument_rejects_above_i32_max() {
    let error = parse_checksum_seed_argument(OsStr::new("4294967295"))
        .expect_err("a value above i32::MAX should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("invalid --checksum-seed value"),
        "diagnostic missing invalid message: {rendered}"
    );
}

/// upstream: options.c:151 declares `int checksum_seed`, so `-1` is a legal
/// seed. Refusing it makes oc-rsync unable to express a value upstream accepts,
/// and - on the `--server` side - unable to serve an upstream client that used
/// it.
#[test]
fn parse_checksum_seed_argument_accepts_negative() {
    let seed = parse_checksum_seed_argument(OsStr::new("-1")).expect("negative seed should parse");
    assert_eq!(seed, -1);
    let min = parse_checksum_seed_argument(OsStr::new("-2147483648")).expect("i32::MIN parses");
    assert_eq!(min, i32::MIN);
}

#[test]
fn parse_checksum_seed_argument_rejects_below_i32_min() {
    let error = parse_checksum_seed_argument(OsStr::new("-2147483649"))
        .expect_err("a value below i32::MIN should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("invalid --checksum-seed value"),
        "diagnostic missing invalid message: {rendered}"
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

#[test]
fn parse_checksum_seed_argument_rejects_overflow() {
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
