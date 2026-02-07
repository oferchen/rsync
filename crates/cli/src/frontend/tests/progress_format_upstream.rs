//! Tests verifying that --progress output format matches upstream rsync.
//!
//! Upstream rsync --progress per-file format:
//!   `   32,768 100%    1.23MB/s    0:00:00 (xfr#1, to-chk=5/7)`
//!
//! The format consists of:
//!   - Right-aligned bytes (15-char field, thousands-separated)
//!   - Right-aligned percentage (4-char field)
//!   - Right-aligned transfer rate (12-char field, kB/s, MB/s, GB/s)
//!   - Right-aligned elapsed time (11-char field, H:MM:SS)
//!   - Transfer count and remaining count: (xfr#N, to-chk=M/T)
//!
//! When totals are unavailable, percentage shows as "??%".

use super::common::*;
use super::*;

// ============================================================================
// Progress line format structure tests (integration)
// ============================================================================

#[test]
fn progress_line_contains_upstream_fields() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("upstream_fmt.txt");
    let destination = tmp.path().join("upstream_fmt.out");
    std::fs::write(&source, b"hello world").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    let normalized = rendered.replace('\r', "\n");
    let lines: Vec<&str> = normalized.lines().collect();

    // Find the final progress line (the one with xfr#)
    let progress_line = lines
        .iter()
        .find(|l| l.contains("xfr#"))
        .expect("should contain xfr# progress line");

    // Upstream format: bytes percentage rate elapsed (xfr#N, to-chk=M/T)
    assert!(
        progress_line.contains("100%"),
        "progress line should show 100%: {progress_line:?}"
    );
    assert!(
        progress_line.contains("(xfr#1, to-chk=0/1)"),
        "progress line should show transfer count: {progress_line:?}"
    );
    assert!(
        progress_line.contains("0:00:0"),
        "progress line should show elapsed time H:MM:SS: {progress_line:?}"
    );
    // Rate should end with B/s, kB/s, MB/s, or GB/s
    assert!(
        progress_line.contains("B/s"),
        "progress line should show transfer rate: {progress_line:?}"
    );
}

#[test]
fn progress_line_bytes_field_uses_thousands_separator() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("sep.bin");
    let destination = tmp.path().join("sep.out");
    // Write 1536 bytes so the decimal format shows "1,536"
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    let normalized = rendered.replace('\r', "\n");

    assert!(
        normalized.contains("1,536"),
        "bytes field should use thousands separator: {normalized:?}"
    );
}

// ============================================================================
// Per-file progress format: percentage calculation
// ============================================================================

#[test]
fn format_progress_percent_zero_percent() {
    assert_eq!(format_progress_percent(0, Some(100)), "0%");
}

#[test]
fn format_progress_percent_half() {
    assert_eq!(format_progress_percent(50, Some(100)), "50%");
}

#[test]
fn format_progress_percent_full() {
    assert_eq!(format_progress_percent(100, Some(100)), "100%");
}

#[test]
fn format_progress_percent_exceeds_total_caps_at_100() {
    assert_eq!(format_progress_percent(200, Some(100)), "100%");
}

#[test]
fn format_progress_percent_zero_total_returns_100() {
    // Upstream rsync treats zero-length files as 100% complete
    assert_eq!(format_progress_percent(0, Some(0)), "100%");
}

#[test]
fn format_progress_percent_unknown_total_shows_placeholder() {
    // When the total is not available, upstream shows "??%"
    assert_eq!(format_progress_percent(42, None), "??%");
}

#[test]
fn format_progress_percent_large_file() {
    // 1GB file, half transferred
    assert_eq!(
        format_progress_percent(500_000_000, Some(1_000_000_000)),
        "50%"
    );
}

#[test]
fn format_progress_percent_one_percent() {
    assert_eq!(format_progress_percent(1, Some(100)), "1%");
}

