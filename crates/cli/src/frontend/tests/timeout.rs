use super::common::*;
use super::*;

// =============================================================================
// Basic timeout argument parsing tests
// =============================================================================

#[test]
fn timeout_argument_zero_disables_timeout() {
    let timeout = parse_timeout_argument(OsStr::new("0")).expect("parse timeout");
    assert_eq!(timeout, TransferTimeout::Disabled);
}

#[test]
fn timeout_argument_positive_sets_seconds() {
    let timeout = parse_timeout_argument(OsStr::new("15")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(15));
}

#[test]
fn timeout_argument_negative_reports_error() {
    let error = parse_timeout_argument(OsStr::new("-1")).unwrap_err();
    assert!(error.to_string().contains("timeout must be non-negative"));
}

// =============================================================================
// Comprehensive timeout argument parsing tests
// =============================================================================

#[test]
fn timeout_argument_one_second() {
    let timeout = parse_timeout_argument(OsStr::new("1")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(1));
}

#[test]
fn timeout_argument_typical_values() {
    // Common timeout values used in practice
    for value in [10, 30, 60, 120, 300, 600, 3600] {
        let timeout =
            parse_timeout_argument(OsStr::new(&value.to_string())).expect("parse timeout");
        assert_eq!(timeout.as_seconds(), NonZeroU64::new(value));
    }
}

#[test]
fn timeout_argument_large_value() {
    // 24 hours in seconds
    let timeout = parse_timeout_argument(OsStr::new("86400")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(86400));
}

#[test]
fn timeout_argument_very_large_value() {
    // Maximum practical timeout (about 136 years)
    let timeout = parse_timeout_argument(OsStr::new("4294967295")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(4294967295));
}

#[test]
fn timeout_argument_u64_max() {
    // Maximum u64 value
    let max_u64 = u64::MAX.to_string();
    let timeout = parse_timeout_argument(OsStr::new(&max_u64)).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(u64::MAX));
}

#[test]
fn timeout_argument_with_leading_plus() {
    let timeout = parse_timeout_argument(OsStr::new("+30")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(30));
}

#[test]
fn timeout_argument_with_leading_plus_zero() {
    let timeout = parse_timeout_argument(OsStr::new("+0")).expect("parse timeout");
    assert_eq!(timeout, TransferTimeout::Disabled);
}

#[test]
fn timeout_argument_with_leading_whitespace() {
    let timeout = parse_timeout_argument(OsStr::new("  30")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(30));
}

#[test]
fn timeout_argument_with_trailing_whitespace() {
    let timeout = parse_timeout_argument(OsStr::new("30  ")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(30));
}

#[test]
fn timeout_argument_with_surrounding_whitespace() {
    let timeout = parse_timeout_argument(OsStr::new("   30   ")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(30));
}

// =============================================================================
// Error case tests for timeout argument parsing
// =============================================================================

#[test]
fn timeout_argument_empty_reports_error() {
    let error = parse_timeout_argument(OsStr::new("")).unwrap_err();
    assert!(
        error.to_string().contains("must not be empty"),
        "error message: {error}"
    );
}

#[test]
fn timeout_argument_whitespace_only_reports_error() {
    let error = parse_timeout_argument(OsStr::new("   ")).unwrap_err();
    assert!(
        error.to_string().contains("must not be empty"),
        "error message: {error}"
    );
}

#[test]
fn timeout_argument_non_numeric_reports_error() {
    let error = parse_timeout_argument(OsStr::new("abc")).unwrap_err();
    assert!(
        error.to_string().contains("must be an unsigned integer"),
        "error message: {error}"
    );
}

#[test]
fn timeout_argument_mixed_alphanumeric_reports_error() {
    let error = parse_timeout_argument(OsStr::new("30s")).unwrap_err();
    assert!(
        error.to_string().contains("must be an unsigned integer"),
        "error message: {error}"
    );
}

#[test]
fn timeout_argument_decimal_reports_error() {
    let error = parse_timeout_argument(OsStr::new("30.5")).unwrap_err();
    assert!(
        error.to_string().contains("must be an unsigned integer"),
        "error message: {error}"
    );
}

#[test]
fn timeout_argument_negative_large_reports_error() {
    let error = parse_timeout_argument(OsStr::new("-999")).unwrap_err();
    assert!(
        error.to_string().contains("timeout must be non-negative"),
        "error message: {error}"
    );
}

#[test]
fn timeout_argument_overflow_reports_error() {
    // Value larger than u64::MAX
    let error = parse_timeout_argument(OsStr::new("18446744073709551616")).unwrap_err();
    assert!(
        error.to_string().contains("exceeds the supported range"),
        "error message: {error}"
    );
}

#[test]
fn timeout_argument_special_characters_report_error() {
    for special in ["!", "@", "#", "$", "%", "^", "&", "*", "(", ")"] {
        let error = parse_timeout_argument(OsStr::new(special)).unwrap_err();
        assert!(
            error.is_error(),
            "special char '{special}' should cause error"
        );
    }
}

