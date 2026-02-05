//! Tests for timestamp handling at the year 2038 boundary.
//!
//! The Unix epoch timestamp stored as a 32-bit signed integer overflows on
//! January 19, 2038, at 03:14:07 UTC (the "Year 2038 problem"). This test
//! suite verifies that the metadata crate correctly handles timestamps beyond
//! this boundary using 64-bit signed integers.
//!
//! Test coverage:
//! - Files with timestamps beyond 2038 are handled correctly
//! - 64-bit timestamp support works for both positive and negative values
//! - Round-trip preserves timestamps after 2038
//! - No overflow errors occur at the boundary

use filetime::{FileTime, set_file_times};
use metadata::{apply_file_metadata, apply_directory_metadata, apply_symlink_metadata};
use std::fs;
use tempfile::tempdir;

/// The Year 2038 overflow boundary for 32-bit signed Unix timestamps.
/// This is 2^31 - 1 = 2,147,483,647 seconds since Unix epoch.
/// Corresponds to: 2038-01-19 03:14:07 UTC
const YEAR_2038_BOUNDARY: i64 = 2_147_483_647;

/// A timestamp just before the 2038 boundary (one day before).
const BEFORE_2038: i64 = YEAR_2038_BOUNDARY - 86_400;

/// A timestamp just after the 2038 boundary (one day after).
const AFTER_2038: i64 = YEAR_2038_BOUNDARY + 86_400;

/// A timestamp far in the future (year 2100).
/// Corresponds to: 2100-01-01 00:00:00 UTC
const YEAR_2100: i64 = 4_102_444_800;

/// A timestamp far in the future (year 3000).
/// Corresponds to: 3000-01-01 00:00:00 UTC
const YEAR_3000: i64 = 32_503_680_000;

#[test]
fn file_metadata_preserves_timestamp_at_2038_boundary() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"2038 boundary test").expect("write source");
    fs::write(&dest, b"2038 boundary test").expect("write dest");

    // Set timestamp to exactly the 2038 boundary with nanoseconds
    let atime = FileTime::from_unix_time(YEAR_2038_BOUNDARY, 500_000_000);
    let mtime = FileTime::from_unix_time(YEAR_2038_BOUNDARY, 999_999_999);
    set_file_times(&source, atime, mtime).expect("set source times");

    let metadata = fs::metadata(&source).expect("source metadata");
    apply_file_metadata(&dest, &metadata).expect("apply file metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(dest_atime, atime, "atime should be preserved at 2038 boundary");
    assert_eq!(dest_mtime, mtime, "mtime should be preserved at 2038 boundary");
}

#[test]
fn file_metadata_preserves_timestamp_before_2038() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"before 2038").expect("write source");
    fs::write(&dest, b"before 2038").expect("write dest");

    // One day before the boundary
    let atime = FileTime::from_unix_time(BEFORE_2038, 100_000_000);
    let mtime = FileTime::from_unix_time(BEFORE_2038, 200_000_000);
    set_file_times(&source, atime, mtime).expect("set source times");

    let metadata = fs::metadata(&source).expect("source metadata");
    apply_file_metadata(&dest, &metadata).expect("apply file metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(dest_atime, atime, "atime should be preserved before 2038");
    assert_eq!(dest_mtime, mtime, "mtime should be preserved before 2038");
}

#[test]
fn file_metadata_preserves_timestamp_after_2038() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"after 2038").expect("write source");
    fs::write(&dest, b"after 2038").expect("write dest");

    // One day after the boundary
    let atime = FileTime::from_unix_time(AFTER_2038, 333_000_000);
    let mtime = FileTime::from_unix_time(AFTER_2038, 444_000_000);
    set_file_times(&source, atime, mtime).expect("set source times");

    let metadata = fs::metadata(&source).expect("source metadata");
    apply_file_metadata(&dest, &metadata).expect("apply file metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(dest_atime, atime, "atime should be preserved after 2038");
    assert_eq!(dest_mtime, mtime, "mtime should be preserved after 2038");
}

