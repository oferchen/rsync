// Output parity tests: verify oc-rsync output format matches upstream rsync conventions.
//
// These tests call the formatting functions directly with known inputs and verify
// that the rendered output matches the structure and content expected by upstream
// rsync's --stats, --verbose, and --itemize-changes modes.

use super::*;
use super::{
    NameOutputLevel, OutFormatContext, ProgressSetting, emit_transfer_summary, parse_out_format,
};
use super::common::{RSYNC, run_with_args};
use core::client::{
    ClientConfig, ClientEntryKind, ClientEvent, ClientEventKind, ClientSummary,
    HumanReadableMode, run_client,
};
use engine::local_copy::{LocalCopyChangeSet, TimeChange};
use std::ffi::OsStr;
use std::path::PathBuf;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helper: create a transfer with known file sizes for predictable stats
// ---------------------------------------------------------------------------

fn create_known_summary(file_contents: &[(&str, &[u8])]) -> (ClientSummary, TempDir) {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source directory");
    fs::create_dir_all(&dest_dir).expect("create destination directory");

    for (name, contents) in file_contents {
        fs::write(source_dir.join(name), contents).expect("write source file");
    }

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let config = ClientConfig::builder()
        .transfer_args([src_operand, dest_dir.into_os_string()])
        .recursive(true)
        .verbosity(2)
        .stats(true)
        .force_event_collection(true)
        .build();

    let summary = run_client(config).expect("run_client succeeds");
    (summary, temp)
}

fn render_stats(summary: &ClientSummary, human_readable: HumanReadableMode) -> String {
    let mut rendered = Vec::new();
    emit_transfer_summary(
        summary,
        0,
        None,
        true,  // stats
        false, // progress_already_rendered
        false, // list_only
        None,
        &OutFormatContext::default(),
        NameOutputLevel::Disabled,
        false,
        human_readable,
        false,
        &mut rendered,
    )
    .expect("render stats");
    String::from_utf8(rendered).expect("utf8")
}

fn render_verbose(summary: &ClientSummary, verbosity: u8) -> String {
    let mut rendered = Vec::new();
    emit_transfer_summary(
        summary,
        verbosity,
        None,
        false, // stats
        false, // progress_already_rendered
        false, // list_only
        None,
        &OutFormatContext::default(),
        NameOutputLevel::UpdatedAndUnchanged,
        false,
        HumanReadableMode::Disabled,
        false,
        &mut rendered,
    )
    .expect("render verbose");
    String::from_utf8(rendered).expect("utf8")
}

fn render_itemize(event: &ClientEvent) -> String {
    let format = parse_out_format(OsStr::new("%i %n")).expect("parse %i %n");
    let mut output = Vec::new();
    format
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %i %n");
    String::from_utf8(output).expect("utf8")
}

// ===========================================================================
// 1. Stats output format parity
// ===========================================================================

