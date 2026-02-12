//! Tests verifying that `LiveProgress` rendering works correctly during real transfers.
//!
//! These tests feed real `ClientProgressUpdate` events through a `LiveProgress`
//! writer (backed by a `Vec<u8>`) by wiring it as the observer of a genuine
//! `run_client_with_observer` invocation.  The rendered output is then validated
//! for expected format fields: file path, byte counts, percentage, rate, xfr#,
//! to-chk, and elapsed time.

use super::*;
use core::client::{
    ClientConfig, ClientEventKind, ClientProgressObserver, ClientProgressUpdate, HumanReadableMode,
    run_client_with_observer,
};
use std::io::Cursor;
use tempfile::TempDir;

use crate::frontend::progress::{LiveProgress, ProgressMode};

// ============================================================================
// Helper: run a real transfer with LiveProgress as the observer
// ============================================================================

/// Runs a transfer of `source_dir/` -> `dest_dir` through `LiveProgress`,
/// returning the rendered output and the summary.
fn run_with_live_progress(
    source_dir: &std::path::Path,
    dest_dir: &std::path::Path,
    mode: ProgressMode,
    human_readable: HumanReadableMode,
) -> (String, core::client::ClientSummary, bool) {
    let mut source_arg = source_dir.as_os_str().to_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_dir.as_os_str().to_os_string()])
        .mkpath(true)
        .progress(true)
        .stats(true)
        .force_event_collection(true)
        .build();

    let mut buffer: Vec<u8> = Vec::new();
    let rendered_flag;

    {
        let mut live = LiveProgress::new(&mut buffer, mode, human_readable);
        let summary =
            run_client_with_observer(config, Some(&mut live as &mut dyn ClientProgressObserver))
                .expect("transfer succeeds");
        rendered_flag = live.rendered();
        live.finish().expect("finish succeeds");
        let output = String::from_utf8(buffer).expect("output is valid UTF-8");
        (output, summary, rendered_flag)
    }
}

/// Creates a temp directory with a single source file of the given size.
fn setup_single_file(name: &str, size: usize) -> (TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    let file_path = source_dir.join(name);
    std::fs::write(&file_path, vec![0xABu8; size]).expect("write source file");
    (tmp, source_dir)
}

/// Creates a temp directory with multiple source files.
fn setup_multiple_files(files: &[(&str, usize)]) -> (TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    for (name, size) in files {
        let file_path = source_dir.join(name);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&file_path, vec![0xCDu8; *size]).expect("write source file");
    }
    (tmp, source_dir)
}

// ============================================================================
// LiveProgress PerFile mode tests
// ============================================================================

#[test]
fn live_progress_per_file_renders_file_path() {
    let (tmp, source_dir) = setup_single_file("hello.txt", 128);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, rendered) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );

    assert!(
        rendered,
        "rendered() should return true after receiving events"
    );
    assert!(
        output.contains("hello.txt"),
        "per-file mode should print the file name: {output:?}"
    );
}

#[test]
fn live_progress_per_file_renders_xfr_and_to_chk() {
    let (tmp, source_dir) = setup_single_file("single.bin", 64);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );

    assert!(
        output.contains("xfr#1"),
        "per-file output should contain xfr#1: {output:?}"
    );
    assert!(
        output.contains("to-chk=0/1"),
        "per-file output should contain to-chk=0/1 for single file: {output:?}"
    );
}

#[test]
fn live_progress_per_file_renders_percentage() {
    let (tmp, source_dir) = setup_single_file("pct.bin", 256);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );

    assert!(
        output.contains("100%"),
        "completed transfer should show 100%: {output:?}"
    );
}

#[test]
fn live_progress_per_file_renders_rate() {
    let (tmp, source_dir) = setup_single_file("rate.bin", 512);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );

    assert!(
        output.contains("B/s"),
        "per-file output should contain a transfer rate: {output:?}"
    );
}

#[test]
fn live_progress_per_file_renders_elapsed_time() {
    let (tmp, source_dir) = setup_single_file("elapsed.bin", 128);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );

    // Elapsed format is H:MM:SS; for a fast transfer it should start with "0:00:0"
    assert!(
        output.contains(":00:0"),
        "per-file output should contain elapsed time in H:MM:SS format: {output:?}"
    );
}