#[test]
fn format_progress_percent_99_percent() {
    assert_eq!(format_progress_percent(99, Some(100)), "99%");
}

// ============================================================================
// Rate formatting (kB/s, MB/s, GB/s)
// ============================================================================

#[test]
fn format_progress_rate_decimal_small_rate() {
    // Under 1MB/s -> shows kB/s
    let rate_str = format_progress_rate_decimal(512.0);
    assert!(
        rate_str.contains("kB/s"),
        "small rate should use kB/s: {rate_str:?}"
    );
}

#[test]
fn format_progress_rate_decimal_megabyte_rate() {
    // 1MB/s = 1048576 bytes/s
    let rate_str = format_progress_rate_decimal(1_048_576.0);
    assert!(
        rate_str.contains("MB/s"),
        "megabyte rate should use MB/s: {rate_str:?}"
    );
}

#[test]
fn format_progress_rate_decimal_gigabyte_rate() {
    // 1GB/s = 1073741824 bytes/s
    let rate_str = format_progress_rate_decimal(1_073_741_824.0);
    assert!(
        rate_str.contains("GB/s"),
        "gigabyte rate should use GB/s: {rate_str:?}"
    );
}

#[test]
fn format_progress_rate_human_small() {
    let rate_str = format_progress_rate_human(500.0);
    assert!(
        rate_str.contains("B/s"),
        "human rate <1000 should use B/s: {rate_str:?}"
    );
}

#[test]
fn format_progress_rate_human_kilo() {
    let rate_str = format_progress_rate_human(1_500.0);
    assert!(
        rate_str.contains("kB/s"),
        "human rate ~1.5k should use kB/s: {rate_str:?}"
    );
}

#[test]
fn format_progress_rate_human_mega() {
    let rate_str = format_progress_rate_human(2_500_000.0);
    assert!(
        rate_str.contains("MB/s"),
        "human rate ~2.5M should use MB/s: {rate_str:?}"
    );
}

#[test]
fn format_progress_rate_human_giga() {
    let rate_str = format_progress_rate_human(2_500_000_000.0);
    assert!(
        rate_str.contains("GB/s"),
        "human rate ~2.5G should use GB/s: {rate_str:?}"
    );
}

#[test]
fn format_progress_rate_zero_elapsed_returns_zero() {
    let rate_str = format_progress_rate(0, Duration::ZERO, HumanReadableMode::Disabled);
    assert_eq!(rate_str, "0.00kB/s");
}

#[test]
fn format_progress_rate_zero_bytes_returns_zero() {
    let rate_str = format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Disabled);
    assert_eq!(rate_str, "0.00kB/s");
}

#[test]
fn format_progress_rate_zero_bytes_human_returns_zero() {
    let rate_str = format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Enabled);
    assert_eq!(rate_str, "0.00B/s");
}

// ============================================================================
// Elapsed time formatting (H:MM:SS)
// ============================================================================

#[test]
fn format_progress_elapsed_zero() {
    assert_eq!(format_progress_elapsed(Duration::ZERO), "0:00:00");
}

#[test]
fn format_progress_elapsed_one_second() {
    assert_eq!(format_progress_elapsed(Duration::from_secs(1)), "0:00:01");
}

#[test]
fn format_progress_elapsed_59_seconds() {
    assert_eq!(format_progress_elapsed(Duration::from_secs(59)), "0:00:59");
}

#[test]
fn format_progress_elapsed_one_minute() {
    assert_eq!(format_progress_elapsed(Duration::from_secs(60)), "0:01:00");
}

#[test]
fn format_progress_elapsed_one_hour() {
    assert_eq!(
        format_progress_elapsed(Duration::from_secs(3600)),
        "1:00:00"
    );
}

#[test]
fn format_progress_elapsed_complex() {
    // 1 hour, 23 minutes, 45 seconds = 5025 seconds
    assert_eq!(
        format_progress_elapsed(Duration::from_secs(5025)),
        "1:23:45"
    );
}

