use super::common::*;
use super::*;

// ============================================================================
// CLI Parsing: --max-alloc accepts various size formats
// ============================================================================

#[test]
fn parse_max_alloc_bytes() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=1048576"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.max_alloc, Some(OsString::from("1048576")));
}

#[test]
fn parse_max_alloc_kilobytes() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=512K"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.max_alloc, Some(OsString::from("512K")));
}

#[test]
fn parse_max_alloc_megabytes() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=256M"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.max_alloc, Some(OsString::from("256M")));
}

#[test]
fn parse_max_alloc_gigabytes() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=2G"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.max_alloc, Some(OsString::from("2G")));
}

#[test]
fn parse_max_alloc_terabytes() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=1T"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.max_alloc, Some(OsString::from("1T")));
}

#[test]
fn parse_max_alloc_with_space_separator() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc"),
        OsString::from("128M"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.max_alloc, Some(OsString::from("128M")));
}

#[test]
fn parse_max_alloc_lowercase_suffix() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=1g"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.max_alloc, Some(OsString::from("1g")));
}

#[test]
fn parse_max_alloc_default_is_none() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.max_alloc.is_none());
}

#[test]
fn parse_max_alloc_zero() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.max_alloc, Some(OsString::from("0")));
}

#[test]
fn parse_max_alloc_fractional() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=1.5G"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.max_alloc, Some(OsString::from("1.5G")));
}

// ============================================================================
// Size Resolution: --max-alloc values resolve to correct byte counts
// ============================================================================

#[test]
fn max_alloc_size_resolution_bytes() {
    let result =
        parse_size_limit_argument(OsStr::new("1048576"), "--max-alloc").expect("parse succeeds");
    assert_eq!(result, 1_048_576);
}

#[test]
fn max_alloc_size_resolution_kilobytes() {
    let result =
        parse_size_limit_argument(OsStr::new("512K"), "--max-alloc").expect("parse succeeds");
    assert_eq!(result, 512 * 1024);
}

#[test]
fn max_alloc_size_resolution_megabytes() {
    let result =
        parse_size_limit_argument(OsStr::new("256M"), "--max-alloc").expect("parse succeeds");
    assert_eq!(result, 256 * 1024 * 1024);
}

#[test]
fn max_alloc_size_resolution_gigabytes() {
    let result =
        parse_size_limit_argument(OsStr::new("2G"), "--max-alloc").expect("parse succeeds");
    assert_eq!(result, 2 * 1024 * 1024 * 1024);
}

#[test]
fn max_alloc_size_resolution_terabytes() {
    let result =
        parse_size_limit_argument(OsStr::new("1T"), "--max-alloc").expect("parse succeeds");
    assert_eq!(result, 1024u64.pow(4));
}

#[test]
fn max_alloc_size_resolution_fractional() {
    let result =
        parse_size_limit_argument(OsStr::new("1.5G"), "--max-alloc").expect("parse succeeds");
    assert_eq!(result, 1_610_612_736); // 1.5 * 1024^3
}

#[test]
fn max_alloc_size_resolution_zero() {
    let result = parse_size_limit_argument(OsStr::new("0"), "--max-alloc").expect("parse succeeds");
    assert_eq!(result, 0);
}

#[test]
fn max_alloc_size_resolution_decimal_suffix() {
    // KB = 1000 (decimal), K = 1024 (binary)
    let result =
        parse_size_limit_argument(OsStr::new("1KB"), "--max-alloc").expect("parse succeeds");
    assert_eq!(result, 1000);
}

#[test]
fn max_alloc_size_resolution_binary_explicit_suffix() {
    let result =
        parse_size_limit_argument(OsStr::new("1KiB"), "--max-alloc").expect("parse succeeds");
    assert_eq!(result, 1024);
}

// ============================================================================
// Error Handling: invalid --max-alloc values produce clear errors
// ============================================================================

#[test]
fn max_alloc_rejects_negative() {
    let error = parse_size_limit_argument(OsStr::new("-1M"), "--max-alloc")
        .expect_err("negative should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("size must be non-negative"),
        "expected non-negative error, got: {rendered}"
    );
}

#[test]
fn max_alloc_rejects_invalid_suffix() {
    let error = parse_size_limit_argument(OsStr::new("100X"), "--max-alloc")
        .expect_err("invalid suffix should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("expected a size with an optional"),
        "expected suffix error, got: {rendered}"
    );
}

#[test]
fn max_alloc_rejects_empty() {
    let error =
        parse_size_limit_argument(OsStr::new(""), "--max-alloc").expect_err("empty should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("must not be empty"),
        "expected empty error, got: {rendered}"
    );
}

#[test]
fn max_alloc_rejects_non_numeric() {
    let error = parse_size_limit_argument(OsStr::new("abc"), "--max-alloc")
        .expect_err("non-numeric should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("expected a size with an optional"),
        "expected format error, got: {rendered}"
    );
}

// ============================================================================
// Integration: --max-alloc produces correct error on invalid values via run
// ============================================================================

#[test]
fn max_alloc_invalid_value_produces_error_exit() {
    let _guard = clear_rsync_rsh();
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=garbage"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_ne!(code, 0, "should exit with error for invalid --max-alloc");
    assert!(
        stderr_text.contains("--max-alloc"),
        "error should mention --max-alloc, got: {stderr_text}"
    );
}

#[test]
fn max_alloc_negative_value_produces_error_exit() {
    let _guard = clear_rsync_rsh();
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=-512M"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_ne!(code, 0, "should exit with error for negative --max-alloc");
    assert!(
        stderr_text.contains("non-negative"),
        "error should mention non-negative, got: {stderr_text}"
    );
}