#[test]
fn live_progress_per_file_renders_byte_count() {
    let (tmp, source_dir) = setup_single_file("bytes.bin", 1_536);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );

    assert!(
        output.contains("1,536"),
        "per-file output should contain thousands-separated byte count: {output:?}"
    );
}

#[test]
fn live_progress_per_file_multiple_files_renders_all_xfr() {
    let files = [("a.txt", 32), ("b.txt", 64), ("c.txt", 128)];
    let (tmp, source_dir) = setup_multiple_files(&files);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );

    assert!(output.contains("xfr#1"), "should contain xfr#1: {output:?}");
    assert!(output.contains("xfr#2"), "should contain xfr#2: {output:?}");
    assert!(output.contains("xfr#3"), "should contain xfr#3: {output:?}");

    // Last transfer should show to-chk=0/N
    assert!(
        output.contains("to-chk=0/"),
        "last transfer should show to-chk=0/N: {output:?}"
    );
}

#[test]
fn live_progress_per_file_finish_completes_without_error() {
    let (tmp, source_dir) = setup_single_file("finish.bin", 64);
    let dest_dir = tmp.path().join("dest");

    let mut source_arg = source_dir.as_os_str().to_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_dir.as_os_str().to_os_string()])
        .mkpath(true)
        .progress(true)
        .stats(true)
        .force_event_collection(true)
        .build();

    let mut buffer: Vec<u8> = Vec::new();
    let mut live = LiveProgress::new(
        &mut buffer,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );
    let _summary =
        run_client_with_observer(config, Some(&mut live as &mut dyn ClientProgressObserver))
            .expect("transfer succeeds");

    // finish() should succeed without panic or error
    live.finish().expect("finish should complete without error");
}

// ============================================================================
// LiveProgress Overall mode tests
// ============================================================================

#[test]
fn live_progress_overall_renders_xfr_and_to_chk() {
    let (tmp, source_dir) = setup_single_file("overall.bin", 256);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, rendered) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::Overall,
        HumanReadableMode::Disabled,
    );

    assert!(rendered, "rendered() should return true in overall mode");
    assert!(
        output.contains("xfr#1"),
        "overall mode should contain xfr#: {output:?}"
    );
    assert!(
        output.contains("to-chk=0/1"),
        "overall mode should show to-chk counters: {output:?}"
    );
}

#[test]
fn live_progress_overall_does_not_print_filename() {
    let (tmp, source_dir) = setup_single_file("noname.bin", 128);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::Overall,
        HumanReadableMode::Disabled,
    );

    // Overall mode should NOT print the individual file name as a separate line
    // (unlike per-file mode which prints filename\n before the progress line)
    let lines: Vec<&str> = output.lines().collect();
    // In overall mode, no line should be just the filename
    let standalone_filename_lines: Vec<&&str> =
        lines.iter().filter(|l| l.trim() == "noname.bin").collect();
    assert!(
        standalone_filename_lines.is_empty(),
        "overall mode should not print standalone filename lines: {output:?}"
    );
}

#[test]
fn live_progress_overall_renders_percentage_and_rate() {
    let (tmp, source_dir) = setup_single_file("overall_pct.bin", 512);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::Overall,
        HumanReadableMode::Disabled,
    );

    assert!(
        output.contains('%'),
        "overall mode should show percentage: {output:?}"
    );
    assert!(
        output.contains("B/s"),
        "overall mode should show transfer rate: {output:?}"
    );
}

#[test]
fn live_progress_overall_multiple_files() {
    let files = [("x.bin", 64), ("y.bin", 64)];
    let (tmp, source_dir) = setup_multiple_files(&files);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::Overall,
        HumanReadableMode::Disabled,
    );

    assert!(
        output.contains("xfr#2"),
        "overall mode with 2 files should show xfr#2: {output:?}"
    );
    assert!(
        output.contains("to-chk=0/"),
        "final update should show to-chk=0/N: {output:?}"
    );
}

// ============================================================================
// LiveProgress human-readable mode tests
// ============================================================================

#[test]
fn live_progress_human_readable_formats_bytes() {
    let (tmp, source_dir) = setup_single_file("human.bin", 1_536);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Enabled,
    );

    // Human-readable mode should show K/M/G suffixes instead of thousands separators
    assert!(
        output.contains("1.54K") || output.contains("1.50K"),
        "human-readable mode should use K suffix for KB range: {output:?}"
    );
}

