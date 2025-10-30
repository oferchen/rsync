use super::common::*;
use super::*;

#[test]
fn format_size_combined_includes_exact_component() {
    assert_eq!(
        format_size(1_536, HumanReadableMode::Combined),
        "1.54K (1,536)"
    );
}

#[test]
fn format_progress_rate_zero_bytes_matches_mode() {
    assert_eq!(
        format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Disabled),
        "0.00kB/s"
    );
    assert_eq!(
        format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Enabled),
        "0.00B/s"
    );
    assert_eq!(
        format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Combined),
        "0.00B/s"
    );
}

#[test]
fn format_progress_rate_combined_includes_decimal_component() {
    let rendered = format_progress_rate(
        1_048_576,
        Duration::from_secs(1),
        HumanReadableMode::Combined,
    );
    assert_eq!(rendered, "1.05MB/s (1.00MB/s)");
}
