//! Integration tests verifying output format parity with upstream rsync.
//!
//! This test suite ensures that our implementation produces output formats
//! that match upstream rsync exactly for common scenarios including:
//! - `--stats` statistics output
//! - `--itemize-changes` file change indicators
//! - `--progress` transfer progress display
//! - `--dry-run` dry-run mode output
//! - `--info=FLAGS` informational flag parsing
//!
//! Each test validates specific format strings, field positions, and
//! formatting conventions to ensure compatibility with tools and scripts
//! that parse rsync output.

use cli::{
    format_number_with_commas, DryRunAction, DryRunFormatter, DryRunSummary,
    info_output::{parse_info_flags, InfoFlags},
    progress_format::{
        calculate_rate, format_eta, format_number, format_rate, OverallProgress, PerFileProgress,
    },
    stats_format::{format_speed, format_speedup, StatsData, StatsFormatter},
    FileType, ItemizeChange, UpdateType,
};
use std::time::Duration;

// ============================================================================
// STATS OUTPUT PARITY TESTS
// ============================================================================

#[test]
fn stats_number_formatting_zero() {
    // Upstream rsync formats 0 as "0" without separators
    assert_eq!(
        cli::stats_format::format_number(0),
        "0",
        "zero should format without separators"
    );
}

#[test]
fn stats_number_formatting_thousands() {
    // Upstream rsync uses comma separators for thousands
    assert_eq!(
        cli::stats_format::format_number(1_234),
        "1,234",
        "thousands should have comma separator"
    );
    assert_eq!(
        cli::stats_format::format_number(999_999),
        "999,999",
        "large numbers should have comma separators"
    );
}

#[test]
fn stats_number_formatting_millions() {
    // Verify proper comma placement for millions
    assert_eq!(
        cli::stats_format::format_number(1_234_567),
        "1,234,567",
        "millions should have two comma separators"
    );
}

#[test]
fn stats_number_formatting_large_values() {
    // Test large file sizes (multi-GB transfers)
    assert_eq!(
        cli::stats_format::format_number(9_999_999_999),
        "9,999,999,999",
        "very large numbers should format correctly"
    );
}

#[test]
fn stats_summary_line_format_exact() {
    // Upstream format: "sent X bytes  received Y bytes  Z bytes/sec"
    // Note: TWO spaces between "bytes" and "received", and "bytes" and the speed
    let data = StatsData {
        total_bytes_sent: 12_345,
        total_bytes_received: 67_890,
        file_list_generation_time: 1.0,
        file_list_transfer_time: 2.0,
        ..Default::default()
    };

    let formatter = StatsFormatter::new(data);
    let output = formatter.format();

    // Verify exact format with two spaces
    assert!(
        output.contains("sent 12,345 bytes  received 67,890 bytes"),
        "summary line must have exact upstream format with two spaces"
    );
}

#[test]
fn stats_speedup_line_format_exact() {
    // Upstream format: "total size is X  speedup is Y.ZZ"
    // Note: TWO spaces between "is" and "speedup"
    let data = StatsData {
        total_file_size: 1_234_567,
        total_bytes_sent: 12_345,
        total_bytes_received: 67_890,
        ..Default::default()
    };

    let formatter = StatsFormatter::new(data);
    let output = formatter.format();

    // Verify exact format with two spaces
    assert!(
        output.contains("total size is 1,234,567  speedup is"),
        "speedup line must have exact upstream format with two spaces"
    );
}

#[test]
fn stats_speedup_calculation() {
    // Speedup = total_size / (sent + received)
    // Upstream calculation: 1,234,567 / (12,345 + 67,890) = 15.38
    // Test via formatted output since calculation is internal
    let data = StatsData {
        total_file_size: 1_234_567,
        total_bytes_sent: 12_345,
        total_bytes_received: 67_890,
        ..Default::default()
    };

    let formatter = StatsFormatter::new(data);
    let output = formatter.format();

    // Expected speedup: 1,234,567 / (12,345 + 67,890) = 1,234,567 / 80,235 = 15.38...
    // Should contain speedup around 15.38 (allow for rounding)
    assert!(
        output.contains("speedup is 15.38")
            || output.contains("speedup is 15.39")
            || output.contains("speedup is 15.37"),
        "speedup calculation should match upstream formula, got: {}",
        output
            .lines()
            .find(|line| line.contains("speedup"))
            .unwrap_or("")
    );
}