#[test]
fn live_progress_combined_human_readable_shows_both_formats() {
    let (tmp, source_dir) = setup_single_file("combined.bin", 1_536);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Combined,
    );

    // Combined mode should show both human-readable and decimal
    assert!(
        output.contains("1,536"),
        "combined mode should include decimal format: {output:?}"
    );
}

// ============================================================================
// LiveProgress rendered() state tests
// ============================================================================

#[test]
fn live_progress_rendered_returns_false_before_events() {
    let mut buffer: Vec<u8> = Vec::new();
    let live = LiveProgress::new(
        &mut buffer,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );
    assert!(
        !live.rendered(),
        "rendered() should be false before any events"
    );
    live.finish().expect("finish on empty progress");
}

#[test]
fn live_progress_rendered_returns_true_after_transfer() {
    let (tmp, source_dir) = setup_single_file("rendered.bin", 64);
    let dest_dir = tmp.path().join("dest");

    let (_output, _summary, rendered) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );

    assert!(
        rendered,
        "rendered() should be true after receiving progress events"
    );
}

// ============================================================================
// End-to-end: recording observer collects correct fields
// ============================================================================

#[derive(Default)]
struct RecordingObserver {
    updates: Vec<RecordedUpdate>,
}

struct RecordedUpdate {
    path: std::path::PathBuf,
    kind: ClientEventKind,
    is_final: bool,
    total_bytes: Option<u64>,
    overall_transferred: u64,
    index: usize,
    total: usize,
    remaining: usize,
    bytes_transferred: u64,
}

impl ClientProgressObserver for RecordingObserver {
    fn on_progress(&mut self, update: &ClientProgressUpdate) {
        self.updates.push(RecordedUpdate {
            path: update.event().relative_path().to_path_buf(),
            kind: update.event().kind().clone(),
            is_final: update.is_final(),
            total_bytes: update.total_bytes(),
            overall_transferred: update.overall_transferred(),
            index: update.index(),
            total: update.total(),
            remaining: update.remaining(),
            bytes_transferred: update.event().bytes_transferred(),
        });
    }
}

#[test]
fn e2e_progress_observer_receives_events_for_single_file() {
    let (tmp, source_dir) = setup_single_file("observed.bin", 2048);
    let dest_dir = tmp.path().join("dest");

    let mut source_arg = source_dir.as_os_str().to_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_dir.as_os_str().to_os_string()])
        .mkpath(true)
        .progress(true)
        .stats(true)
        .force_event_collection(true)
        .build();

    let mut observer = RecordingObserver::default();
    let _summary =
        run_client_with_observer(config, Some(&mut observer)).expect("transfer succeeds");

    assert!(
        !observer.updates.is_empty(),
        "observer should receive at least one progress event"
    );

    // Verify the final update
    let final_updates: Vec<&RecordedUpdate> =
        observer.updates.iter().filter(|u| u.is_final).collect();
    assert!(
        !final_updates.is_empty(),
        "should have at least one final update"
    );

    let last_final = final_updates.last().expect("at least one final");
    assert_eq!(
        last_final.remaining, 0,
        "final update should have zero remaining"
    );
    assert!(
        last_final.bytes_transferred > 0,
        "final update should report bytes transferred"
    );
    assert!(
        last_final.total_bytes.is_some(),
        "final update for data copy should have total_bytes"
    );
}

#[test]
fn e2e_progress_observer_receives_events_for_multiple_files() {
    let files = [("alpha.txt", 100), ("beta.txt", 200), ("gamma.txt", 300)];
    let (tmp, source_dir) = setup_multiple_files(&files);
    let dest_dir = tmp.path().join("dest");

    let mut source_arg = source_dir.as_os_str().to_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_dir.as_os_str().to_os_string()])
        .mkpath(true)
        .progress(true)
        .stats(true)
        .force_event_collection(true)
        .build();

    let mut observer = RecordingObserver::default();
    let _summary =
        run_client_with_observer(config, Some(&mut observer)).expect("transfer succeeds");

    let data_updates: Vec<&RecordedUpdate> = observer
        .updates
        .iter()
        .filter(|u| matches!(u.kind, ClientEventKind::DataCopied))
        .collect();

    assert!(
        data_updates.len() >= 3,
        "should have at least 3 data-copy events, got {}: {:?}",
        data_updates.len(),
        data_updates
            .iter()
            .map(|u| u.path.display().to_string())
            .collect::<Vec<_>>()
    );

    // Check that overall_transferred is non-decreasing
    let mut prev_transferred = 0u64;
    for update in &data_updates {
        assert!(
            update.overall_transferred >= prev_transferred,
            "overall_transferred should be non-decreasing: {} < {}",
            update.overall_transferred,
            prev_transferred
        );
        prev_transferred = update.overall_transferred;
    }

    // Check that index increments for final updates
    let final_indices: Vec<usize> = observer
        .updates
        .iter()
        .filter(|u| u.is_final && matches!(u.kind, ClientEventKind::DataCopied))
        .map(|u| u.index)
        .collect();
    assert!(
        final_indices.len() >= 3,
        "should have at least 3 final data events"
    );
    // Indices should be sequential
    for window in final_indices.windows(2) {
        assert!(
            window[1] > window[0],
            "indices should be strictly increasing: {} >= {}",
            window[1],
            window[0]
        );
    }
}