#[test]
fn format_progress_elapsed_large_hours() {
    // 100 hours, 0 minutes, 0 seconds
    assert_eq!(
        format_progress_elapsed(Duration::from_secs(360_000)),
        "100:00:00"
    );
}

#[test]
fn format_progress_elapsed_ignores_subsecond() {
    // Fractional seconds should be truncated
    assert_eq!(
        format_progress_elapsed(Duration::from_millis(1999)),
        "0:00:01"
    );
}

// ============================================================================
// Transfer count and check count in progress line
// ============================================================================

#[test]
fn progress_multiple_files_shows_correct_xfr_and_to_chk() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("multi_src");
    std::fs::create_dir_all(&source_dir).expect("mkdir source");
    std::fs::write(source_dir.join("a.txt"), b"aaa").expect("write a");
    std::fs::write(source_dir.join("b.txt"), b"bbb").expect("write b");
    std::fs::write(source_dir.join("c.txt"), b"ccc").expect("write c");

    let dest_dir = tmp.path().join("multi_dst");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("-r"),
        source_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );

    let rendered = String::from_utf8(stdout).expect("utf8");
    let normalized = rendered.replace('\r', "\n");

    // Should have xfr#1, xfr#2, xfr#3 somewhere in the output
    assert!(
        normalized.contains("xfr#1"),
        "should contain xfr#1: {normalized:?}"
    );
    assert!(
        normalized.contains("xfr#2"),
        "should contain xfr#2: {normalized:?}"
    );
    assert!(
        normalized.contains("xfr#3"),
        "should contain xfr#3: {normalized:?}"
    );

    // The last xfr line should have to-chk=0/N (all checked)
    let last_xfr_line = normalized
        .lines()
        .filter(|l| l.contains("xfr#3"))
        .next_back()
        .expect("should have xfr#3 line");
    assert!(
        last_xfr_line.contains("to-chk=0/"),
        "last transfer should show to-chk=0/N: {last_xfr_line:?}"
    );
}

// ============================================================================
// -P is equivalent to --partial --progress
// ============================================================================

#[test]
fn p_short_option_enables_progress_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("p_flag.txt");
    let destination = tmp.path().join("p_flag.out");
    std::fs::write(&source, b"p_flag_data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-P"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    // -P should imply --progress, so xfr# should be present
    assert!(
        rendered.contains("xfr#1"),
        "-P should enable progress output with xfr# line: {rendered:?}"
    );
    assert!(
        rendered.contains("to-chk=0/1"),
        "-P should show to-chk counter: {rendered:?}"
    );
    // Also should keep partial files (test the --partial side)
    assert_eq!(
        std::fs::read(&destination).expect("read destination"),
        b"p_flag_data"
    );
}

#[test]
fn p_short_option_sets_progress_setting_in_parsed_args() {
    use crate::frontend::arguments::parse_args;
    use crate::frontend::progress::ProgressSetting;

    let args = ["rsync", "-P", "src/", "dst/"];
    let parsed = parse_args(args.iter().map(|s| s.to_string())).expect("parse");
    assert_eq!(
        parsed.progress,
        ProgressSetting::PerFile,
        "-P should set progress to PerFile"
    );
    assert!(parsed.partial, "-P should set partial to true");
}

#[test]
fn double_p_short_option_sets_progress_and_partial() {
    use crate::frontend::arguments::parse_args;
    use crate::frontend::progress::ProgressSetting;

    let args = ["rsync", "-PP", "src/", "dst/"];
    let parsed = parse_args(args.iter().map(|s| s.to_string())).expect("parse");
    assert_eq!(
        parsed.progress,
        ProgressSetting::PerFile,
        "-PP should set progress to PerFile"
    );
    assert!(parsed.partial, "-PP should set partial to true");
}

// ============================================================================
// --progress with --dry-run
// ============================================================================