// =============================================================================
// TransferTimeout enum behavior tests
// =============================================================================

#[test]
fn transfer_timeout_default_returns_default_duration() {
    let timeout = TransferTimeout::Default;
    let default = Duration::from_secs(30);
    assert_eq!(timeout.effective(default), Some(default));
}

#[test]
fn transfer_timeout_disabled_returns_none() {
    let timeout = TransferTimeout::Disabled;
    let default = Duration::from_secs(30);
    assert_eq!(timeout.effective(default), None);
}

#[test]
fn transfer_timeout_seconds_overrides_default() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(60).unwrap());
    let default = Duration::from_secs(30);
    assert_eq!(timeout.effective(default), Some(Duration::from_secs(60)));
}

#[test]
fn transfer_timeout_as_seconds_returns_value() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(45).unwrap());
    assert_eq!(timeout.as_seconds(), Some(NonZeroU64::new(45).unwrap()));
}

#[test]
fn transfer_timeout_default_as_seconds_returns_none() {
    let timeout = TransferTimeout::Default;
    assert_eq!(timeout.as_seconds(), None);
}

#[test]
fn transfer_timeout_disabled_as_seconds_returns_none() {
    let timeout = TransferTimeout::Disabled;
    assert_eq!(timeout.as_seconds(), None);
}

#[test]
fn transfer_timeout_equality() {
    assert_eq!(TransferTimeout::Default, TransferTimeout::Default);
    assert_eq!(TransferTimeout::Disabled, TransferTimeout::Disabled);
    assert_eq!(
        TransferTimeout::Seconds(NonZeroU64::new(30).unwrap()),
        TransferTimeout::Seconds(NonZeroU64::new(30).unwrap())
    );
    assert_ne!(TransferTimeout::Default, TransferTimeout::Disabled);
    assert_ne!(
        TransferTimeout::Seconds(NonZeroU64::new(30).unwrap()),
        TransferTimeout::Seconds(NonZeroU64::new(60).unwrap())
    );
}

#[test]
fn transfer_timeout_debug_format() {
    let timeout = TransferTimeout::Default;
    assert_eq!(format!("{timeout:?}"), "Default");

    let timeout = TransferTimeout::Disabled;
    assert_eq!(format!("{timeout:?}"), "Disabled");

    let timeout = TransferTimeout::Seconds(NonZeroU64::new(30).unwrap());
    let debug = format!("{timeout:?}");
    assert!(debug.contains("Seconds"));
    assert!(debug.contains("30"));
}

#[test]
fn transfer_timeout_clone_and_copy() {
    let original = TransferTimeout::Seconds(NonZeroU64::new(30).unwrap());
    let cloned = original;
    let copied: TransferTimeout = original;
    assert_eq!(original, cloned);
    assert_eq!(original, copied);
}

// =============================================================================
// CLI argument integration tests for --timeout
// =============================================================================

