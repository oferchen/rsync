use super::common::*;
use super::*;

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

#[test]
fn compress_level_negative_is_clamped_not_rejected() {
    // upstream: token.c:init_compression_level() clamps out-of-range levels
    // instead of erroring; -1 maps to the zlib default and enables compression.
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=-1"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("--compress-level=-1 should clamp, not error");

    assert!(
        parsed.compress,
        "--compress-level=-1 should enable compression"
    );
    assert_eq!(parsed.compress_level, Some(OsString::from("-1")));
}

#[test]
fn compress_level_above_max_is_clamped_not_rejected() {
    // upstream saturates values above max_level (9) rather than rejecting them.
    for level in ["10", "99"] {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from(format!("--compress-level={level}")),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .unwrap_or_else(|_| panic!("--compress-level={level} should clamp, not error"));

        assert!(
            parsed.compress,
            "--compress-level={level} should enable compression"
        );
        assert_eq!(parsed.compress_level, Some(OsString::from(level)));
    }
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

#[test]
fn parse_compress_level_argument_accepts_all_valid_levels() {
    use compress::algorithm::CompressionAlgorithm;

    for level in 0..=9 {
        let value = format!("{level}");
        let result = parse_compress_level_argument(OsStr::new(&value), CompressionAlgorithm::Zlib);
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
fn parse_compress_level_argument_clamps_above_zlib_max() {
    use compress::algorithm::CompressionAlgorithm;
    use compress::zlib::CompressionLevel;

    // upstream: token.c:init_compression_level() saturates to max_level (9)
    // rather than rejecting the value.
    for value in ["10", "22", "99"] {
        let setting = parse_compress_level_argument(OsStr::new(value), CompressionAlgorithm::Zlib)
            .unwrap_or_else(|_| panic!("level {value} should clamp, not error"));
        assert_eq!(
            setting.level_or_default(),
            CompressionLevel::from_numeric(9).unwrap(),
            "zlib level {value} should clamp to 9"
        );
    }
}

#[test]
fn parse_compress_level_argument_maps_negative_one_to_zlib_default() {
    use compress::algorithm::CompressionAlgorithm;
    use compress::zlib::CompressionLevel;

    // upstream: token.c - Z_DEFAULT_COMPRESSION (-1) remaps to the real default.
    let setting = parse_compress_level_argument(OsStr::new("-1"), CompressionAlgorithm::Zlib)
        .expect("-1 should map to the zlib default, not error");
    assert_eq!(
        setting.level_or_default(),
        CompressionLevel::from_numeric(6).unwrap(),
        "zlib -1 should map to default level 6"
    );
}

#[test]
fn parse_compress_level_argument_clamps_large_negative_to_min() {
    use compress::algorithm::CompressionAlgorithm;
    use compress::zlib::CompressionLevel;

    // upstream: token.c saturates values below min_level (1).
    let setting = parse_compress_level_argument(OsStr::new("-5"), CompressionAlgorithm::Zlib)
        .expect("-5 should clamp, not error");
    assert_eq!(
        setting.level_or_default(),
        CompressionLevel::from_numeric(1).unwrap(),
        "zlib -5 should clamp to min level 1"
    );
}

#[cfg(feature = "zstd")]
#[test]
fn parse_compress_level_argument_clamps_to_zstd_range() {
    use compress::algorithm::CompressionAlgorithm;
    use compress::zlib::CompressionLevel;
    use std::num::NonZeroU8;

    // upstream: token.c zstd branch - max_level is ZSTD_maxCLevel() (22) and 0
    // selects ZSTD_CLEVEL_DEFAULT (3) rather than disabling compression.
    let expect = |raw: &str, level: u8| {
        let setting = parse_compress_level_argument(OsStr::new(raw), CompressionAlgorithm::Zstd)
            .unwrap_or_else(|_| panic!("zstd level {raw} should clamp, not error"));
        assert_eq!(
            setting.level_or_default(),
            CompressionLevel::precise(NonZeroU8::new(level).unwrap()),
            "zstd level {raw} should resolve to {level}"
        );
    };
    expect("0", 3);
    expect("22", 22);
    expect("99", 22);
    expect("15", 15);
}

#[cfg(feature = "zstd")]
#[test]
fn parse_compress_level_argument_threads_negative_zstd_to_encoder() {
    use compress::algorithm::{CompressionAlgorithm, zstd_min_level};

    // WHY: --compress-level=-5 must survive the whole CLI pipeline
    // (parse -> CompressionSetting -> CompressionLevel) and map to the raw
    // signed level the zstd encoder feeds to ZSTD_c_compressionLevel. The
    // historical unsigned NonZeroU8 clamp collapsed every negative to 1 here,
    // long before the resolver saw it. upstream: token.c:74,803.
    let resolved = |raw: &str| {
        let setting = parse_compress_level_argument(OsStr::new(raw), CompressionAlgorithm::Zstd)
            .unwrap_or_else(|_| panic!("zstd level {raw} should clamp, not error"));
        compress::zstd::level_to_i32(setting.level_or_default())
    };

    assert_eq!(resolved("-5"), -5, "-5 reaches the encoder unchanged");
    let min = zstd_min_level();
    assert_eq!(
        resolved(&min.to_string()),
        min,
        "the exact ZSTD_minCLevel() boundary is preserved end to end"
    );
    assert_eq!(
        resolved(&(min - 1).to_string()),
        min,
        "below ZSTD_minCLevel() saturates UP to the min, never rejected"
    );
    // No regression: level 0 still selects the zstd default (3), never negative.
    assert_eq!(resolved("0"), 3, "level 0 = zstd default, unchanged");
}

#[test]
fn parse_compress_level_argument_rejects_empty() {
    use compress::algorithm::CompressionAlgorithm;

    let result = parse_compress_level_argument(OsStr::new(""), CompressionAlgorithm::Zlib);
    assert!(result.is_err(), "empty value should be rejected");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("must not be empty"),
        "error should mention empty value: {msg}"
    );
}

#[test]
fn parse_compress_level_argument_rejects_float() {
    use compress::algorithm::CompressionAlgorithm;

    let result = parse_compress_level_argument(OsStr::new("3.5"), CompressionAlgorithm::Zlib);
    assert!(result.is_err(), "float value should be rejected");
}

#[test]
fn parse_compress_level_argument_trims_whitespace() {
    use compress::algorithm::CompressionAlgorithm;

    // Whitespace around the value should be handled
    let result = parse_compress_level_argument(OsStr::new(" 6 "), CompressionAlgorithm::Zlib);
    assert!(result.is_ok(), "whitespace-padded value should be accepted");
    let setting = result.unwrap();
    assert!(setting.is_enabled());
}

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

#[test]
fn local_copy_with_all_valid_compress_levels() {
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let _guard = EnvGuard::remove("OC_RSYNC_FORCE_NO_COMPRESS");
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