#[test]
fn e2e_progress_observer_reports_correct_total() {
    let files = [("one.bin", 50), ("two.bin", 50)];
    let (tmp, source_dir) = setup_multiple_files(&files);
    let dest_dir = tmp.path().join("dest");

    let mut source_arg = source_dir.as_os_str().to_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_dir.as_os_str().to_os_string()])
        .mkpath(true)
        .progress(true)
        .stats(true)
        .force_event_collection(true)
        .build();

    let mut observer = RecordingObserver::default();
    let _summary =
        run_client_with_observer(config, Some(&mut observer)).expect("transfer succeeds");

    let data_updates: Vec<&RecordedUpdate> = observer
        .updates
        .iter()
        .filter(|u| matches!(u.kind, ClientEventKind::DataCopied) && u.is_final)
        .collect();

    // All updates should agree on total
    let totals: std::collections::HashSet<usize> = data_updates.iter().map(|u| u.total).collect();
    assert_eq!(
        totals.len(),
        1,
        "all updates should report the same total, got {totals:?}",
    );

    let total = *totals.iter().next().unwrap();
    assert!(
        total >= 2,
        "total should reflect at least 2 data files, got {total}",
    );
}

// ============================================================================
// LiveProgress output format matches expected pattern
// ============================================================================

#[test]
fn live_progress_output_matches_upstream_format_pattern() {
    let (tmp, source_dir) = setup_single_file("pattern_check.dat", 2048);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );

    let normalized = output.replace('\r', "\n");
    let lines: Vec<&str> = normalized.lines().collect();

    // Find the xfr# line
    let xfr_line = lines
        .iter()
        .find(|l| l.contains("xfr#"))
        .expect("should contain xfr# progress line");

    // Validate format components:
    // 1. Contains digits (byte count)
    assert!(
        xfr_line.chars().any(|c| c.is_ascii_digit()),
        "should contain digits: {xfr_line:?}"
    );

    // 2. Contains percentage
    assert!(
        xfr_line.contains('%'),
        "should contain percentage: {xfr_line:?}"
    );

    // 3. Contains rate with /s
    assert!(xfr_line.contains("/s"), "should contain rate: {xfr_line:?}");

    // 4. Contains elapsed time H:MM:SS
    assert!(
        xfr_line.contains(":00:0"),
        "should contain elapsed time: {xfr_line:?}"
    );

    // 5. Contains tracking suffix
    assert!(
        xfr_line.contains("(xfr#1, to-chk=0/1)"),
        "should contain tracking suffix: {xfr_line:?}"
    );
}

#[test]
fn live_progress_per_file_prints_filename_before_progress_line() {
    let (tmp, source_dir) = setup_single_file("order_test.txt", 128);
    let dest_dir = tmp.path().join("dest");

    let (output, _summary, _) = run_with_live_progress(
        &source_dir,
        &dest_dir,
        ProgressMode::PerFile,
        HumanReadableMode::Disabled,
    );

    let normalized = output.replace('\r', "\n");

    // File name should appear before the xfr# line
    let fname_pos = normalized
        .find("order_test.txt")
        .expect("should contain filename");
    let xfr_pos = normalized.find("xfr#1").expect("should contain xfr#1");
    assert!(
        fname_pos < xfr_pos,
        "filename should appear before xfr# line (fname_pos={fname_pos}, xfr_pos={xfr_pos})"
    );
}