#[test]
fn cli_timeout_with_equals_syntax() {
    let parsed = parse_args(["rsync", "--timeout=30", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.timeout, Some(OsString::from("30")));
}

#[test]
fn cli_timeout_with_space_syntax() {
    let parsed = parse_args(["rsync", "--timeout", "30", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.timeout, Some(OsString::from("30")));
}

#[test]
fn cli_timeout_zero_value() {
    let parsed = parse_args(["rsync", "--timeout=0", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.timeout, Some(OsString::from("0")));
}

#[test]
fn cli_timeout_large_value() {
    let parsed = parse_args(["rsync", "--timeout=86400", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.timeout, Some(OsString::from("86400")));
}

#[test]
fn cli_no_timeout_overrides_timeout() {
    let parsed =
        parse_args(["rsync", "--timeout=30", "--no-timeout", "src/", "dst/"]).expect("parse");
    // --no-timeout should clear the timeout setting
    assert_eq!(parsed.timeout, None);
}

#[test]
fn cli_timeout_after_no_timeout() {
    let parsed =
        parse_args(["rsync", "--no-timeout", "--timeout=60", "src/", "dst/"]).expect("parse");
    // Later --timeout should override --no-timeout
    assert_eq!(parsed.timeout, Some(OsString::from("60")));
}

#[test]
fn cli_timeout_default_is_none() {
    let parsed = parse_args(["rsync", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.timeout, None);
}

// =============================================================================
// CLI argument integration tests for --contimeout
// =============================================================================

#[test]
fn cli_contimeout_with_equals_syntax() {
    let parsed = parse_args(["rsync", "--contimeout=10", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.contimeout, Some(OsString::from("10")));
}

#[test]
fn cli_contimeout_with_space_syntax() {
    let parsed = parse_args(["rsync", "--contimeout", "10", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.contimeout, Some(OsString::from("10")));
}

#[test]
fn cli_contimeout_zero_value() {
    let parsed = parse_args(["rsync", "--contimeout=0", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.contimeout, Some(OsString::from("0")));
}

#[test]
fn cli_no_contimeout_overrides_contimeout() {
    let parsed = parse_args([
        "rsync",
        "--contimeout=10",
        "--no-contimeout",
        "src/",
        "dst/",
    ])
    .expect("parse");
    assert_eq!(parsed.contimeout, None);
}

#[test]
fn cli_contimeout_after_no_contimeout() {
    let parsed = parse_args([
        "rsync",
        "--no-contimeout",
        "--contimeout=20",
        "src/",
        "dst/",
    ])
    .expect("parse");
    assert_eq!(parsed.contimeout, Some(OsString::from("20")));
}

#[test]
fn cli_contimeout_default_is_none() {
    let parsed = parse_args(["rsync", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.contimeout, None);
}

// =============================================================================
// Combined timeout and contimeout tests
// =============================================================================

#[test]
fn cli_both_timeout_and_contimeout() {
    let parsed =
        parse_args(["rsync", "--timeout=30", "--contimeout=10", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.timeout, Some(OsString::from("30")));
    assert_eq!(parsed.contimeout, Some(OsString::from("10")));
}

#[test]
fn cli_timeout_without_contimeout() {
    let parsed = parse_args(["rsync", "--timeout=30", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.timeout, Some(OsString::from("30")));
    assert_eq!(parsed.contimeout, None);
}

#[test]
fn cli_contimeout_without_timeout() {
    let parsed = parse_args(["rsync", "--contimeout=10", "src/", "dst/"]).expect("parse");
    assert_eq!(parsed.timeout, None);
    assert_eq!(parsed.contimeout, Some(OsString::from("10")));
}

// =============================================================================
// Edge cases for timeout values
// =============================================================================

#[test]
fn timeout_argument_boundary_one() {
    // Smallest positive timeout
    let timeout = parse_timeout_argument(OsStr::new("1")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(1));
}

#[test]
fn timeout_argument_minute_boundary() {
    // 1 minute
    let timeout = parse_timeout_argument(OsStr::new("60")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(60));
}

#[test]
fn timeout_argument_hour_boundary() {
    // 1 hour
    let timeout = parse_timeout_argument(OsStr::new("3600")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(3600));
}

#[test]
fn timeout_argument_day_boundary() {
    // 1 day
    let timeout = parse_timeout_argument(OsStr::new("86400")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(86400));
}

#[test]
fn timeout_argument_week_boundary() {
    // 1 week
    let timeout = parse_timeout_argument(OsStr::new("604800")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(604800));
}

// =============================================================================
// Duration conversion tests
// =============================================================================

#[test]
fn transfer_timeout_effective_with_various_defaults() {
    let timeout = TransferTimeout::Default;

    // Test with different default values
    for default_secs in [1, 10, 30, 60, 300, 3600] {
        let default = Duration::from_secs(default_secs);
        assert_eq!(timeout.effective(default), Some(default));
    }
}

#[test]
fn transfer_timeout_seconds_converts_to_duration_correctly() {
    for secs in [1, 30, 60, 3600, 86400] {
        let timeout = TransferTimeout::Seconds(NonZeroU64::new(secs).unwrap());
        let default = Duration::from_secs(999); // Should be ignored
        assert_eq!(timeout.effective(default), Some(Duration::from_secs(secs)));
    }
}

#[test]
fn transfer_timeout_disabled_ignores_default() {
    let timeout = TransferTimeout::Disabled;

    // No matter what default is provided, disabled always returns None
    for default_secs in [1, 30, 60, 3600] {
        let default = Duration::from_secs(default_secs);
        assert_eq!(timeout.effective(default), None);
    }
}

// =============================================================================
// Error message quality tests
// =============================================================================

#[test]
fn timeout_error_includes_invalid_value_in_message() {
    let error = parse_timeout_argument(OsStr::new("invalid")).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("invalid"),
        "error should mention the invalid value: {message}"
    );
}

#[test]
fn timeout_negative_error_includes_value() {
    let error = parse_timeout_argument(OsStr::new("-42")).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("-42"),
        "error should mention the invalid value: {message}"
    );
}

#[test]
fn timeout_overflow_error_is_descriptive() {
    let error = parse_timeout_argument(OsStr::new("99999999999999999999999")).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("exceeds") || message.contains("range"),
        "error should mention overflow: {message}"
    );
}

// =============================================================================
// Exit code tests
// =============================================================================

#[test]
fn timeout_parse_error_has_exit_code_1() {
    let error = parse_timeout_argument(OsStr::new("invalid")).unwrap_err();
    assert_eq!(
        error.code(),
        Some(1),
        "syntax errors should have exit code 1"
    );
}

#[test]
fn timeout_negative_error_has_exit_code_1() {
    let error = parse_timeout_argument(OsStr::new("-1")).unwrap_err();
    assert_eq!(
        error.code(),
        Some(1),
        "negative value should have exit code 1"
    );
}

#[test]
fn timeout_empty_error_has_exit_code_1() {
    let error = parse_timeout_argument(OsStr::new("")).unwrap_err();
    assert_eq!(error.code(), Some(1), "empty value should have exit code 1");
}
