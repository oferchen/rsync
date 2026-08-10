use super::common::*;
use super::*;

#[test]
fn bwlimit_invalid_value_reports_error() {
    let (code, stdout, stderr) =
        run_with_args([OsString::from(RSYNC), OsString::from("--bwlimit=oops")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("--bwlimit=oops is invalid"));
    assert_contains_client_trailer(&rendered);
}

#[test]
fn bwlimit_rejects_small_fractional_values() {
    let (code, stdout, stderr) =
        run_with_args([OsString::from(RSYNC), OsString::from("--bwlimit=0.4")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("--bwlimit=0.4 is too small (min: 512 or 0 for unlimited)",));
}

#[test]
fn bwlimit_accepts_decimal_suffixes() {
    let limit = parse_bandwidth_limit(OsStr::new("1.5M"))
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.bytes_per_second().get(), 1_572_864);
}

#[test]
fn bwlimit_accepts_decimal_base_specifier() {
    let limit = parse_bandwidth_limit(OsStr::new("10KB"))
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.bytes_per_second().get(), 10_000);
}

#[test]
fn bwlimit_accepts_burst_component() {
    let limit = parse_bandwidth_limit(OsStr::new("4M:32K"))
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.bytes_per_second().get(), 4_194_304);
    assert_eq!(
        limit.burst_bytes().map(std::num::NonZeroU64::get),
        Some(32 * 1024)
    );
}

#[test]
fn bwlimit_zero_disables_limit() {
    let limit = parse_bandwidth_limit(OsStr::new("0")).expect("parse succeeds");
    assert!(limit.is_none());
}

#[test]
fn bwlimit_rejects_whitespace_wrapped_argument() {
    let error = parse_bandwidth_limit(OsStr::new(" 1M \t"))
        .expect_err("whitespace-wrapped bwlimit should fail");
    let rendered = format!("{error}");
    assert!(rendered.contains("--bwlimit= 1M \t is invalid"));
}

#[test]
fn bwlimit_accepts_leading_plus_sign() {
    let limit = parse_bandwidth_limit(OsStr::new("+2M"))
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.bytes_per_second().get(), 2_097_152);
}

#[test]
fn bwlimit_rejects_negative_values() {
    let (code, stdout, stderr) =
        run_with_args([OsString::from(RSYNC), OsString::from("--bwlimit=-1")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("--bwlimit=-1 is invalid"));
}