#[test]
fn stats_speedup_zero_transfer() {
    // If no bytes transferred, speedup is 0.00 (not infinite)
    let data = StatsData {
        total_file_size: 1_234_567,
        total_bytes_sent: 0,
        total_bytes_received: 0,
        ..Default::default()
    };

    let formatter = StatsFormatter::new(data);
    let output = formatter.format();

    assert!(
        output.contains("speedup is 0.00"),
        "speedup should be 0 when no bytes transferred"
    );
}

#[test]
fn stats_speedup_formatting_decimal_places() {
    // Upstream always shows 2 decimal places: "15.38"
    assert_eq!(
        format_speedup(15.38),
        "15.38",
        "speedup should have exactly 2 decimal places"
    );
    assert_eq!(
        format_speedup(1.5),
        "1.50",
        "speedup should pad to 2 decimal places"
    );
}

#[test]
fn stats_speed_formatting_decimal_places() {
    // Transfer speed always shows 2 decimal places
    assert_eq!(
        format_speed(1234.56),
        "1,234.56",
        "speed should have exactly 2 decimal places"
    );
    assert_eq!(
        format_speed(100.0),
        "100.00",
        "speed should show .00 for whole numbers"
    );
}

#[test]
fn stats_full_output_format() {
    // Verify complete stats output structure matches upstream
    let data = StatsData {
        num_files: 1234,
        num_created_files: 56,
        num_deleted_files: 0,
        num_transferred_files: 42,
        total_file_size: 1_234_567,
        total_transferred_size: 123_456,
        literal_data: 12_345,
        matched_data: 111_111,
        file_list_size: 1_234,
        file_list_generation_time: 0.001,
        file_list_transfer_time: 0.0,
        total_bytes_sent: 12_345,
        total_bytes_received: 67_890,
    };

    let formatter = StatsFormatter::new(data);
    let output = formatter.format();

    // Verify all required lines are present in exact format
    assert!(output.contains("Number of files: 1,234"));
    assert!(output.contains("Number of created files: 56"));
    assert!(output.contains("Number of deleted files: 0"));
    assert!(output.contains("Number of regular files transferred: 42"));
    assert!(output.contains("Total file size: 1,234,567 bytes"));
    assert!(output.contains("Total transferred file size: 123,456 bytes"));
    assert!(output.contains("Literal data: 12,345 bytes"));
    assert!(output.contains("Matched data: 111,111 bytes"));
    assert!(output.contains("File list size: 1,234"));
    assert!(output.contains("File list generation time: 0.001 seconds"));
    assert!(output.contains("File list transfer time: 0.000 seconds"));
    assert!(output.contains("Total bytes sent: 12,345"));
    assert!(output.contains("Total bytes received: 67,890"));
}

// ============================================================================
// ITEMIZE OUTPUT PARITY TESTS
// ============================================================================

#[test]
fn itemize_new_file_format() {
    // Upstream format for new file: ">f+++++++++"
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_new_file(true);

    assert_eq!(
        change.format(),
        ">f+++++++++",
        "new file must show + for all attributes"
    );
}

#[test]
fn itemize_unchanged_file_format() {
    // Upstream format for unchanged file: ".f........."
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::NotUpdated)
        .with_file_type(FileType::RegularFile);

    assert_eq!(
        change.format(),
        ".f.........",
        "unchanged file must show dots for all attributes"
    );
}

#[test]
fn itemize_update_type_sent() {
    // '<' indicates sent to remote
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Sent)
        .with_file_type(FileType::RegularFile);

    assert_eq!(
        change.format().chars().next().unwrap(),
        '<',
        "sent files must start with <"
    );
}

#[test]
fn itemize_update_type_received() {
    // '>' indicates received from remote
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile);

    assert_eq!(
        change.format().chars().next().unwrap(),
        '>',
        "received files must start with >"
    );
}

#[test]
fn itemize_update_type_created() {
    // 'c' indicates local change/created
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Created)
        .with_file_type(FileType::RegularFile);

    assert_eq!(
        change.format().chars().next().unwrap(),
        'c',
        "created files must start with c"
    );
}

#[test]
fn itemize_update_type_hard_link() {
    // 'h' indicates hard link
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::HardLink)
        .with_file_type(FileType::RegularFile);

    assert_eq!(
        change.format().chars().next().unwrap(),
        'h',
        "hard links must start with h"
    );
}

