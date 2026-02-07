use super::common::*;
use super::*;

// =============================================================================
// CLI parsing: --compress-level accepts all valid levels 0-9
// =============================================================================

#[test]
fn parse_args_compress_level_0_records_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("0")));
    assert!(
        !parsed.compress,
        "--compress-level=0 should disable compression"
    );
}

#[test]
fn parse_args_compress_level_1_enables_compression() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=1"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("1")));
    assert!(
        parsed.compress,
        "--compress-level=1 should enable compression"
    );
}

#[test]
fn parse_args_compress_level_2_enables_compression() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=2"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("2")));
    assert!(parsed.compress);
}

#[test]
fn parse_args_compress_level_3_enables_compression() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=3"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("3")));
    assert!(parsed.compress);
}

#[test]
fn parse_args_compress_level_4_enables_compression() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=4"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("4")));
    assert!(parsed.compress);
}

#[test]
fn parse_args_compress_level_5_enables_compression() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("5")));
    assert!(parsed.compress);
}

#[test]
fn parse_args_compress_level_6_enables_compression() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=6"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("6")));
    assert!(parsed.compress);
}

#[test]
fn parse_args_compress_level_7_enables_compression() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=7"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("7")));
    assert!(parsed.compress);
}

#[test]
fn parse_args_compress_level_8_enables_compression() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=8"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("8")));
    assert!(parsed.compress);
}

#[test]
fn parse_args_compress_level_9_enables_compression() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=9"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("9")));
    assert!(parsed.compress);
}

// =============================================================================
// --compress-level implies --compress (without explicit -z)
// =============================================================================

#[test]
fn compress_level_without_z_flag_enables_compression() {
    // Upstream rsync: --compress-level=N (where N > 0) implies --compress
    for level in 1..=9 {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from(format!("--compress-level={level}")),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .expect("parse");

        assert!(
            parsed.compress,
            "--compress-level={level} without -z should still enable compression"
        );
    }
}

// =============================================================================
// Invalid level rejection
// =============================================================================

#[test]
fn compress_level_negative_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=-1"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(
        rendered.contains("--compress-level=-1"),
        "error should reference the flag value: {rendered}"
    );
}

#[test]
fn compress_level_10_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=10"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(
        rendered.contains("--compress-level=10"),
        "error should reference the flag value: {rendered}"
    );
    assert!(
        rendered.contains("between 0 and 9"),
        "error should mention valid range: {rendered}"
    );
}

#[test]
fn compress_level_99_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=99"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("between 0 and 9"));
}

#[test]
fn compress_level_non_numeric_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=abc"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(
        rendered.contains("--compress-level=abc"),
        "error should reference the flag: {rendered}"
    );
    assert!(
        rendered.contains("invalid"),
        "error should say invalid: {rendered}"
    );
}

// =============================================================================
// --compress-level=0 effectively disables compression (even with -z)
// =============================================================================

#[test]
fn compress_level_zero_overrides_z_flag() {
    // Upstream: --compress-level=0 disables compression even if -z is present
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        OsString::from("--compress-level=0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(
        !parsed.compress,
        "--compress-level=0 should override -z and disable compression"
    );
    assert_eq!(parsed.compress_level, Some(OsString::from("0")));
}

// =============================================================================
// --compress-level overrides --no-compress
// =============================================================================

#[test]
fn compress_level_nonzero_overrides_no_compress() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-compress"),
        OsString::from("--compress-level=6"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(
        parsed.compress,
        "--compress-level=6 should override --no-compress"
    );
    assert_eq!(parsed.compress_level, Some(OsString::from("6")));
}

// =============================================================================
// Default compression level
// =============================================================================

#[test]
fn no_compress_level_specified_defaults_to_none() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.compress_level, None,
        "compress_level should default to None when not specified"
    );
    assert!(
        !parsed.compress,
        "compression should be disabled by default"
    );
}