#[test]
fn progress_with_dry_run_shows_progress_info() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("dry_progress.txt");
    let destination = tmp.path().join("dry_progress.out");
    std::fs::write(&source, b"dry run data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--dry-run"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );

    let rendered = String::from_utf8(stdout).expect("utf8");
    // In dry-run mode, progress may show file names and xfr lines
    // or may be suppressed since no actual transfer occurs.
    // Upstream rsync with --progress --dry-run shows the file listing
    // with progress counters.
    assert!(
        rendered.contains("dry_progress.txt") || rendered.contains("xfr#"),
        "--progress --dry-run should show file name or progress: {rendered:?}"
    );

    // Destination should NOT exist (dry-run)
    assert!(
        !destination.exists(),
        "dry-run should not create destination file"
    );
}

// ============================================================================
// Bytes formatting: decimal thousands-separated (upstream default)
// ============================================================================

#[test]
fn format_decimal_bytes_zero() {
    assert_eq!(format_decimal_bytes(0), "0");
}

#[test]
fn format_decimal_bytes_under_thousand() {
    assert_eq!(format_decimal_bytes(999), "999");
}

#[test]
fn format_decimal_bytes_exact_thousand() {
    assert_eq!(format_decimal_bytes(1_000), "1,000");
}

#[test]
fn format_decimal_bytes_tens_of_thousands() {
    assert_eq!(format_decimal_bytes(12_345), "12,345");
}

#[test]
fn format_decimal_bytes_millions() {
    assert_eq!(format_decimal_bytes(1_234_567), "1,234,567");
}

#[test]
fn format_decimal_bytes_billions() {
    assert_eq!(format_decimal_bytes(1_234_567_890), "1,234,567,890");
}

#[test]
fn format_decimal_bytes_u64_max() {
    // Verify no panic on large values
    let result = format_decimal_bytes(u64::MAX);
    assert!(result.contains(','), "u64::MAX should contain separators");
    assert!(!result.is_empty());
}

// ============================================================================
// Human-readable bytes formatting
// ============================================================================

#[test]
fn format_human_bytes_under_threshold() {
    assert_eq!(format_human_bytes(0), "0");
    assert_eq!(format_human_bytes(999), "999");
}

#[test]
fn format_human_bytes_kilo_range() {
    assert_eq!(format_human_bytes(1_000), "1.00K");
    assert_eq!(format_human_bytes(1_500), "1.50K");
    assert_eq!(format_human_bytes(999_999), "1000.00K");
}

#[test]
fn format_human_bytes_mega_range() {
    assert_eq!(format_human_bytes(1_000_000), "1.00M");
    assert_eq!(format_human_bytes(2_500_000), "2.50M");
}

#[test]
fn format_human_bytes_giga_range() {
    assert_eq!(format_human_bytes(1_000_000_000), "1.00G");
}

#[test]
fn format_human_bytes_tera_range() {
    assert_eq!(format_human_bytes(1_000_000_000_000), "1.00T");
}

#[test]
fn format_human_bytes_peta_range() {
    assert_eq!(format_human_bytes(1_000_000_000_000_000), "1.00P");
}

// ============================================================================
// Progress output field alignment tests
// ============================================================================

#[test]
fn progress_bytes_field_is_right_aligned_15_chars() {
    // The bytes field in progress output should be right-aligned in a 15-char field
    let small = format!(
        "{:>15}",
        format_progress_bytes(11, HumanReadableMode::Disabled)
    );
    assert_eq!(small.len(), 15);
    assert!(
        small.starts_with(' '),
        "small value should be right-padded: {small:?}"
    );

    let large = format!(
        "{:>15}",
        format_progress_bytes(1_234_567, HumanReadableMode::Disabled)
    );
    assert_eq!(large.len(), 15);
}