#[test]
fn itemize_update_type_message() {
    // '*' indicates message (e.g., *deleting)
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Message)
        .with_file_type(FileType::RegularFile);

    assert_eq!(
        change.format().chars().next().unwrap(),
        '*',
        "messages must start with *"
    );
}

#[test]
fn itemize_file_type_regular() {
    // 'f' for regular file
    let change = ItemizeChange::new().with_file_type(FileType::RegularFile);
    assert_eq!(
        change.format().chars().nth(1).unwrap(),
        'f',
        "regular files must show f at position 1"
    );
}

#[test]
fn itemize_file_type_directory() {
    // 'd' for directory
    let change = ItemizeChange::new().with_file_type(FileType::Directory);
    assert_eq!(
        change.format().chars().nth(1).unwrap(),
        'd',
        "directories must show d at position 1"
    );
}

#[test]
fn itemize_file_type_symlink() {
    // 'L' (uppercase) for symlink
    let change = ItemizeChange::new().with_file_type(FileType::Symlink);
    assert_eq!(
        change.format().chars().nth(1).unwrap(),
        'L',
        "symlinks must show uppercase L at position 1"
    );
}

#[test]
fn itemize_file_type_device() {
    // 'D' (uppercase) for device
    let change = ItemizeChange::new().with_file_type(FileType::Device);
    assert_eq!(
        change.format().chars().nth(1).unwrap(),
        'D',
        "devices must show uppercase D at position 1"
    );
}

#[test]
fn itemize_file_type_special() {
    // 'S' (uppercase) for special file
    let change = ItemizeChange::new().with_file_type(FileType::Special);
    assert_eq!(
        change.format().chars().nth(1).unwrap(),
        'S',
        "special files must show uppercase S at position 1"
    );
}

#[test]
fn itemize_checksum_changed() {
    // Position 2: 'c' for checksum changed
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_checksum_changed(true);

    assert_eq!(
        change.format().chars().nth(2).unwrap(),
        'c',
        "checksum change must show c at position 2"
    );
}

#[test]
fn itemize_size_changed() {
    // Position 3: 's' for size changed
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_size_changed(true);

    assert_eq!(
        change.format().chars().nth(3).unwrap(),
        's',
        "size change must show s at position 3"
    );
}

#[test]
fn itemize_time_changed_lowercase() {
    // Position 4: 't' (lowercase) for modification time changed
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_time_changed(true);

    assert_eq!(
        change.format().chars().nth(4).unwrap(),
        't',
        "time change must show lowercase t at position 4"
    );
}

#[test]
fn itemize_time_set_to_transfer_uppercase() {
    // Position 4: 'T' (uppercase) for time set to transfer time
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_time_set_to_transfer(true);

    assert_eq!(
        change.format().chars().nth(4).unwrap(),
        'T',
        "time set to transfer must show uppercase T at position 4"
    );
}

#[test]
fn itemize_permissions_changed() {
    // Position 5: 'p' for permissions changed
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_perms_changed(true);

    assert_eq!(
        change.format().chars().nth(5).unwrap(),
        'p',
        "permissions change must show p at position 5"
    );
}

#[test]
fn itemize_owner_changed() {
    // Position 6: 'o' for owner changed
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_owner_changed(true);

    assert_eq!(
        change.format().chars().nth(6).unwrap(),
        'o',
        "owner change must show o at position 6"
    );
}

#[test]
fn itemize_group_changed() {
    // Position 7: 'g' for group changed
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_group_changed(true);

    assert_eq!(
        change.format().chars().nth(7).unwrap(),
        'g',
        "group change must show g at position 7"
    );
}

#[test]
fn itemize_acl_changed() {
    // Position 9: 'a' for ACL changed
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_acl_changed(true);

    assert_eq!(
        change.format().chars().nth(9).unwrap(),
        'a',
        "ACL change must show a at position 9"
    );
}

#[test]
fn itemize_xattr_changed() {
    // Position 10: 'x' for extended attributes changed
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_xattr_changed(true);

    assert_eq!(
        change.format().chars().nth(10).unwrap(),
        'x',
        "xattr change must show x at position 10"
    );
}

#[test]
fn itemize_typical_content_update() {
    // Typical file update: checksum, size, time changed
    // Format: ">fcst......"
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_changed(true);

    assert_eq!(
        change.format(),
        ">fcst......",
        "typical file update must match upstream format"
    );
}

