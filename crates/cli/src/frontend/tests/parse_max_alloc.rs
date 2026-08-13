use super::common::*;
use super::*;

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

#[test]
fn max_alloc_argument_resolution_rejects_zero() {
    // upstream: options.c:2069-2072 - `--max-alloc=0` used to resolve to
    // SIZE_MAX and so disabled the allocation ceiling; 3.5.0 rejects it
    // (CVE-2026-53794).
    use crate::frontend::execution::parse_max_alloc_argument;
    let error = parse_max_alloc_argument(OsStr::new("0")).expect_err("zero rejected");
    let rendered = error.to_string();
    assert!(
        rendered.contains("max-alloc must be greater than zero"),
        "expected the upstream zero-rejection text, got: {rendered}"
    );
}

#[test]
fn max_alloc_argument_resolution_rejects_below_one_mib() {
    // upstream: options.c:2067 - parse_size_arg min value is 1 MiB, so "512K"
    // (below the minimum) is rejected as "too small".
    use crate::frontend::execution::parse_max_alloc_argument;
    let error = parse_max_alloc_argument(OsStr::new("512K")).expect_err("below 1 MiB rejected");
    let rendered = error.to_string();
    assert!(
        rendered.contains("is too small (min: 1.00M or 0 for unlimited)"),
        "expected too-small error, got: {rendered}"
    );
}

#[test]
fn max_alloc_argument_resolution_accepts_typical_values() {
    use crate::frontend::execution::parse_max_alloc_argument;
    assert_eq!(
        parse_max_alloc_argument(OsStr::new("1G")).expect("1G accepted"),
        1024 * 1024 * 1024
    );
    assert_eq!(
        parse_max_alloc_argument(OsStr::new("512M")).expect("512M accepted"),
        512 * 1024 * 1024
    );
    // 1024K == 1 MiB, exactly the upstream minimum.
    assert_eq!(
        parse_max_alloc_argument(OsStr::new("1024K")).expect("1024K accepted"),
        1024 * 1024
    );
}

#[test]
fn max_alloc_argument_resolution_rejects_invalid() {
    use crate::frontend::execution::parse_max_alloc_argument;
    assert!(parse_max_alloc_argument(OsStr::new("garbage")).is_err());
    assert!(parse_max_alloc_argument(OsStr::new("100X")).is_err());
    assert!(parse_max_alloc_argument(OsStr::new("")).is_err());
    assert!(parse_max_alloc_argument(OsStr::new("-1G")).is_err());
}

#[test]
fn max_alloc_argument_resolution_rejects_excessive_value() {
    use crate::frontend::execution::parse_max_alloc_argument;
    let value = format!("{}", u64::MAX);
    let err = parse_max_alloc_argument(OsStr::new(&value)).expect_err("ceiling enforced");
    let rendered = err.to_string();
    assert!(
        rendered.contains("exceeds the supported range"),
        "expected range error, got: {rendered}"
    );
}

#[test]
fn max_alloc_zero_value_is_rejected() {
    // upstream: options.c:2069-2072 - `--max-alloc=0` is refused with
    // "max-alloc must be greater than zero" and `goto cleanup`, which
    // `option_error()` reports before `exit_cleanup(RERR_SYNTAX)` (exit 1).
    // Zero used to select SIZE_MAX and so removed the my_alloc() ceiling
    // (CVE-2026-53794).
    let _guard = clear_rsync_rsh();
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=0"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_eq!(
        code, 1,
        "zero --max-alloc should exit 1, got: {stderr_text}"
    );
    assert!(
        stderr_text.contains("max-alloc must be greater than zero"),
        "expected the upstream zero-rejection text, got: {stderr_text}"
    );
}

#[test]
fn max_alloc_non_zero_value_still_transfers() {
    // Positive control for `max_alloc_zero_value_is_rejected`: without it the
    // rejection test would pass even if every --max-alloc value were refused.
    // A valid cap must leave the run to succeed on its own merits.
    let _guard = clear_rsync_rsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    std::fs::create_dir(&src).expect("create src");
    std::fs::write(src.join("f.txt"), b"payload\n").expect("write source file");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--max-alloc=2G"),
        OsString::from(format!("{}/", src.display())),
        OsString::from(format!("{}/", dst.display())),
    ]);
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_eq!(
        code, 0,
        "valid --max-alloc should succeed, got: {stderr_text}"
    );
    assert_eq!(
        std::fs::read(dst.join("f.txt")).expect("read destination file"),
        b"payload\n"
    );
}

#[test]
fn max_alloc_zero_from_the_environment_is_rejected() {
    // upstream: options.c:2061-2073 - RSYNC_MAX_ALLOC seeds `max_alloc_arg`,
    // so an environment-supplied zero reaches the same rejection as the flag.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let _rsh = clear_rsync_rsh();
    let _env = EnvGuard::set("RSYNC_MAX_ALLOC", OsStr::new("0"));

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_eq!(
        code, 1,
        "RSYNC_MAX_ALLOC=0 should exit 1, got: {stderr_text}"
    );
    assert!(
        stderr_text.contains("max-alloc must be greater than zero"),
        "expected the upstream zero-rejection text, got: {stderr_text}"
    );
}

#[test]
fn max_alloc_below_one_mib_produces_error_exit() {
    // upstream: options.c:1960 - a non-zero `--max-alloc` below 1 MiB is a
    // syntax error (exit 1).
    let _guard = clear_rsync_rsh();
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--max-alloc=512K"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_eq!(code, 1, "below-minimum --max-alloc should exit 1");
    assert!(
        stderr_text.contains("is too small (min: 1.00M or 0 for unlimited)"),
        "error should mention the 1 MiB minimum, got: {stderr_text}"
    );
}

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