#[test]
fn compress_flag_alone_uses_default_level() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.compress, "-z should enable compression");
    assert_eq!(
        parsed.compress_level, None,
        "-z without --compress-level should leave compress_level as None (default level 6)"
    );
}

// =============================================================================
// parse_compress_level_argument function tests
// =============================================================================

#[test]
fn parse_compress_level_argument_accepts_all_valid_levels() {
    for level in 0..=9 {
        let value = format!("{level}");
        let result = parse_compress_level_argument(OsStr::new(&value));
        assert!(
            result.is_ok(),
            "parse_compress_level_argument should accept level {level}"
        );

        let setting = result.unwrap();
        if level == 0 {
            assert!(
                setting.is_disabled(),
                "level 0 should produce disabled setting"
            );
        } else {
            assert!(
                setting.is_enabled(),
                "level {level} should produce enabled setting"
            );
        }
    }
}

#[test]
fn parse_compress_level_argument_rejects_10() {
    let result = parse_compress_level_argument(OsStr::new("10"));
    assert!(result.is_err(), "level 10 should be rejected");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("outside the supported range"),
        "error should mention supported range: {msg}"
    );
}

#[test]
fn parse_compress_level_argument_rejects_negative() {
    let result = parse_compress_level_argument(OsStr::new("-1"));
    assert!(result.is_err(), "level -1 should be rejected");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("outside the supported range"),
        "error should mention supported range: {msg}"
    );
}

#[test]
fn parse_compress_level_argument_rejects_empty() {
    let result = parse_compress_level_argument(OsStr::new(""));
    assert!(result.is_err(), "empty value should be rejected");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("must not be empty"),
        "error should mention empty value: {msg}"
    );
}

#[test]
fn parse_compress_level_argument_rejects_float() {
    let result = parse_compress_level_argument(OsStr::new("3.5"));
    assert!(result.is_err(), "float value should be rejected");
}

#[test]
fn parse_compress_level_argument_trims_whitespace() {
    // Whitespace around the value should be handled
    let result = parse_compress_level_argument(OsStr::new(" 6 "));
    assert!(result.is_ok(), "whitespace-padded value should be accepted");
    let setting = result.unwrap();
    assert!(setting.is_enabled());
}

// =============================================================================
// CompressionSetting::try_from_numeric verification
// =============================================================================

#[test]
fn compression_setting_try_from_numeric_matches_upstream_levels() {
    use core::client::CompressionSetting;

    // Level 0 = disabled (upstream: no compression)
    let setting = CompressionSetting::try_from_numeric(0).unwrap();
    assert!(setting.is_disabled());

    // Levels 1-9 = enabled with specific level
    for level in 1..=9 {
        let setting = CompressionSetting::try_from_numeric(level).unwrap();
        assert!(setting.is_enabled(), "level {level} should be enabled");
    }

    // Level 10+ = rejected
    assert!(CompressionSetting::try_from_numeric(10).is_err());
    assert!(CompressionSetting::try_from_numeric(100).is_err());
    assert!(CompressionSetting::try_from_numeric(u32::MAX).is_err());
}

// =============================================================================
// End-to-end: local copies with various compression levels
// =============================================================================

#[test]
fn local_copy_with_all_valid_compress_levels() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    std::fs::write(&source, b"compressible test data content").expect("write source");

    for level in 0..=9 {
        let dest_name = format!("dest_{level}.txt");
        let destination = tmp.path().join(&dest_name);

        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from(format!("--compress-level={level}")),
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ]);

        assert_eq!(
            code,
            0,
            "compress-level={level} should succeed; stderr: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert!(stdout.is_empty());
        assert!(
            stderr.is_empty(),
            "compress-level={level} produced stderr: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert_eq!(
            std::fs::read(&destination).expect("read destination"),
            b"compressible test data content",
            "compress-level={level} produced incorrect output"
        );
    }
}