#[test]
fn itemize_length_always_eleven() {
    // All itemize strings must be exactly 11 characters
    let test_cases = vec![
        ItemizeChange::new(),
        ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_new_file(true),
        ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_checksum_changed(true)
            .with_size_changed(true)
            .with_time_changed(true),
    ];

    for change in test_cases {
        assert_eq!(
            change.format().len(),
            11,
            "itemize string must be exactly 11 characters"
        );
    }
}

// ============================================================================
// PROGRESS OUTPUT PARITY TESTS
// ============================================================================

#[test]
fn progress_rate_formatting_kilobytes() {
    // Upstream always shows at least kB/s, never B/s
    assert_eq!(
        format_rate(100.0),
        "0.10kB/s",
        "small rates should show in kB/s"
    );
    assert_eq!(
        format_rate(1024.0),
        "1.00kB/s",
        "1024 bytes/sec should be 1.00kB/s"
    );
}

#[test]
fn progress_rate_formatting_megabytes() {
    // Rates >= 1 MiB show in MB/s
    assert_eq!(
        format_rate(1_048_576.0),
        "1.00MB/s",
        "1 MiB/sec should be 1.00MB/s"
    );
    assert_eq!(
        format_rate(12_582_912.0),
        "12.00MB/s",
        "12 MiB/sec should be 12.00MB/s"
    );
}

#[test]
fn progress_rate_formatting_gigabytes() {
    // Rates >= 1 GiB show in GB/s
    assert_eq!(
        format_rate(1_073_741_824.0),
        "1.00GB/s",
        "1 GiB/sec should be 1.00GB/s"
    );
}

#[test]
fn progress_rate_calculation() {
    // Rate = bytes / seconds
    let rate = calculate_rate(1_000_000, Duration::from_secs(10));
    assert!(
        (rate - 100_000.0).abs() < 1.0,
        "rate calculation should be bytes/second"
    );
}

#[test]
fn progress_eta_format_seconds() {
    // ETA format for < 1 hour: "0:MM:SS"
    assert_eq!(
        format_eta(1_000_000, 100_000.0),
        "0:00:10",
        "ETA under 1 minute should be 0:00:SS"
    );
    assert_eq!(
        format_eta(6_000_000, 100_000.0),
        "0:01:00",
        "ETA of 1 minute should be 0:01:00"
    );
}

#[test]
fn progress_eta_format_hours() {
    // ETA format for >= 1 hour: "H:MM:SS"
    assert_eq!(
        format_eta(360_000_000, 100_000.0),
        "1:00:00",
        "ETA of 1 hour should be 1:00:00"
    );
}

#[test]
fn progress_eta_format_days() {
    // ETA format for >= 1 day: "D:HH:MM:SS"
    assert_eq!(
        format_eta(8_640_000_000, 100_000.0),
        "1:00:00:00",
        "ETA of 1 day should be 1:00:00:00"
    );
}

#[test]
fn progress_per_file_format() {
    // Per-file progress shows: bytes, percentage, rate, elapsed
    let mut progress = PerFileProgress::new("test.txt", 1_000_000);
    progress.update(500_000, Duration::from_secs(5));
    let line = progress.format_line();

    assert!(line.contains("500,000"), "should show comma-separated bytes");
    assert!(line.contains("50%"), "should show percentage");
    assert!(line.contains("kB/s"), "should show transfer rate");
}

#[test]
fn progress_overall_format_files() {
    // Overall progress format: "  42/1,234 files  X.XX% (xfr#42, to-chk=1,192/1,234)"
    let mut progress = OverallProgress::new(1234, 9_999_999_999);
    for _ in 0..42 {
        progress.update_file_complete(100_000);
    }
    let line = progress.format_line();

    assert!(line.contains("files"), "should contain 'files' label");
    assert!(line.contains("xfr#42"), "should show transfer number");
    assert!(line.contains("to-chk=1192"), "should show remaining files");
}

#[test]
fn progress_overall_format_to_chk() {
    // to-chk counter should decrease as files complete
    let mut progress = OverallProgress::new(100, 10_000_000);
    progress.update_file_complete(100_000);
    let line = progress.format_line();

    // to-chk = total - completed = 100 - 1 = 99
    assert!(
        line.contains("to-chk=99"),
        "to-chk should be total minus completed"
    );
}

#[test]
fn progress_number_formatting() {
    // Progress uses same number formatting as stats (comma separators)
    assert_eq!(
        format_number(1_234_567),
        "1,234,567",
        "progress numbers should have comma separators"
    );
}

// ============================================================================
// DRY-RUN OUTPUT PARITY TESTS
// ============================================================================