#[test]
fn file_metadata_preserves_timestamp_year_2100() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"year 2100").expect("write source");
    fs::write(&dest, b"year 2100").expect("write dest");

    // Year 2100 timestamp
    let atime = FileTime::from_unix_time(YEAR_2100, 0);
    let mtime = FileTime::from_unix_time(YEAR_2100, 123_456_789);
    set_file_times(&source, atime, mtime).expect("set source times");

    let metadata = fs::metadata(&source).expect("source metadata");
    apply_file_metadata(&dest, &metadata).expect("apply file metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(dest_atime, atime, "atime should be preserved for year 2100");
    assert_eq!(dest_mtime, mtime, "mtime should be preserved for year 2100");
}

#[test]
fn file_metadata_preserves_timestamp_year_3000() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"year 3000").expect("write source");
    fs::write(&dest, b"year 3000").expect("write dest");

    // Year 3000 timestamp (far future)
    let atime = FileTime::from_unix_time(YEAR_3000, 555_000_000);
    let mtime = FileTime::from_unix_time(YEAR_3000, 666_000_000);
    set_file_times(&source, atime, mtime).expect("set source times");

    let metadata = fs::metadata(&source).expect("source metadata");
    apply_file_metadata(&dest, &metadata).expect("apply file metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(dest_atime, atime, "atime should be preserved for year 3000");
    assert_eq!(dest_mtime, mtime, "mtime should be preserved for year 3000");
}

#[test]
fn directory_metadata_preserves_timestamp_beyond_2038() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source_dir");
    let dest = temp.path().join("dest_dir");
    fs::create_dir(&source).expect("create source dir");
    fs::create_dir(&dest).expect("create dest dir");

    // Use timestamp beyond 2038
    let atime = FileTime::from_unix_time(YEAR_2100, 111_222_333);
    let mtime = FileTime::from_unix_time(YEAR_2100, 444_555_666);
    set_file_times(&source, atime, mtime).expect("set source times");

    let metadata = fs::metadata(&source).expect("source metadata");
    apply_directory_metadata(&dest, &metadata).expect("apply directory metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(dest_atime, atime, "directory atime should be preserved beyond 2038");
    assert_eq!(dest_mtime, mtime, "directory mtime should be preserved beyond 2038");
}