// ============================================================================
// Additional format_progress_bytes tests
// ============================================================================

#[test]
fn format_progress_bytes_zero_disabled() {
    assert_eq!(format_progress_bytes(0, HumanReadableMode::Disabled), "0");
}

#[test]
fn format_progress_bytes_small_disabled() {
    assert_eq!(format_progress_bytes(42, HumanReadableMode::Disabled), "42");
}

#[test]
fn format_progress_bytes_thousands_disabled() {
    assert_eq!(
        format_progress_bytes(1_234, HumanReadableMode::Disabled),
        "1,234"
    );
}

#[test]
fn format_progress_bytes_millions_disabled() {
    assert_eq!(
        format_progress_bytes(1_234_567, HumanReadableMode::Disabled),
        "1,234,567"
    );
}

#[test]
fn format_progress_bytes_gigabytes_disabled() {
    assert_eq!(
        format_progress_bytes(1_234_567_890, HumanReadableMode::Disabled),
        "1,234,567,890"
    );
}

#[test]
fn format_progress_bytes_zero_human() {
    assert_eq!(format_progress_bytes(0, HumanReadableMode::Enabled), "0");
}

#[test]
fn format_progress_bytes_kilo_human() {
    assert_eq!(
        format_progress_bytes(2_500, HumanReadableMode::Enabled),
        "2.50K"
    );
}

#[test]
fn format_progress_bytes_mega_human() {
    assert_eq!(
        format_progress_bytes(5_000_000, HumanReadableMode::Enabled),
        "5.00M"
    );
}

#[test]
fn format_progress_bytes_giga_human() {
    assert_eq!(
        format_progress_bytes(3_000_000_000, HumanReadableMode::Enabled),
        "3.00G"
    );
}

// ============================================================================
// Additional format_progress_rate tests
// ============================================================================

#[test]
fn format_progress_rate_nonzero_bytes_nonzero_elapsed() {
    let rate = format_progress_rate(
        1_048_576,
        Duration::from_secs(1),
        HumanReadableMode::Disabled,
    );
    assert!(
        rate.contains("MB/s"),
        "1MB in 1s should show MB/s: {rate:?}"
    );
}

#[test]
fn format_progress_rate_large_bytes_short_duration() {
    let rate = format_progress_rate(
        10_737_418_240,
        Duration::from_secs(10),
        HumanReadableMode::Disabled,
    );
    assert!(rate.contains("GB/s"), "~1GB/s should show GB/s: {rate:?}");
}

#[test]
fn format_progress_rate_human_nonzero() {
    let rate = format_progress_rate(5_000, Duration::from_secs(1), HumanReadableMode::Enabled);
    assert!(
        rate.contains("kB/s"),
        "5000 B/s in human mode should show kB/s: {rate:?}"
    );
}

// ============================================================================
// Additional format_progress_percent edge case tests
// ============================================================================

#[test]
fn format_progress_percent_small_fraction() {
    // 1 byte out of 10000 -> 0%
    assert_eq!(format_progress_percent(1, Some(10000)), "0%");
}

#[test]
fn format_progress_percent_very_large_values() {
    // 500GB out of 1TB
    assert_eq!(
        format_progress_percent(500_000_000_000, Some(1_000_000_000_000)),
        "50%"
    );
}

#[test]
fn format_progress_percent_equal_to_total() {
    assert_eq!(format_progress_percent(42, Some(42)), "100%");
}

// ============================================================================
// Additional format_progress_elapsed tests
// ============================================================================

#[test]
fn format_progress_elapsed_ten_hours() {
    assert_eq!(
        format_progress_elapsed(Duration::from_secs(36_000)),
        "10:00:00"
    );
}

#[test]
fn format_progress_elapsed_mixed() {
    // 2 hours, 30 minutes, 15 seconds = 9015
    assert_eq!(
        format_progress_elapsed(Duration::from_secs(9_015)),
        "2:30:15"
    );
}

#[test]
fn format_progress_elapsed_just_under_minute() {
    assert_eq!(format_progress_elapsed(Duration::from_secs(59)), "0:00:59");
}

#[test]
fn format_progress_elapsed_millis_truncated() {
    // 2.999 seconds should show as 0:00:02
    assert_eq!(
        format_progress_elapsed(Duration::from_millis(2_999)),
        "0:00:02"
    );
}
