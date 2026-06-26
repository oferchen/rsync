use super::common::*;
use super::*;

#[test]
fn format_size_combined_uses_base_1024_no_exact() {
    // upstream: lib/compat.c:183 - `-hh` (Combined) divides by 1024 and never
    // appends an exact-value component: 1536 / 1024 = 1.50K.
    assert_eq!(format_size(1_536, HumanReadableMode::Combined), "1.50K");
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
fn format_progress_rate_combined_no_exact_component() {
    // upstream never appends an exact-value component to the progress rate.
    let rendered = format_progress_rate(
        1_048_576,
        Duration::from_secs(1),
        HumanReadableMode::Combined,
    );
    assert_eq!(rendered, "1.05MB/s");
}