#[cfg(unix)]
#[test]
fn symlink_metadata_preserves_timestamp_beyond_2038() {
    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    let source_link = temp.path().join("source_link");
    let dest_link = temp.path().join("dest_link");

    fs::write(&target, b"target").expect("write target");
    std::os::unix::fs::symlink(&target, &source_link).expect("create source symlink");
    std::os::unix::fs::symlink(&target, &dest_link).expect("create dest symlink");

    // Use timestamp beyond 2038
    let atime = FileTime::from_unix_time(AFTER_2038, 777_000_000);
    let mtime = FileTime::from_unix_time(AFTER_2038, 888_000_000);
    filetime::set_symlink_file_times(&source_link, atime, mtime).expect("set link times");

    let metadata = fs::symlink_metadata(&source_link).expect("source link metadata");
    apply_symlink_metadata(&dest_link, &metadata).expect("apply symlink metadata");

    let dest_meta = fs::symlink_metadata(&dest_link).expect("dest link metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(dest_atime, atime, "symlink atime should be preserved beyond 2038");
    assert_eq!(dest_mtime, mtime, "symlink mtime should be preserved beyond 2038");
}

#[test]
fn round_trip_preserves_timestamps_across_2038_boundary() {
    let temp = tempdir().expect("tempdir");

    // Test multiple round trips with timestamps spanning the 2038 boundary
    let test_cases = vec![
        ("before_2038.txt", BEFORE_2038, 100_000_000),
        ("at_2038.txt", YEAR_2038_BOUNDARY, 500_000_000),
        ("after_2038.txt", AFTER_2038, 900_000_000),
        ("year_2100.txt", YEAR_2100, 123_456_789),
    ];

    for (filename, timestamp, nsec) in test_cases {
        let file1 = temp.path().join(format!("1_{}", filename));
        let file2 = temp.path().join(format!("2_{}", filename));
        let file3 = temp.path().join(format!("3_{}", filename));

        fs::write(&file1, b"round trip test").expect("write file1");
        fs::write(&file2, b"round trip test").expect("write file2");
        fs::write(&file3, b"round trip test").expect("write file3");

        // Set timestamp on file1
        let original_time = FileTime::from_unix_time(timestamp, nsec);
        set_file_times(&file1, original_time, original_time).expect("set file1 times");

        // Apply to file2
        let meta1 = fs::metadata(&file1).expect("file1 metadata");
        apply_file_metadata(&file2, &meta1).expect("apply to file2");

        // Apply to file3
        let meta2 = fs::metadata(&file2).expect("file2 metadata");
        apply_file_metadata(&file3, &meta2).expect("apply to file3");

        // Verify file3 has the same timestamp as file1
        let meta3 = fs::metadata(&file3).expect("file3 metadata");
        let final_mtime = FileTime::from_last_modification_time(&meta3);

        assert_eq!(
            final_mtime, original_time,
            "Round trip should preserve timestamp for {} ({})",
            filename, timestamp
        );
    }
}

#[test]
fn no_overflow_at_i32_max_boundary() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"overflow test").expect("write source");
    fs::write(&dest, b"overflow test").expect("write dest");

    // Test exactly at i32::MAX (the overflow point)
    let critical_timestamp = FileTime::from_unix_time(i32::MAX as i64, 999_999_999);
    set_file_times(&source, critical_timestamp, critical_timestamp).expect("set times at i32::MAX");

    let metadata = fs::metadata(&source).expect("source metadata");
    let result = apply_file_metadata(&dest, &metadata);

    // Should not panic or produce an error
    assert!(result.is_ok(), "Should handle i32::MAX timestamp without overflow");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_mtime, critical_timestamp, "Timestamp at i32::MAX should be preserved");
}

#[test]
fn no_overflow_just_past_i32_max() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"overflow test").expect("write source");
    fs::write(&dest, b"overflow test").expect("write dest");

    // Test one second past i32::MAX
    let past_overflow = FileTime::from_unix_time((i32::MAX as i64) + 1, 0);
    set_file_times(&source, past_overflow, past_overflow).expect("set times past i32::MAX");

    let metadata = fs::metadata(&source).expect("source metadata");
    let result = apply_file_metadata(&dest, &metadata);

    assert!(result.is_ok(), "Should handle i32::MAX + 1 timestamp without overflow");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_mtime, past_overflow, "Timestamp past i32::MAX should be preserved");
}

#[test]
fn nanosecond_precision_preserved_beyond_2038() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"nanosecond precision").expect("write source");
    fs::write(&dest, b"nanosecond precision").expect("write dest");

    // Test various nanosecond values with timestamps beyond 2038
    let test_nanoseconds = vec![0, 1, 999_999_999, 123_456_789, 500_000_000];

    for nsec in test_nanoseconds {
        let time_with_nsec = FileTime::from_unix_time(YEAR_2100, nsec);
        set_file_times(&source, time_with_nsec, time_with_nsec).expect("set times");

        let metadata = fs::metadata(&source).expect("source metadata");
        apply_file_metadata(&dest, &metadata).expect("apply metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

        assert_eq!(
            dest_mtime, time_with_nsec,
            "Nanosecond precision should be preserved: {} ns at year 2100",
            nsec
        );
    }
}

#[cfg(unix)]
#[test]
fn file_entry_set_mtime_handles_64bit_timestamps() {
    use protocol::flist::FileEntry;
    use std::path::PathBuf;

    // Test that FileEntry correctly stores and retrieves 64-bit timestamps
    let test_cases = vec![
        (BEFORE_2038, 100_000_000),
        (YEAR_2038_BOUNDARY, 999_999_999),
        (AFTER_2038, 500_000_000),
        (YEAR_2100, 123_456_789),
        (YEAR_3000, 777_888_999),
    ];

    for (timestamp, nsec) in test_cases {
        let mut entry = FileEntry::new_file(PathBuf::from("test.txt"), 1024, 0o644);
        entry.set_mtime(timestamp, nsec);

        assert_eq!(
            entry.mtime(),
            timestamp,
            "FileEntry should store 64-bit timestamp: {}",
            timestamp
        );
        assert_eq!(
            entry.mtime_nsec(),
            nsec,
            "FileEntry should store nanoseconds: {}",
            nsec
        );
    }
}