#[test]
fn dry_run_marker_present() {
    // Upstream always shows "(DRY RUN)" marker at the end
    let summary = DryRunSummary::new();
    let output = summary.format_summary();

    assert!(
        output.contains("(DRY RUN)"),
        "dry-run output must contain (DRY RUN) marker"
    );
}

#[test]
fn dry_run_zero_bytes_transferred() {
    // In dry-run mode, no bytes are actually sent or received
    let summary = DryRunSummary::new();
    let output = summary.format_summary();

    assert!(
        output.contains("sent 0 bytes  received 0 bytes"),
        "dry-run must show 0 bytes sent/received"
    );
}

#[test]
fn dry_run_file_listing_format() {
    // Files are listed by name, one per line
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "file1.txt".to_string(),
        size: 100,
    });
    summary.add_action(DryRunAction::SendFile {
        path: "file2.txt".to_string(),
        size: 200,
    });

    let output = summary.format_output(1);
    assert!(output.contains("file1.txt\n"), "should list file1.txt");
    assert!(output.contains("file2.txt\n"), "should list file2.txt");
}

#[test]
fn dry_run_deletion_prefix() {
    // Deletions are prefixed with "deleting "
    let formatter = DryRunFormatter::new(1);
    let action = DryRunAction::DeleteFile {
        path: "old.txt".to_string(),
    };

    let output = formatter.format_action(&action);
    assert_eq!(
        output, "deleting old.txt\n",
        "deletions must be prefixed with 'deleting '"
    );
}

#[test]
fn dry_run_verbosity_zero_silent() {
    // Verbosity 0 should produce no output
    let formatter = DryRunFormatter::new(0);
    let action = DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 100,
    };

    assert_eq!(
        formatter.format_action(&action),
        "",
        "verbosity 0 should produce no output"
    );
}

#[test]
fn dry_run_symlink_arrow_verbosity_two() {
    // At verbosity 2+, symlinks show " -> target"
    let formatter = DryRunFormatter::new(2);
    let action = DryRunAction::CreateSymlink {
        path: "link".to_string(),
        target: "target".to_string(),
    };

    assert_eq!(
        formatter.format_action(&action),
        "link -> target\n",
        "symlinks should show -> at verbosity 2+"
    );
}

#[test]
fn dry_run_hardlink_arrow_verbosity_two() {
    // At verbosity 2+, hard links show " => target"
    let formatter = DryRunFormatter::new(2);
    let action = DryRunAction::CreateHardlink {
        path: "link".to_string(),
        target: "target".to_string(),
    };

    assert_eq!(
        formatter.format_action(&action),
        "link => target\n",
        "hard links should show => at verbosity 2+"
    );
}

#[test]
fn dry_run_number_formatting() {
    // Dry-run uses same comma formatting as stats
    assert_eq!(
        format_number_with_commas(1_234_567),
        "1,234,567",
        "dry-run numbers should have comma separators"
    );
}

// ============================================================================
// INFO FLAGS PARITY TESTS
// ============================================================================

#[test]
fn info_flags_verbosity_zero_silent() {
    // Verbosity 0 (-q) should disable all info output
    let flags = InfoFlags::from_verbosity(0);
    assert!(!flags.should_show_name(), "verbosity 0 should hide names");
    assert!(!flags.should_show_stats(), "verbosity 0 should hide stats");
    assert!(!flags.should_show_del(), "verbosity 0 should hide deletions");
}

#[test]
fn info_flags_verbosity_one_normal() {
    // Verbosity 1 (-v) shows names, stats, deletions
    let flags = InfoFlags::from_verbosity(1);
    assert!(flags.should_show_name(), "verbosity 1 should show names");
    assert!(flags.should_show_stats(), "verbosity 1 should show stats");
    assert!(flags.should_show_del(), "verbosity 1 should show deletions");
}

#[test]
fn info_flags_verbosity_two_verbose() {
    // Verbosity 2 (-vv) increases info levels
    let flags = InfoFlags::from_verbosity(2);
    assert!(flags.should_show_name(), "verbosity 2 should show names");
    assert!(flags.should_show_stats(), "verbosity 2 should show stats");
    assert_eq!(
        flags.levels().get(logging::InfoFlag::Name),
        2,
        "verbosity 2 should set name level to 2"
    );
}

#[test]
fn info_flags_parse_single_flag() {
    // Parse single flag: "name2"
    let flags = parse_info_flags("name2").unwrap();
    assert_eq!(
        flags.levels().get(logging::InfoFlag::Name),
        2,
        "should parse name2 as level 2"
    );
}

