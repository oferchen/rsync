use super::common::*;
use super::*;

#[test]
fn format_size_combined_uses_base_1024_no_exact() {
    // upstream: lib/compat.c:183 - `-hh` (Combined) divides by 1024 and never
    // appends an exact-value component: 1536 / 1024 = 1.50K.
    assert_eq!(format_size(1_536, HumanReadableMode::BinaryUnits), "1.50K");
}

#[test]
fn format_progress_rate_zero_bytes_stays_kb_per_sec_in_every_mode() {
    // upstream: progress.c:108-116 - the progress rate is always base-1024 and
    // never honours `-h`/`-hh`; a zero rate falls into the `kB/s` tier in every
    // human-readable mode.
    for mode in [
        HumanReadableMode::Grouped,
        HumanReadableMode::DecimalUnits,
        HumanReadableMode::BinaryUnits,
    ] {
        assert_eq!(
            format_progress_rate(0, Duration::from_secs(1), mode),
            "0.00kB/s"
        );
    }
}

#[test]
fn format_progress_rate_is_base_1024_regardless_of_mode() {
    // upstream: progress.c:108-116 - 1 MiB/s renders as `1.00MB/s` with base-1024
    // divisors, never the base-1000 `1.05MB/s`, in every human-readable mode.
    for mode in [
        HumanReadableMode::Grouped,
        HumanReadableMode::DecimalUnits,
        HumanReadableMode::BinaryUnits,
    ] {
        assert_eq!(
            format_progress_rate(1_048_576, Duration::from_secs(1), mode),
            "1.00MB/s"
        );
    }
}