#[cfg(unix)]
#[test]
fn apply_metadata_from_file_entry_handles_post_2038_timestamps() {
    use metadata::{apply_metadata_from_file_entry, MetadataOptions};
    use protocol::flist::FileEntry;
    use std::path::PathBuf;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("dest.txt");
    fs::write(&dest, b"file entry test").expect("write dest");

    // Create a FileEntry with a timestamp beyond 2038
    let mut entry = FileEntry::new_file(PathBuf::from("test.txt"), 1024, 0o644);
    entry.set_mtime(YEAR_2100, 123_456_789);

    // Apply the metadata from the entry
    let options = MetadataOptions::default();
    apply_metadata_from_file_entry(&dest, &entry, &options).expect("apply from entry");

    // Verify the timestamp was correctly applied
    let metadata = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    let expected_mtime = FileTime::from_unix_time(YEAR_2100, 123_456_789);

    assert_eq!(
        dest_mtime, expected_mtime,
        "apply_metadata_from_file_entry should handle timestamps beyond 2038"
    );
}

#[test]
fn negative_timestamps_are_handled_correctly() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"negative timestamp").expect("write source");
    fs::write(&dest, b"negative timestamp").expect("write dest");

    // Test negative timestamps (before 1970)
    // January 1, 1960: -315619200
    let timestamp_1960 = FileTime::from_unix_time(-315_619_200, 0);
    set_file_times(&source, timestamp_1960, timestamp_1960).expect("set negative times");

    let metadata = fs::metadata(&source).expect("source metadata");
    apply_file_metadata(&dest, &metadata).expect("apply metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(
        dest_mtime, timestamp_1960,
        "Negative timestamps (pre-1970) should be preserved"
    );
}

#[test]
fn extreme_range_64bit_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"extreme range").expect("write source");
    fs::write(&dest, b"extreme range").expect("write dest");

    // Test a very large positive timestamp (year ~2500)
    // Note: We avoid i64::MAX as some filesystems may not support it
    let year_2500: i64 = 16_725_225_600; // Approximately year 2500
    let large_timestamp = FileTime::from_unix_time(year_2500, 999_999_999);

    set_file_times(&source, large_timestamp, large_timestamp).expect("set extreme times");

    let metadata = fs::metadata(&source).expect("source metadata");
    let result = apply_file_metadata(&dest, &metadata);

    assert!(result.is_ok(), "Should handle extreme 64-bit timestamps");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_mtime, large_timestamp, "Extreme timestamp should be preserved");
}

#[cfg(unix)]
#[test]
fn metadata_options_respect_times_flag_post_2038() {
    use metadata::{apply_file_metadata_with_options, MetadataOptions};

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"options test").expect("write source");
    fs::write(&dest, b"options test").expect("write dest");

    // Set source to a time beyond 2038
    let future_time = FileTime::from_unix_time(YEAR_2100, 555_000_000);
    set_file_times(&source, future_time, future_time).expect("set future times");

    let metadata = fs::metadata(&source).expect("source metadata");

    // Apply with times disabled
    let options = MetadataOptions::new().preserve_times(false);
    apply_file_metadata_with_options(&dest, &metadata, &options).expect("apply without times");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    // Time should NOT match because times preservation was disabled
    assert_ne!(dest_mtime, future_time, "Times should not be preserved when flag is false");

    // Now apply with times enabled
    let options = MetadataOptions::new().preserve_times(true);
    apply_file_metadata_with_options(&dest, &metadata, &options).expect("apply with times");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    // Time should match now
    assert_eq!(dest_mtime, future_time, "Times should be preserved when flag is true");
}