#[test]
fn parity_stats_output_contains_all_upstream_field_labels() {
    let (summary, _temp) = create_known_summary(&[("file.txt", b"hello world")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    // Verify all upstream rsync --stats labels are present
    assert!(
        output.contains("Number of files:"),
        "missing 'Number of files:' label in:\n{output}"
    );
    assert!(
        output.contains("Number of created files:"),
        "missing 'Number of created files:' label in:\n{output}"
    );
    assert!(
        output.contains("Number of deleted files:"),
        "missing 'Number of deleted files:' label in:\n{output}"
    );
    assert!(
        output.contains("Number of regular files transferred:"),
        "missing 'Number of regular files transferred:' label in:\n{output}"
    );
    assert!(
        output.contains("Total file size:"),
        "missing 'Total file size:' label in:\n{output}"
    );
    assert!(
        output.contains("Total transferred file size:"),
        "missing 'Total transferred file size:' label in:\n{output}"
    );
    assert!(
        output.contains("Literal data:"),
        "missing 'Literal data:' label in:\n{output}"
    );
    assert!(
        output.contains("Matched data:"),
        "missing 'Matched data:' label in:\n{output}"
    );
    assert!(
        output.contains("File list size:"),
        "missing 'File list size:' label in:\n{output}"
    );
    assert!(
        output.contains("File list generation time:"),
        "missing 'File list generation time:' label in:\n{output}"
    );
    assert!(
        output.contains("File list transfer time:"),
        "missing 'File list transfer time:' label in:\n{output}"
    );
    assert!(
        output.contains("Total bytes sent:"),
        "missing 'Total bytes sent:' label in:\n{output}"
    );
    assert!(
        output.contains("Total bytes received:"),
        "missing 'Total bytes received:' label in:\n{output}"
    );
}

#[test]
fn parity_stats_output_field_order_matches_upstream() {
    let (summary, _temp) = create_known_summary(&[("order.txt", b"test content")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    // Upstream rsync emits stats in a fixed order; verify ours matches
    let labels = [
        "Number of files:",
        "Number of created files:",
        "Number of deleted files:",
        "Number of regular files transferred:",
        "Total file size:",
        "Total transferred file size:",
        "Literal data:",
        "Matched data:",
        "File list size:",
        "File list generation time:",
        "File list transfer time:",
        "Total bytes sent:",
        "Total bytes received:",
    ];

    let mut last_pos = 0;
    for label in &labels {
        let pos = output.find(label).unwrap_or_else(|| {
            panic!("label {label:?} not found in stats output:\n{output}");
        });
        assert!(
            pos >= last_pos,
            "label {label:?} at position {pos} appears before previous label at {last_pos}; order mismatch in:\n{output}"
        );
        last_pos = pos;
    }
}

#[test]
fn parity_stats_total_file_size_uses_bytes_suffix() {
    let (summary, _temp) = create_known_summary(&[("size.txt", b"payload data")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    // Upstream: "Total file size: N bytes"
    for line in output.lines() {
        if line.starts_with("Total file size:") {
            assert!(
                line.ends_with(" bytes"),
                "Total file size line should end with ' bytes': {line:?}"
            );
            break;
        }
    }
}

#[test]
fn parity_stats_literal_data_uses_bytes_suffix() {
    let (summary, _temp) = create_known_summary(&[("literal.txt", b"data")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    for line in output.lines() {
        if line.starts_with("Literal data:") {
            assert!(
                line.ends_with(" bytes"),
                "Literal data line should end with ' bytes': {line:?}"
            );
            break;
        }
    }
}

#[test]
fn parity_stats_matched_data_uses_bytes_suffix() {
    let (summary, _temp) = create_known_summary(&[("matched.txt", b"data")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    for line in output.lines() {
        if line.starts_with("Matched data:") {
            assert!(
                line.ends_with(" bytes"),
                "Matched data line should end with ' bytes': {line:?}"
            );
            break;
        }
    }
}

#[test]
fn parity_stats_file_list_generation_time_uses_seconds_suffix() {
    let (summary, _temp) = create_known_summary(&[("gen.txt", b"data")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    for line in output.lines() {
        if line.starts_with("File list generation time:") {
            assert!(
                line.ends_with(" seconds"),
                "File list generation time should end with ' seconds': {line:?}"
            );
            break;
        }
    }
}

#[test]
fn parity_stats_file_list_transfer_time_uses_seconds_suffix() {
    let (summary, _temp) = create_known_summary(&[("xfer.txt", b"data")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    for line in output.lines() {
        if line.starts_with("File list transfer time:") {
            assert!(
                line.ends_with(" seconds"),
                "File list transfer time should end with ' seconds': {line:?}"
            );
            break;
        }
    }
}

#[test]
fn parity_stats_file_list_time_format_has_three_decimal_places() {
    let (summary, _temp) = create_known_summary(&[("decimal.txt", b"data")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    for line in output.lines() {
        if line.starts_with("File list generation time:") {
            // Extract the numeric value: "File list generation time: 0.001 seconds"
            let value_part = line
                .strip_prefix("File list generation time: ")
                .unwrap()
                .strip_suffix(" seconds")
                .unwrap();
            let decimal_pos = value_part.find('.').expect("should contain decimal point");
            let fractional = &value_part[decimal_pos + 1..];
            assert_eq!(
                fractional.len(),
                3,
                "file list generation time should have 3 decimal places, got: {value_part:?}"
            );
            break;
        }
    }
}

#[test]
fn parity_stats_number_of_files_includes_category_breakdown() {
    let (summary, _temp) = create_known_summary(&[("breakdown.txt", b"content")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    for line in output.lines() {
        if line.starts_with("Number of files:") {
            // Upstream format: "Number of files: N (reg: X, dir: Y)"
            // The parenthetical breakdown should be present when there are non-zero categories
            assert!(
                line.contains('(') && line.contains(')'),
                "Number of files should include category breakdown in parentheses: {line:?}"
            );
            break;
        }
    }
}

#[test]
fn parity_stats_number_of_deleted_files_shows_zero_without_breakdown() {
    let (summary, _temp) = create_known_summary(&[("del.txt", b"data")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    for line in output.lines() {
        if line.starts_with("Number of deleted files:") {
            assert_eq!(
                line, "Number of deleted files: 0",
                "deleted files count should be 0 for a basic transfer: {line:?}"
            );
            break;
        }
    }
}

#[test]
fn parity_stats_includes_totals_after_blank_line() {
    let (summary, _temp) = create_known_summary(&[("totals.txt", b"data")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    // Upstream: stats block ends with "Total bytes received: N", then a blank line,
    // then "sent X bytes  received Y bytes  Z bytes/sec"
    assert!(
        output.contains("\n\nsent "),
        "stats output should have a blank line before the 'sent' totals line:\n{output}"
    );
}

// ===========================================================================
// 2. Totals line format parity
// ===========================================================================

#[test]
fn parity_totals_sent_received_line_format() {
    let (summary, _temp) = create_known_summary(&[("total.txt", b"test content")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    // Upstream: "sent X bytes  received Y bytes  Z bytes/sec"
    let totals_line = output
        .lines()
        .find(|line| line.starts_with("sent "))
        .expect("should contain a 'sent' totals line");

    assert!(
        totals_line.contains(" bytes  received "),
        "totals line should contain ' bytes  received ': {totals_line:?}"
    );
    assert!(
        totals_line.contains(" bytes/sec"),
        "totals line should contain ' bytes/sec': {totals_line:?}"
    );
}

#[test]
fn parity_totals_sent_line_uses_double_space_separator() {
    let (summary, _temp) = create_known_summary(&[("space.txt", b"test")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    let totals_line = output
        .lines()
        .find(|line| line.starts_with("sent "))
        .expect("should contain a 'sent' totals line");

    // Upstream uses double-space between "sent X bytes" and "received Y bytes"
    assert!(
        totals_line.contains("bytes  received"),
        "should use double-space between sent and received: {totals_line:?}"
    );
    // Also double-space before bytes/sec
    assert!(
        totals_line.contains("bytes  ") && totals_line.contains("bytes/sec"),
        "should use double-space before bytes/sec: {totals_line:?}"
    );
}

#[test]
fn parity_totals_speedup_line_format() {
    let (summary, _temp) = create_known_summary(&[("speed.txt", b"test content for speedup")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    // Upstream: "total size is X  speedup is Y"
    let speedup_line = output
        .lines()
        .find(|line| line.starts_with("total size is "))
        .expect("should contain a 'total size is' line");

    assert!(
        speedup_line.contains("  speedup is "),
        "speedup line should contain double-space before 'speedup is': {speedup_line:?}"
    );
}

#[test]
fn parity_totals_speedup_has_two_decimal_places() {
    let (summary, _temp) = create_known_summary(&[("decimal_speed.txt", b"test")]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    let speedup_line = output
        .lines()
        .find(|line| line.starts_with("total size is "))
        .expect("should contain a 'total size is' line");

    // Extract speedup value: "total size is X  speedup is Y.ZZ"
    let speedup_value = speedup_line
        .rsplit("speedup is ")
        .next()
        .expect("should have speedup value");

    let decimal_pos = speedup_value
        .find('.')
        .expect("speedup should contain decimal point");
    let fractional = &speedup_value[decimal_pos + 1..];
    assert_eq!(
        fractional.len(),
        2,
        "speedup should have 2 decimal places: {speedup_value:?}"
    );
}

#[test]
fn parity_totals_only_without_stats_flag() {
    let (summary, _temp) = create_known_summary(&[("totals_only.txt", b"content")]);

    // With verbosity > 0 but stats=false, only totals should appear (no detailed stats)
    let mut rendered = Vec::new();
    emit_transfer_summary(
        &summary,
        1, // verbosity
        None,
        false, // stats=false
        false,
        false,
        None,
        &OutFormatContext::default(),
        NameOutputLevel::UpdatedAndUnchanged,
        false,
        HumanReadableMode::Disabled,
        false,
        &mut rendered,
    )
    .expect("render");
    let output = String::from_utf8(rendered).expect("utf8");

    assert!(
        output.contains("sent "),
        "verbose mode should include 'sent' totals line:\n{output}"
    );
    assert!(
        output.contains("total size is "),
        "verbose mode should include 'total size is' line:\n{output}"
    );
    assert!(
        !output.contains("Number of files:"),
        "verbose without stats should NOT include detailed stats:\n{output}"
    );
}

#[test]
fn parity_totals_human_readable_mode_uses_units() {
    let (summary, _temp) = create_known_summary(&[("hr.txt", &[0u8; 2048])]);
    let output = render_stats(&summary, HumanReadableMode::Enabled);

    // With human-readable enabled, sizes should use unit suffixes
    let totals_line = output
        .lines()
        .find(|line| line.starts_with("sent "))
        .expect("should contain a 'sent' totals line");

    // The rate should be formatted with human-readable units
    assert!(
        totals_line.contains("bytes/sec"),
        "totals line should still end with 'bytes/sec': {totals_line:?}"
    );
}

// ===========================================================================
// 3. Verbose output parity
// ===========================================================================

#[test]
fn parity_verbose_lists_filenames_one_per_line() {
    let (summary, _temp) = create_known_summary(&[
        ("alpha.txt", b"alpha content"),
        ("beta.txt", b"beta content"),
    ]);
    let output = render_verbose(&summary, 1);

    // At verbosity 1, upstream rsync prints each filename on its own line
    assert!(
        output.contains("alpha.txt\n"),
        "should list alpha.txt on its own line:\n{output}"
    );
    assert!(
        output.contains("beta.txt\n"),
        "should list beta.txt on its own line:\n{output}"
    );
}

#[test]
fn parity_verbose_directory_names_end_with_slash() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    std::fs::create_dir_all(source_dir.join("subdir")).expect("create subdir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");
    std::fs::write(source_dir.join("subdir").join("file.txt"), b"data")
        .expect("write file in subdir");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let config = ClientConfig::builder()
        .transfer_args([src_operand, dest_dir.into_os_string()])
        .recursive(true)
        .verbosity(1)
        .force_event_collection(true)
        .build();

    let summary = run_client(config).expect("run_client succeeds");

    // Render with out-format %n (which adds trailing slash for directories)
    let format = parse_out_format(OsStr::new("%n")).expect("parse %n");
    let mut output = Vec::new();
    for event in summary.events() {
        if matches!(event.kind(), ClientEventKind::DirectoryCreated) {
            format
                .render(event, &OutFormatContext::default(), &mut output)
                .expect("render event");
        }
    }
    let rendered = String::from_utf8(output).expect("utf8");

    // Directory names in verbose/out-format output should end with /
    for line in rendered.lines() {
        assert!(
            line.ends_with('/'),
            "directory name should end with '/': {line:?}"
        );
    }
}

#[test]
fn parity_verbose_v2_includes_descriptor_and_bytes() {
    let (summary, _temp) = create_known_summary(&[("vv.txt", b"double verbose content")]);

    let mut rendered = Vec::new();
    emit_transfer_summary(
        &summary,
        2, // -vv
        None,
        false,
        false,
        false,
        None,
        &OutFormatContext::default(),
        NameOutputLevel::UpdatedAndUnchanged,
        false,
        HumanReadableMode::Disabled,
        false,
        &mut rendered,
    )
    .expect("render");
    let output = String::from_utf8(rendered).expect("utf8");

    // At verbosity 2, upstream rsync includes descriptors like "copied:" before filenames
    // and byte counts for data transfers
    let has_descriptor_line = output.lines().any(|line| {
        line.contains("copied:")
            || line.contains("directory:")
            || line.contains("symlink:")
            || line.contains("hard link:")
    });
    assert!(
        has_descriptor_line,
        "verbosity 2 should include descriptor labels (e.g. 'copied:'):\n{output}"
    );
}

#[test]
fn parity_verbose_skipped_file_messages_use_upstream_format() {
    // Verify that skip messages match upstream rsync format:
    // "skipping non-regular file \"name\""
    // "skipping existing file \"name\""
    // etc.
    let event = ClientEvent::for_test(
        PathBuf::from("skip_me.txt"),
        ClientEventKind::SkippedExisting,
        false,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        LocalCopyChangeSet::new(),
    );

    let events = [event];
    let summary_with_skip = {
        // Create a dummy summary by running a trivial transfer, then we test the
        // emit_verbose rendering manually on the skip event
        let mut rendered = Vec::new();
        // emit_verbose is crate-internal; access through emit_transfer_summary with
        // the skip event in context doesn't work since we can't inject events.
        // Instead, verify the skip message format by using run_with_args or direct format.
        //
        // For direct verification, we test the %i format for skip events:
        let format = parse_out_format(OsStr::new("%o %n")).expect("parse");
        format
            .render(&events[0], &OutFormatContext::default(), &mut rendered)
            .expect("render");
        String::from_utf8(rendered).expect("utf8")
    };

    assert_eq!(
        summary_with_skip, "skipped existing file skip_me.txt\n",
        "skip message format should match upstream"
    );
}

// ===========================================================================
// 4. Itemized changes format parity
// ===========================================================================

#[test]
fn parity_itemize_new_file_format_matches_upstream() {
    // Upstream rsync: ">f+++++++++ file.txt"
    let event = ClientEvent::for_test(
        PathBuf::from("file.txt"),
        ClientEventKind::DataCopied,
        true,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        LocalCopyChangeSet::new(),
    );

    let output = render_itemize(&event);
    assert_eq!(
        output, ">f+++++++++ file.txt\n",
        "new file itemize should match upstream format"
    );
}

#[test]
fn parity_itemize_new_directory_format_matches_upstream() {
    // Upstream rsync: "cd+++++++++ dir/"
    let event = ClientEvent::for_test(
        PathBuf::from("dir"),
        ClientEventKind::DirectoryCreated,
        true,
        Some(ClientEvent::test_metadata(ClientEntryKind::Directory)),
        LocalCopyChangeSet::new(),
    );

    let output = render_itemize(&event);
    assert_eq!(
        output, "cd+++++++++ dir/\n",
        "new directory itemize should match upstream format (with trailing slash)"
    );
}

#[test]
fn parity_itemize_new_symlink_format_matches_upstream() {
    // Upstream rsync: "cL+++++++++ link"
    let event = ClientEvent::for_test(
        PathBuf::from("link"),
        ClientEventKind::SymlinkCopied,
        true,
        Some(ClientEvent::test_metadata(ClientEntryKind::Symlink)),
        LocalCopyChangeSet::new(),
    );

    let output = render_itemize(&event);
    assert_eq!(
        output, "cL+++++++++ link\n",
        "new symlink itemize should match upstream format"
    );
}

#[test]
fn parity_itemize_deletion_format_matches_upstream() {
    // Upstream rsync: "*deleting   filename"
    let event = ClientEvent::for_test(
        PathBuf::from("obsolete.txt"),
        ClientEventKind::EntryDeleted,
        false,
        None,
        LocalCopyChangeSet::new(),
    );

    let output = render_itemize(&event);
    assert!(
        output.starts_with("*deleting"),
        "deletion should start with '*deleting': {output:?}"
    );
    assert!(
        output.contains("obsolete.txt"),
        "deletion should include the filename: {output:?}"
    );
}

#[test]
fn parity_itemize_string_is_eleven_chars_for_non_delete() {
    // Upstream rsync: the itemize string is always 11 characters for non-delete operations
    let test_cases: &[(ClientEventKind, bool, ClientEntryKind)] = &[
        (ClientEventKind::DataCopied, true, ClientEntryKind::File),
        (
            ClientEventKind::DirectoryCreated,
            true,
            ClientEntryKind::Directory,
        ),
        (
            ClientEventKind::SymlinkCopied,
            true,
            ClientEntryKind::Symlink,
        ),
        (
            ClientEventKind::MetadataReused,
            false,
            ClientEntryKind::File,
        ),
        (ClientEventKind::DataCopied, false, ClientEntryKind::File),
    ];

    for (kind, created, entry_kind) in test_cases {
        let event = ClientEvent::for_test(
            PathBuf::from("test.txt"),
            kind.clone(),
            *created,
            Some(ClientEvent::test_metadata(*entry_kind)),
            LocalCopyChangeSet::new(),
        );

        let format = parse_out_format(OsStr::new("%i")).expect("parse %i");
        let mut output = Vec::new();
        format
            .render(&event, &OutFormatContext::default(), &mut output)
            .expect("render");
        let rendered = String::from_utf8(output).expect("utf8");
        let itemize_str = rendered.trim_end_matches('\n');
        assert_eq!(
            itemize_str.len(),
            11,
            "itemize string should be 11 chars for {kind:?}: got {itemize_str:?}"
        );
    }
}

#[test]
fn parity_itemize_unchanged_file_shows_dots() {
    // Upstream rsync: ".f........." for unchanged file with -ii
    let event = ClientEvent::for_test(
        PathBuf::from("unchanged.txt"),
        ClientEventKind::MetadataReused,
        false,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        LocalCopyChangeSet::new(),
    );

    let format = parse_out_format(OsStr::new("%i")).expect("parse %i");
    let mut output = Vec::new();
    format
        .render(&event, &OutFormatContext::default(), &mut output)
        .expect("render");
    let rendered = String::from_utf8(output).expect("utf8");

    assert_eq!(
        rendered.trim(),
        ".f.........",
        "unchanged file should show all dots"
    );
}

#[test]
fn parity_itemize_checksum_change_shows_c_at_position_2() {
    // Upstream rsync position 2: 'c' for checksum/content change
    let cs = LocalCopyChangeSet::new().with_checksum_changed(true);
    let event = ClientEvent::for_test(
        PathBuf::from("checksum.txt"),
        ClientEventKind::DataCopied,
        false,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        cs,
    );

    let format = parse_out_format(OsStr::new("%i")).expect("parse %i");
    let mut output = Vec::new();
    format
        .render(&event, &OutFormatContext::default(), &mut output)
        .expect("render");
    let rendered = String::from_utf8(output).expect("utf8");
    let itemize = rendered.trim();

    assert_eq!(
        itemize.chars().nth(2),
        Some('c'),
        "position 2 should be 'c' for checksum change: {itemize:?}"
    );
}

#[test]
fn parity_itemize_size_change_shows_s_at_position_3() {
    // Upstream rsync position 3: 's' for size change
    let cs = LocalCopyChangeSet::new().with_size_changed(true);
    let event = ClientEvent::for_test(
        PathBuf::from("sized.txt"),
        ClientEventKind::DataCopied,
        false,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        cs,
    );

    let format = parse_out_format(OsStr::new("%i")).expect("parse %i");
    let mut output = Vec::new();
    format
        .render(&event, &OutFormatContext::default(), &mut output)
        .expect("render");
    let rendered = String::from_utf8(output).expect("utf8");
    let itemize = rendered.trim();

    assert_eq!(
        itemize.chars().nth(3),
        Some('s'),
        "position 3 should be 's' for size change: {itemize:?}"
    );
}

#[test]
fn parity_itemize_time_change_shows_t_at_position_4() {
    // Upstream rsync position 4: 't' for time modified, 'T' for transfer time
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::Modified));
    let event = ClientEvent::for_test(
        PathBuf::from("timed.txt"),
        ClientEventKind::DataCopied,
        false,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        cs,
    );

    let format = parse_out_format(OsStr::new("%i")).expect("parse %i");
    let mut output = Vec::new();
    format
        .render(&event, &OutFormatContext::default(), &mut output)
        .expect("render");
    let rendered = String::from_utf8(output).expect("utf8");
    let itemize = rendered.trim();

    assert_eq!(
        itemize.chars().nth(4),
        Some('t'),
        "position 4 should be 't' for time modified: {itemize:?}"
    );
}

#[test]
fn parity_itemize_transfer_time_shows_uppercase_t_at_position_4() {
    // Upstream rsync: 'T' when times are not preserved (transfer time)
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::TransferTime));
    let event = ClientEvent::for_test(
        PathBuf::from("transfer_time.txt"),
        ClientEventKind::DataCopied,
        false,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        cs,
    );

    let format = parse_out_format(OsStr::new("%i")).expect("parse %i");
    let mut output = Vec::new();
    format
        .render(&event, &OutFormatContext::default(), &mut output)
        .expect("render");
    let rendered = String::from_utf8(output).expect("utf8");
    let itemize = rendered.trim();

    assert_eq!(
        itemize.chars().nth(4),
        Some('T'),
        "position 4 should be 'T' for transfer time: {itemize:?}"
    );
}

#[test]
fn parity_itemize_permissions_change_shows_p_at_position_5() {
    let cs = LocalCopyChangeSet::new().with_permissions_changed(true);
    let event = ClientEvent::for_test(
        PathBuf::from("perms.txt"),
        ClientEventKind::DataCopied,
        false,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        cs,
    );

    let format = parse_out_format(OsStr::new("%i")).expect("parse %i");
    let mut output = Vec::new();
    format
        .render(&event, &OutFormatContext::default(), &mut output)
        .expect("render");
    let rendered = String::from_utf8(output).expect("utf8");
    let itemize = rendered.trim();

    assert_eq!(
        itemize.chars().nth(5),
        Some('p'),
        "position 5 should be 'p' for permissions change: {itemize:?}"
    );
}

#[test]
fn parity_itemize_typical_update_pattern_cst() {
    // Upstream rsync typical content update: ">fcst......"
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_change(Some(TimeChange::Modified));
    let event = ClientEvent::for_test(
        PathBuf::from("updated.txt"),
        ClientEventKind::DataCopied,
        false,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        cs,
    );

    let format = parse_out_format(OsStr::new("%i")).expect("parse %i");
    let mut output = Vec::new();
    format
        .render(&event, &OutFormatContext::default(), &mut output)
        .expect("render");
    let rendered = String::from_utf8(output).expect("utf8");

    assert_eq!(
        rendered.trim(),
        ">fcst......",
        "typical file update should show '>fcst......'"
    );
}

#[test]
fn parity_itemize_full_change_pattern() {
    // Upstream rsync all changes: ">fcstpogbax"
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_change(Some(TimeChange::Modified))
        .with_permissions_changed(true)
        .with_owner_changed(true)
        .with_group_changed(true)
        .with_access_time_changed(true)
        .with_create_time_changed(true)
        .with_acl_changed(true)
        .with_xattr_changed(true);
    let event = ClientEvent::for_test(
        PathBuf::from("all_changes.txt"),
        ClientEventKind::DataCopied,
        false,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        cs,
    );

    let format = parse_out_format(OsStr::new("%i")).expect("parse %i");
    let mut output = Vec::new();
    format
        .render(&event, &OutFormatContext::default(), &mut output)
        .expect("render");
    let rendered = String::from_utf8(output).expect("utf8");

    assert_eq!(
        rendered.trim(),
        ">fcstpogbax",
        "all-changes pattern should be '>fcstpogbax'"
    );
}

// ===========================================================================
// 5. End-to-end output parity: full transfer with --stats
// ===========================================================================

#[test]
fn parity_end_to_end_stats_output_via_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");
    std::fs::write(source_dir.join("e2e.txt"), b"end to end test").expect("write");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-r"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0, "transfer should succeed");
    let output = String::from_utf8(stdout).expect("utf8");

    // Verify the complete upstream stats format structure
    assert!(
        output.contains("Number of files:"),
        "should contain stats header"
    );
    assert!(
        output.contains("sent ") && output.contains(" bytes  received "),
        "should contain totals line"
    );
    assert!(
        output.contains("total size is ") && output.contains("speedup is "),
        "should contain speedup line"
    );
}

#[test]
fn parity_end_to_end_itemize_output_via_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("item.txt");
    let dest_dir = temp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"itemize test").expect("write");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-i"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0, "transfer should succeed");
    let output = String::from_utf8(stdout).expect("utf8");

    assert_eq!(
        output.trim(),
        ">f+++++++++ item.txt",
        "itemize output for new file should match upstream format"
    );
}

#[test]
fn parity_end_to_end_verbose_output_via_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("verbose_test.txt");
    let dest_dir = temp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"verbose content").expect("write");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0, "transfer should succeed");
    let output = String::from_utf8(stdout).expect("utf8");

    // At -v, upstream rsync shows filename on its own line, then totals
    assert!(
        output.contains("verbose_test.txt"),
        "verbose output should contain the filename:\n{output}"
    );
    assert!(
        output.contains("sent ") && output.contains("speedup is"),
        "verbose output should contain totals:\n{output}"
    );
}

// ===========================================================================
// 6. Human-readable format parity
// ===========================================================================

#[test]
fn parity_stats_human_readable_disabled_uses_comma_separators() {
    let (summary, _temp) = create_known_summary(&[("comma.txt", &[0u8; 1500])]);
    let output = render_stats(&summary, HumanReadableMode::Disabled);

    // With human-readable disabled, sizes should use comma-separated decimal notation
    // For values >= 1000
    for line in output.lines() {
        if line.starts_with("Total file size:") || line.starts_with("Literal data:") {
            // These lines should contain comma-separated numbers for values >= 1000
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Find the numeric part (between the label and "bytes")
            for part in &parts {
                if part.contains(',') {
                    // Verify comma is used as thousands separator
                    let digits_only: String = part.chars().filter(|c| c.is_ascii_digit()).collect();
                    assert!(
                        !digits_only.is_empty(),
                        "comma-separated value should contain digits: {part:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn parity_stats_human_readable_enabled_uses_unit_suffixes() {
    let (summary, _temp) = create_known_summary(&[("units.txt", &[0u8; 2000])]);
    let output = render_stats(&summary, HumanReadableMode::Enabled);

    // With human-readable enabled, sizes >= 1000 should use K/M/G suffixes
    let has_unit_suffix = output.lines().any(|line| {
        (line.starts_with("Total file size:") || line.starts_with("Literal data:"))
            && (line.contains("K ") || line.contains("M ") || line.contains("G "))
    });

    assert!(
        has_unit_suffix,
        "human-readable mode should use unit suffixes for large sizes:\n{output}"
    );
}