#[test]
fn progress_percent_field_is_right_aligned_4_chars() {
    let pct = format!("{:>4}", format_progress_percent(50, Some(100)));
    assert_eq!(pct.len(), 4);
    assert_eq!(pct, " 50%");
}

#[test]
fn progress_percent_field_100_is_4_chars() {
    let pct = format!("{:>4}", format_progress_percent(100, Some(100)));
    assert_eq!(pct.len(), 4);
    assert_eq!(pct, "100%");
}

#[test]
fn progress_percent_field_unknown_is_3_chars_right_padded() {
    let pct = format!("{:>4}", format_progress_percent(0, None));
    assert_eq!(pct.len(), 4);
    assert_eq!(pct, " ??%");
}

// ============================================================================
// Human-readable rate display (verbose)
// ============================================================================

#[test]
fn format_verbose_rate_human_byte_range() {
    let (value, unit) = format_verbose_rate_human(500.0);
    assert_eq!(value, "500.00");
    assert_eq!(unit, "B/s");
}

#[test]
fn format_verbose_rate_human_kilo_range() {
    let (value, unit) = format_verbose_rate_human(1_500.0);
    assert_eq!(value, "1.50");
    assert_eq!(unit, "kB/s");
}

#[test]
fn format_verbose_rate_human_mega_range() {
    let (value, unit) = format_verbose_rate_human(2_500_000.0);
    assert_eq!(value, "2.50");
    assert_eq!(unit, "MB/s");
}

#[test]
fn format_verbose_rate_human_giga_range() {
    let (value, unit) = format_verbose_rate_human(2_500_000_000.0);
    assert_eq!(value, "2.50");
    assert_eq!(unit, "GB/s");
}

#[test]
fn format_verbose_rate_human_tera_range() {
    let (value, unit) = format_verbose_rate_human(2_500_000_000_000.0);
    assert_eq!(value, "2.50");
    assert_eq!(unit, "TB/s");
}

#[test]
fn format_verbose_rate_human_peta_range() {
    let (value, unit) = format_verbose_rate_human(2_500_000_000_000_000.0);
    assert_eq!(value, "2.50");
    assert_eq!(unit, "PB/s");
}

// ============================================================================
// Progress line regex pattern validation
// ============================================================================

#[test]
fn progress_line_matches_upstream_pattern() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("pattern.txt");
    let destination = tmp.path().join("pattern.out");
    std::fs::write(&source, b"pattern test data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    let normalized = rendered.replace('\r', "\n");

    // Find the xfr line
    let xfr_line = normalized
        .lines()
        .find(|l| l.contains("xfr#"))
        .expect("should have xfr# line");

    // Validate the pattern components:
    // 1. Should contain a number (bytes transferred)
    assert!(
        xfr_line.chars().any(|c| c.is_ascii_digit()),
        "should contain digits (bytes): {xfr_line:?}"
    );

    // 2. Should contain a percentage
    assert!(
        xfr_line.contains('%'),
        "should contain percentage: {xfr_line:?}"
    );

    // 3. Should contain a rate with /s suffix
    assert!(
        xfr_line.contains("/s"),
        "should contain rate with /s: {xfr_line:?}"
    );

    // 4. Should contain elapsed time in H:MM:SS format
    assert!(
        xfr_line.contains(":00:0"),
        "should contain elapsed H:MM:SS: {xfr_line:?}"
    );

    // 5. Should contain the transfer tracking suffix
    assert!(
        xfr_line.contains("(xfr#1, to-chk=0/1)"),
        "should contain upstream tracking format: {xfr_line:?}"
    );
}

// ============================================================================
// --no-progress disables progress
// ============================================================================

#[test]
fn no_progress_suppresses_progress_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("noprog.txt");
    let destination = tmp.path().join("noprog.out");
    std::fs::write(&source, b"no progress data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-progress"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        !rendered.contains("xfr#"),
        "--no-progress should suppress xfr# lines: {rendered:?}"
    );
    assert!(
        !rendered.contains("to-chk="),
        "--no-progress should suppress to-chk= counters: {rendered:?}"
    );
}