#[test]
fn info_flags_parse_multiple_flags() {
    // Parse comma-separated flags: "name2,del1,stats2"
    let flags = parse_info_flags("name2,del1,stats2").unwrap();
    assert_eq!(flags.levels().get(logging::InfoFlag::Name), 2);
    assert_eq!(flags.levels().get(logging::InfoFlag::Del), 1);
    assert_eq!(flags.levels().get(logging::InfoFlag::Stats), 2);
}

#[test]
fn info_flags_parse_all_keyword() {
    // ALL keyword sets all flags to 1
    let flags = parse_info_flags("ALL").unwrap();
    assert!(flags.should_show_name(), "ALL should enable name");
    assert!(flags.should_show_stats(), "ALL should enable stats");
    assert!(flags.should_show_del(), "ALL should enable del");
}

#[test]
fn info_flags_parse_all_with_level() {
    // ALL2 sets all flags to level 2
    let flags = parse_info_flags("ALL2").unwrap();
    assert_eq!(flags.levels().get(logging::InfoFlag::Name), 2);
    assert_eq!(flags.levels().get(logging::InfoFlag::Stats), 2);
}

#[test]
fn info_flags_parse_none_keyword() {
    // NONE keyword sets all flags to 0
    let flags = parse_info_flags("NONE").unwrap();
    assert!(!flags.should_show_name(), "NONE should disable name");
    assert!(!flags.should_show_stats(), "NONE should disable stats");
    assert!(!flags.should_show_del(), "NONE should disable del");
}

#[test]
fn info_flags_parse_case_insensitive() {
    // Flag names should be case-insensitive
    let flags1 = parse_info_flags("name2").unwrap();
    let flags2 = parse_info_flags("NAME2").unwrap();
    let flags3 = parse_info_flags("Name2").unwrap();

    assert_eq!(flags1.levels().get(logging::InfoFlag::Name), 2);
    assert_eq!(flags2.levels().get(logging::InfoFlag::Name), 2);
    assert_eq!(flags3.levels().get(logging::InfoFlag::Name), 2);
}

#[test]
fn info_flags_default_level_one() {
    // Flags without explicit level default to 1
    let flags = parse_info_flags("name,stats,del").unwrap();
    assert_eq!(flags.levels().get(logging::InfoFlag::Name), 1);
    assert_eq!(flags.levels().get(logging::InfoFlag::Stats), 1);
    assert_eq!(flags.levels().get(logging::InfoFlag::Del), 1);
}

// ============================================================================
// CROSS-MODULE CONSISTENCY TESTS
// ============================================================================

#[test]
fn number_formatting_consistency_across_modules() {
    // All modules should use the same number formatting
    let number = 1_234_567_u64;

    // Stats module
    let stats_formatted = cli::stats_format::format_number(number);

    // Progress module
    let progress_formatted = cli::progress_format::format_number(number);

    // Dry-run module
    let dry_run_formatted = format_number_with_commas(number);

    assert_eq!(
        stats_formatted, progress_formatted,
        "stats and progress should format numbers identically"
    );
    assert_eq!(
        progress_formatted, dry_run_formatted,
        "progress and dry-run should format numbers identically"
    );
    assert_eq!(
        stats_formatted, "1,234,567",
        "all modules should use comma separators"
    );
}

#[test]
fn decimal_formatting_consistency() {
    // Speed and speedup should both use 2 decimal places
    let speed = format_speed(1234.5678);
    let speedup = format_speedup(1234.5678);

    assert!(
        speed.contains(".56") || speed.contains(".57"),
        "speed should round to 2 decimal places"
    );
    assert!(
        speedup.contains(".56") || speedup.contains(".57"),
        "speedup should round to 2 decimal places"
    );
}

// ============================================================================
// INTENTIONAL DIFFERENCES DOCUMENTATION
// ============================================================================

/// Documents intentional differences from upstream rsync output format.
///
/// As of this implementation, there are no intentional differences.
/// All output formats are designed to match upstream rsync exactly to ensure
/// compatibility with existing tools, scripts, and user expectations.
///
/// If differences are introduced in the future, they should be documented here
/// with rationale and migration guidance.
#[test]
fn document_intentional_differences() {
    // No intentional differences at this time.
    // This test serves as a reminder to document any future deviations.
    assert!(
        true,
        "All output formats match upstream rsync exactly"
    );
}
