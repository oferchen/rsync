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
fn max_alloc_argument_resolution_resolves_zero_to_the_ceiling() {
    // upstream: options.c:2085-2086 - `--max-alloc=0` means the largest limit
    // this build supports, SIZE_MAX/2: bounded, never unlimited. An empty
    // value is 0 to upstream's parser (strtod("") == 0), so it resolves the
    // same way.
    use crate::frontend::execution::parse_max_alloc_argument;
    use ::protocol::max_alloc::SIZE_ARG_MAX;
    for value in ["0", "0K", "0.0", ""] {
        assert_eq!(
            parse_max_alloc_argument(OsStr::new(value)).expect("zero accepted"),
            SIZE_ARG_MAX,
            "--max-alloc={value:?} must resolve to SIZE_ARG_MAX"
        );
    }
}

#[test]
fn max_alloc_argument_resolution_rejects_below_one_mib() {
    // upstream: options.c:2073 - parse_size_arg min value is 1 MiB, so "512K"
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
    assert!(parse_max_alloc_argument(OsStr::new("-1G")).is_err());
}

#[test]
fn max_alloc_argument_resolution_rejects_excessive_value() {
    // upstream: options.c:1221-1227 - with no explicit maximum the ceiling is
    // SIZE_ARG_MAX, and reaching it is "too large". Accepting 0 must not bring
    // back an unbounded value: 8191P is the last P step below SIZE_MAX/2 on a
    // 64-bit build, 8192P reaches it, and a value that overflows u64 while
    // being scaled gets the same verdict.
    use crate::frontend::execution::parse_max_alloc_argument;
    let overflow = format!("{}", u64::MAX);
    for value in ["8192P", overflow.as_str(), "99999999P"] {
        let rendered = parse_max_alloc_argument(OsStr::new(value))
            .expect_err("ceiling enforced")
            .to_string();
        assert!(
            rendered.contains(&format!("--max-alloc={value} is too large")),
            "expected upstream's too-large text for {value}, got: {rendered}"
        );
    }
    #[cfg(target_pointer_width = "64")]
    assert_eq!(
        parse_max_alloc_argument(OsStr::new("8191P")).expect("below the ceiling"),
        8191 << 50
    );
}

/// Copies one file with `--max-alloc=` set through `args`, returning the
/// exit code and stderr.
fn copy_with_max_alloc(args: &[&str]) -> (i32, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    std::fs::create_dir(&src).expect("create src");
    std::fs::write(src.join("f.txt"), b"payload\n").expect("write source file");

    let mut argv = vec![OsString::from(RSYNC), OsString::from("-a")];
    argv.extend(args.iter().map(OsString::from));
    argv.push(OsString::from(format!("{}/", src.display())));
    argv.push(OsString::from(format!("{}/", dst.display())));
    let (code, _stdout, stderr) = run_with_args(argv);
    let stderr_text = String::from_utf8_lossy(&stderr).into_owned();
    if code == 0 {
        assert_eq!(
            std::fs::read(dst.join("f.txt")).expect("read destination file"),
            b"payload\n"
        );
    }
    (code, stderr_text)
}

#[test]
fn max_alloc_zero_value_transfers() {
    // upstream: rsync 3.5.1 testsuite/max-alloc-zero_test.py - "0 is accepted,
    // and a transfer using it works". 3.5.0 had refused it outright.
    let _guard = clear_rsync_rsh();
    let (code, stderr) = copy_with_max_alloc(&["--max-alloc=0"]);
    assert_eq!(code, 0, "--max-alloc=0 should transfer, got: {stderr}");
}

#[test]
fn max_alloc_non_zero_value_still_transfers() {
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
fn max_alloc_zero_from_the_environment_transfers() {
    // upstream: options.c:2067-2086 - RSYNC_MAX_ALLOC seeds `max_alloc_arg`,
    // so an environment-supplied zero resolves exactly like the flag.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let _rsh = clear_rsync_rsh();
    let _env = EnvGuard::set("RSYNC_MAX_ALLOC", OsStr::new("0"));

    let (code, stderr) = copy_with_max_alloc(&[]);
    assert_eq!(code, 0, "RSYNC_MAX_ALLOC=0 should transfer, got: {stderr}");
}

#[test]
fn max_alloc_below_one_mib_produces_error_exit() {
    // upstream: options.c:1966 - a non-zero `--max-alloc` below 1 MiB is a
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