// ============================================================================
// --progress --no-progress: last flag wins
// ============================================================================

#[test]
fn progress_then_no_progress_disables() {
    use crate::frontend::arguments::parse_args;
    use crate::frontend::progress::ProgressSetting;

    let args = ["rsync", "--progress", "--no-progress", "src/", "dst/"];
    let parsed = parse_args(args.iter().map(|s| s.to_string())).expect("parse");
    assert_eq!(
        parsed.progress,
        ProgressSetting::Disabled,
        "--progress --no-progress should disable progress"
    );
}

#[test]
fn no_progress_then_progress_enables() {
    use crate::frontend::arguments::parse_args;
    use crate::frontend::progress::ProgressSetting;

    let args = ["rsync", "--no-progress", "--progress", "src/", "dst/"];
    let parsed = parse_args(args.iter().map(|s| s.to_string())).expect("parse");
    assert_eq!(
        parsed.progress,
        ProgressSetting::PerFile,
        "--no-progress --progress should enable progress"
    );
}

// ============================================================================
// --info=progress2 sets Overall mode
// ============================================================================

#[test]
fn info_progress2_sets_overall_mode() {
    use crate::frontend::execution::parse_info_flags;
    use crate::frontend::progress::ProgressSetting;

    let flags = vec![OsString::from("progress2")];
    let settings = parse_info_flags(&flags).expect("parse info flags");
    assert_eq!(
        settings.progress,
        ProgressSetting::Overall,
        "--info=progress2 should set Overall mode"
    );
}

#[test]
fn info_progress1_sets_per_file_mode() {
    use crate::frontend::execution::parse_info_flags;
    use crate::frontend::progress::ProgressSetting;

    let flags = vec![OsString::from("progress")];
    let settings = parse_info_flags(&flags).expect("parse info flags");
    assert_eq!(
        settings.progress,
        ProgressSetting::PerFile,
        "--info=progress should set PerFile mode"
    );
}

#[test]
fn info_progress0_disables_progress() {
    use crate::frontend::execution::parse_info_flags;
    use crate::frontend::progress::ProgressSetting;

    let flags = vec![OsString::from("progress0")];
    let settings = parse_info_flags(&flags).expect("parse info flags");
    assert_eq!(
        settings.progress,
        ProgressSetting::Disabled,
        "--info=progress0 should disable progress"
    );
}

// ============================================================================
// File name is printed before progress line (per-file mode)
// ============================================================================

#[test]
fn progress_shows_filename_before_progress_line() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("fname_test.txt");
    let destination = tmp.path().join("fname_test.out");
    std::fs::write(&source, b"filename test").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    let normalized = rendered.replace('\r', "\n");

    // File name should appear before the xfr# line
    let fname_pos = normalized
        .find("fname_test.txt")
        .expect("should contain filename");
    let xfr_pos = normalized.find("xfr#1").expect("should contain xfr#1");
    assert!(
        fname_pos < xfr_pos,
        "filename should appear before xfr# line (fname_pos={fname_pos}, xfr_pos={xfr_pos})"
    );
}

// ============================================================================
// compute_rate helper tests
// ============================================================================

#[test]
fn compute_rate_returns_none_for_zero_duration() {
    use super::compute_rate;
    assert_eq!(compute_rate(1000, Duration::ZERO), None);
}

#[test]
fn compute_rate_returns_bytes_per_second() {
    use super::compute_rate;
    let rate = compute_rate(2000, Duration::from_secs(2)).unwrap();
    assert!((rate - 1000.0).abs() < 0.01);
}

#[test]
fn compute_rate_handles_fractional_seconds() {
    use super::compute_rate;
    let rate = compute_rate(500, Duration::from_millis(250)).unwrap();
    assert!((rate - 2000.0).abs() < 0.01);
}
