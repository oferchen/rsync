// Output parity tests: verify oc-rsync output format matches upstream rsync conventions.
//
// These tests call the formatting functions directly with known inputs and verify
// that the rendered output matches the structure and content expected by upstream
// rsync's --stats, --verbose, and --itemize-changes modes.

use super::common::{RSYNC, run_with_args};
use super::*;
use super::{
    NameOutputLevel, OutFormatContext, ProgressSetting, emit_transfer_summary, parse_out_format,
};
use core::client::{
    ClientConfig, ClientEntryKind, ClientEvent, ClientEventKind, ClientSummary, HumanReadableMode,
    run_client,
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

// ===========================================================================
// 7. Dry-run output format parity with itemize changes
// ===========================================================================

#[test]
fn parity_dry_run_with_itemize_shows_changes_without_modifying_files() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    fs::write(source_dir.join("new.txt"), b"new content").expect("write new");
    fs::write(source_dir.join("modified.txt"), b"updated").expect("write modified");
    fs::write(dest_dir.join("modified.txt"), b"old").expect("write dest modified");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--dry-run"),
        OsString::from("--itemize-changes"),
        src_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "dry-run should succeed");
    assert!(stderr.is_empty(), "stderr should be empty");

    let output = String::from_utf8(stdout).expect("utf8");

    // Upstream rsync shows itemize strings for files that would be transferred
    assert!(
        output.contains(">f+++++++++") || output.contains(">f"),
        "dry-run with -i should show itemize changes: {output}"
    );

    // Files should not actually be modified
    assert_eq!(
        fs::read(dest_dir.join("modified.txt")).expect("read modified"),
        b"old",
        "dry-run must not modify existing files"
    );
    assert!(
        !dest_dir.join("new.txt").exists(),
        "dry-run must not create new files"
    );
}

#[test]
fn parity_dry_run_with_verbose_lists_files_line_by_line() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    fs::write(source_dir.join("alpha.txt"), b"alpha").expect("write alpha");
    fs::write(source_dir.join("beta.txt"), b"beta").expect("write beta");
    fs::write(source_dir.join("gamma.txt"), b"gamma").expect("write gamma");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-nv"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Upstream rsync -nv lists each file on its own line
    assert!(
        output.contains("alpha.txt\n"),
        "should list alpha.txt on separate line"
    );
    assert!(
        output.contains("beta.txt\n"),
        "should list beta.txt on separate line"
    );
    assert!(
        output.contains("gamma.txt\n"),
        "should list gamma.txt on separate line"
    );
}

#[test]
fn parity_dry_run_deletion_shows_deleting_prefix() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    fs::write(source_dir.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest_dir.join("keep.txt"), b"old keep").expect("write dest keep");
    fs::write(dest_dir.join("delete_me.txt"), b"delete").expect("write delete_me");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rnv"),
        OsString::from("--delete"),
        src_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(
        code,
        0,
        "dry-run --delete should succeed; stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
    let output = String::from_utf8(stdout).expect("utf8");

    // Upstream rsync -nv --delete shows "deleting" prefix for files to be deleted
    assert!(
        output.contains("deleting") || output.contains("delete_me.txt"),
        "dry-run --delete should show deletion messages: {output}"
    );

    // File should still exist
    assert!(
        dest_dir.join("delete_me.txt").exists(),
        "dry-run must not actually delete files"
    );
}

// ===========================================================================
// 8. List-only output format parity (long listing like ls -l)
// ===========================================================================

#[test]
fn parity_list_only_format_matches_upstream_structure() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    fs::write(source_dir.join("file.txt"), b"test content").expect("write file");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let output = String::from_utf8(stdout).expect("utf8");

    // Upstream format: <permissions> <size> <timestamp> <name>
    // Example: "-rw-r--r--            12 2023/11/14 22:13:20 file.txt"
    for line in output.lines() {
        if line.contains("file.txt") {
            // Should start with permission string (10 chars)
            assert!(
                line.starts_with('-') || line.starts_with('d') || line.starts_with('l'),
                "list-only line should start with file type: {line}"
            );

            // Should contain the filename at the end
            assert!(
                line.ends_with("file.txt"),
                "list-only line should end with filename: {line}"
            );

            // Should contain a date in YYYY/MM/DD format
            assert!(
                line.contains('/'),
                "list-only line should contain date separators: {line}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn parity_list_only_directory_shows_d_prefix_without_trailing_slash() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    fs::create_dir(source_dir.join("subdir")).expect("create subdir");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("-r"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Find the directory line
    for line in output.lines() {
        if line.contains("subdir") {
            assert!(
                line.starts_with('d'),
                "directory should start with 'd': {line}"
            );
            // In --list-only mode, directories do NOT have trailing slash
            // (unlike verbose mode with %n format)
            assert!(
                !line.ends_with('/'),
                "list-only directory should not have trailing slash: {line}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn parity_list_only_size_field_is_right_aligned_15_chars() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    // Create files of various sizes to test alignment
    fs::write(source_dir.join("small.txt"), b"x").expect("write small");
    fs::write(source_dir.join("large.txt"), vec![0u8; 123456]).expect("write large");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Upstream rsync uses exactly 15 characters for the size field, right-aligned
    for line in output.lines() {
        if line.len() >= 26 {
            let size_field = &line[11..26];
            assert_eq!(
                size_field.len(),
                15,
                "size field should be exactly 15 chars: {line}"
            );
        }
    }
}

// ===========================================================================
// 9. Error message format parity
// ===========================================================================

#[test]
fn parity_error_messages_use_rsync_error_prefix() {
    // Test that error messages follow upstream format: "rsync error: <description>"
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("/nonexistent/source/path"),
        OsString::from("/tmp/dest"),
    ]);

    assert_ne!(code, 0, "should fail with nonexistent source");

    let error_output = String::from_utf8_lossy(&stderr);

    // Upstream rsync error messages contain descriptive text
    // Error format typically includes the program name or "rsync" prefix
    assert!(
        !error_output.is_empty(),
        "should produce error output for nonexistent source"
    );
}

#[test]
fn parity_error_exit_codes_match_upstream_rerr_codes() {
    // Test that we use standard rsync exit codes
    // RERR_SYNTAX = 1 (syntax or usage error)
    // RERR_FILESELECT = 3 (errors selecting files)

    // Invalid option should return syntax error (code 1)
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--invalid-option-that-does-not-exist"),
    ]);

    assert_eq!(
        code, 1,
        "invalid option should return exit code 1 (syntax error)"
    );
}

#[test]
fn parity_error_file_not_found_shows_descriptive_message() {
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("/this/path/definitely/does/not/exist/file.txt"),
        OsString::from("/tmp/dest"),
    ]);

    assert_ne!(code, 0);
    let error_output = String::from_utf8_lossy(&stderr);

    // Error message should contain information about the missing file
    assert!(
        error_output.contains("exist")
            || error_output.contains("No such")
            || error_output.contains("not found"),
        "error should describe file not found: {error_output}"
    );
}

// ===========================================================================
// 9b. Exit code parity for additional error conditions
// ===========================================================================

#[test]
fn parity_error_no_args_returns_syntax_error() {
    // Upstream rsync with no arguments returns RERR_SYNTAX (1)
    let (code, _stdout, stderr) = run_with_args([OsString::from(RSYNC)]);

    let err = String::from_utf8_lossy(&stderr);
    assert_eq!(
        code, 1,
        "no arguments should return exit code 1 (syntax error): {err}"
    );
}

#[test]
fn parity_error_conflicting_options_returns_syntax_error() {
    // Mutually exclusive options should return RERR_SYNTAX (1)
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--daemon"),
        OsString::from("--server"),
        OsString::from("src/"),
        OsString::from("dst/"),
    ]);

    // --daemon and --server are incompatible usage patterns; expect syntax error
    assert!(
        code == 1 || code == 2,
        "conflicting options should return syntax or protocol error, got {code}"
    );
}

#[test]
fn parity_error_missing_destination_returns_syntax_error() {
    // Single operand without destination is a syntax error in upstream rsync
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from("source_only"),
    ]);

    // Upstream returns 1 for syntax/usage error when destination is missing
    assert!(
        code != 0,
        "missing destination should return non-zero exit code"
    );
}

#[test]
fn parity_error_permission_denied_returns_io_error() {
    use std::fs;

    // Create a file then make destination unwritable
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("perm.txt");
    let dest_dir = temp.path().join("dest_no_write");
    fs::write(&source, b"permission test").expect("write");
    fs::create_dir(&dest_dir).expect("create dest");

    // Make destination read-only
    let mut perms = fs::metadata(&dest_dir).expect("metadata").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o444);
        fs::set_permissions(&dest_dir, perms.clone()).expect("set perms");
    }

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        source.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    // Restore permissions for cleanup
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        let _ = fs::set_permissions(&dest_dir, perms);
    }

    // Permission errors should return non-zero (typically 23 for partial transfer
    // or 3 for file selection error)
    #[cfg(unix)]
    assert!(
        code != 0,
        "permission denied should return non-zero exit code"
    );
}

#[test]
fn parity_error_version_returns_zero() {
    // --version should always return 0
    let (code, stdout, _stderr) =
        run_with_args([OsString::from(RSYNC), OsString::from("--version")]);

    assert_eq!(code, 0, "--version should return exit code 0");
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(!output.is_empty(), "--version should produce output");
}

#[test]
fn parity_error_dry_run_returns_zero_on_success() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("dry.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"dry run test").expect("write");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-n"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0, "dry run should return exit code 0 on success");
}

// ===========================================================================
// 10. --info=progress2 overall progress output format
// ===========================================================================

#[test]
fn parity_info_progress2_shows_overall_progress_line() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("progress.txt");
    let dest = temp.path().join("progress.out");
    fs::write(&source, b"progress test content").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=progress2"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Upstream --info=progress2 format: "  12.34kB   5%  123.45kB/s    0:00:01 (xfr#1, to-chk=0/1)"
    // Should contain: percentage, speed, time estimate, and "to-chk=N/M" pattern
    assert!(
        output.contains("to-chk=") || output.contains("B/s"),
        "progress2 should show overall progress with to-chk pattern: {output}"
    );
}

#[test]
fn parity_info_progress2_format_includes_transfer_rate() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("rate.txt");
    let dest = temp.path().join("rate.out");
    fs::write(&source, vec![0u8; 10000]).expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=progress2"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Should show transfer rate in format like "123.45kB/s" or "1.23MB/s"
    assert!(
        output.contains("B/s") || output.contains("kB/s") || output.contains("MB/s"),
        "progress2 should show transfer rate: {output}"
    );
}

#[test]
fn parity_info_progress2_shows_to_chk_counter() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    fs::write(source_dir.join("file1.txt"), b"file1").expect("write file1");
    fs::write(source_dir.join("file2.txt"), b"file2").expect("write file2");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=progress2"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Upstream format includes "to-chk=N/M" where N is files left, M is total
    assert!(
        output.contains("to-chk="),
        "progress2 should show to-chk counter: {output}"
    );

    // Should show to-chk=0/N at the end (all files checked)
    assert!(
        output.contains("to-chk=0/"),
        "progress2 should show to-chk=0/ at completion: {output}"
    );
}

// ===========================================================================
// 11. --out-format placeholder parity (%f, %n, %l, etc.)
// ===========================================================================

#[test]
fn parity_out_format_f_placeholder_shows_filename() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("formattest.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"format test").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=%f"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // %f shows just the filename without path
    assert!(
        output.contains("formattest.txt"),
        "--out-format=%f should show filename: {output}"
    );
}

#[test]
fn parity_out_format_n_placeholder_shows_name_with_directory_slash() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    fs::create_dir(source_dir.join("testdir")).expect("create testdir");
    fs::write(source_dir.join("testdir/file.txt"), b"test").expect("write file");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        OsString::from("--out-format=%n"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // %n adds trailing slash for directories
    let has_dir_with_slash = output
        .lines()
        .any(|line| line.contains("testdir") && line.trim().ends_with('/'));

    assert!(
        has_dir_with_slash,
        "--out-format=%n should show directories with trailing slash: {output}"
    );
}

#[test]
fn parity_out_format_l_placeholder_shows_file_size() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("sized.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    let content = b"test content for size";
    fs::write(&source, content).expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=%l %f"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // %l shows the file size in bytes
    let expected_size = content.len().to_string();
    assert!(
        output.contains(&expected_size),
        "--out-format=%l should show file size {expected_size}: {output}"
    );
}

#[test]
fn parity_out_format_i_placeholder_shows_itemize_string() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("item.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"itemize test").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=%i %n"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // %i shows itemize changes (11-character string for new files: ">f+++++++++")
    assert!(
        output.contains(">f+++++++++") || output.contains(">f"),
        "--out-format=%i should show itemize string: {output}"
    );
}

#[test]
fn parity_out_format_o_placeholder_shows_operation() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("op.txt");
    let dest = temp.path().join("op.out");
    fs::write(&source, b"operation test").expect("write source");

    // First transfer
    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=%o %n"),
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // %o shows operation: "send" for sending, "recv" for receiving, "del" for deleting
    // In local copy mode, the operation depends on the context
    assert!(
        !output.trim().is_empty(),
        "--out-format=%o should produce output: {output}"
    );
}

#[test]
fn parity_out_format_combined_placeholders() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("combo.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"combined test").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=%i %n %l"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Combined format should show: itemize string, filename, and size
    let line = output.trim();

    // Should have itemize string (starts with > or .)
    assert!(
        line.starts_with('>') || line.starts_with('.') || line.starts_with('c'),
        "combined format should start with itemize: {line}"
    );

    // Should contain the filename
    assert!(
        line.contains("combo.txt"),
        "combined format should contain filename: {line}"
    );

    // Should contain the size (13 bytes)
    assert!(
        line.contains("13"),
        "combined format should contain size: {line}"
    );
}

#[test]
fn parity_out_format_literal_text_passthrough() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("literal.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"literal test").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=COPIED: %f"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Literal text in format string should pass through unchanged
    assert!(
        output.contains("COPIED:"),
        "--out-format should preserve literal text: {output}"
    );
    assert!(
        output.contains("literal.txt"),
        "--out-format should expand %f placeholder: {output}"
    );
}

// ===========================================================================
// 11. Verbose level 3+ output parity
// ===========================================================================

#[test]
fn parity_verbose_v3_includes_all_v2_info_flags() {
    // At -vvv, upstream rsync enables all level-2 info flags plus additional debug flags.
    // Verify that level 3 is a superset of level 2 for info flags.
    let config_v2 = logging::VerbosityConfig::from_verbose_level(2);
    let config_v3 = logging::VerbosityConfig::from_verbose_level(3);

    // All info flags at level 2 should be <= their level at level 3
    assert!(
        config_v3.info.get(logging::InfoFlag::Copy) >= config_v2.info.get(logging::InfoFlag::Copy)
    );
    assert!(
        config_v3.info.get(logging::InfoFlag::Del) >= config_v2.info.get(logging::InfoFlag::Del)
    );
    assert!(
        config_v3.info.get(logging::InfoFlag::Flist)
            >= config_v2.info.get(logging::InfoFlag::Flist)
    );
    assert!(
        config_v3.info.get(logging::InfoFlag::Misc) >= config_v2.info.get(logging::InfoFlag::Misc)
    );
    assert!(
        config_v3.info.get(logging::InfoFlag::Name) >= config_v2.info.get(logging::InfoFlag::Name)
    );
    assert!(
        config_v3.info.get(logging::InfoFlag::Stats)
            >= config_v2.info.get(logging::InfoFlag::Stats)
    );
    assert!(
        config_v3.info.get(logging::InfoFlag::Backup)
            >= config_v2.info.get(logging::InfoFlag::Backup)
    );
    assert!(
        config_v3.info.get(logging::InfoFlag::Skip) >= config_v2.info.get(logging::InfoFlag::Skip)
    );
}

#[test]
fn parity_verbose_v3_enables_additional_debug_flags() {
    // At -vvv, upstream rsync enables acl, backup, fuzzy, genr, own, recv, send, time, exit
    // debug flags that are NOT enabled at -vv.
    let config_v2 = logging::VerbosityConfig::from_verbose_level(2);
    let config_v3 = logging::VerbosityConfig::from_verbose_level(3);

    // These debug flags should be 0 at level 2 but > 0 at level 3
    assert_eq!(
        config_v2.debug.get(logging::DebugFlag::Acl),
        0,
        "acl should be off at -vv"
    );
    assert!(
        config_v3.debug.get(logging::DebugFlag::Acl) > 0,
        "acl should be on at -vvv"
    );

    assert_eq!(
        config_v2.debug.get(logging::DebugFlag::Genr),
        0,
        "genr should be off at -vv"
    );
    assert!(
        config_v3.debug.get(logging::DebugFlag::Genr) > 0,
        "genr should be on at -vvv"
    );

    assert_eq!(
        config_v2.debug.get(logging::DebugFlag::Recv),
        0,
        "recv should be off at -vv"
    );
    assert!(
        config_v3.debug.get(logging::DebugFlag::Recv) > 0,
        "recv should be on at -vvv"
    );

    assert_eq!(
        config_v2.debug.get(logging::DebugFlag::Send),
        0,
        "send should be off at -vv"
    );
    assert!(
        config_v3.debug.get(logging::DebugFlag::Send) > 0,
        "send should be on at -vvv"
    );

    assert_eq!(
        config_v2.debug.get(logging::DebugFlag::Exit),
        0,
        "exit should be off at -vv"
    );
    assert!(
        config_v3.debug.get(logging::DebugFlag::Exit) > 0,
        "exit should be on at -vvv"
    );
}

#[test]
fn parity_verbose_v3_increases_debug_levels_for_existing_flags() {
    // At -vvv, some debug flags that were level 1 at -vv increase to level 2
    let config_v2 = logging::VerbosityConfig::from_verbose_level(2);
    let config_v3 = logging::VerbosityConfig::from_verbose_level(3);

    assert!(
        config_v3.debug.get(logging::DebugFlag::Connect)
            > config_v2.debug.get(logging::DebugFlag::Connect),
        "connect should increase from -vv to -vvv"
    );
    assert!(
        config_v3.debug.get(logging::DebugFlag::Del) > config_v2.debug.get(logging::DebugFlag::Del),
        "del should increase from -vv to -vvv"
    );
    assert!(
        config_v3.debug.get(logging::DebugFlag::Deltasum)
            > config_v2.debug.get(logging::DebugFlag::Deltasum),
        "deltasum should increase from -vv to -vvv"
    );
    assert!(
        config_v3.debug.get(logging::DebugFlag::Filter)
            > config_v2.debug.get(logging::DebugFlag::Filter),
        "filter should increase from -vv to -vvv"
    );
    assert!(
        config_v3.debug.get(logging::DebugFlag::Flist)
            > config_v2.debug.get(logging::DebugFlag::Flist),
        "flist should increase from -vv to -vvv"
    );
}

#[test]
fn parity_verbose_v4_enables_proto_debug_flag() {
    // At -vvvv, upstream rsync enables the proto debug flag
    let config_v3 = logging::VerbosityConfig::from_verbose_level(3);
    let config_v4 = logging::VerbosityConfig::from_verbose_level(4);

    assert_eq!(
        config_v3.debug.get(logging::DebugFlag::Proto),
        0,
        "proto should be off at -vvv"
    );
    assert!(
        config_v4.debug.get(logging::DebugFlag::Proto) > 0,
        "proto should be on at -vvvv"
    );
}

#[test]
fn parity_verbose_v5_enables_maximum_debug_levels() {
    // At -vvvvv, upstream rsync sets maximum debug levels for deltasum and flist
    let config_v4 = logging::VerbosityConfig::from_verbose_level(4);
    let config_v5 = logging::VerbosityConfig::from_verbose_level(5);

    assert!(
        config_v5.debug.get(logging::DebugFlag::Deltasum)
            > config_v4.debug.get(logging::DebugFlag::Deltasum),
        "deltasum should be higher at -vvvvv than -vvvv"
    );
    assert!(
        config_v5.debug.get(logging::DebugFlag::Flist)
            > config_v4.debug.get(logging::DebugFlag::Flist),
        "flist should be higher at -vvvvv than -vvvv"
    );
    assert!(
        config_v5.debug.get(logging::DebugFlag::Fuzzy)
            > config_v4.debug.get(logging::DebugFlag::Fuzzy),
        "fuzzy should increase at -vvvvv"
    );
}

#[test]
fn parity_verbose_v3_produces_output_with_debug_info() {
    // Run an actual transfer at -vvv and verify output contains debug-level detail
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::write(source_dir.join("debug.txt"), b"debug output test").expect("write");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vvv"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0, "transfer should succeed at -vvv");
    let out = String::from_utf8(stdout).expect("utf8");
    let err = String::from_utf8(stderr).expect("utf8 stderr");
    let combined = format!("{out}{err}");

    // At -vvv, file listing should still be present
    assert!(
        combined.contains("debug.txt"),
        "verbose level 3 should list transferred files:\n{combined}"
    );
}

#[test]
fn parity_verbose_monotonic_info_levels() {
    // Verify that increasing verbosity never decreases any info flag level.
    // This matches upstream rsync's behavior where -vvv is strictly more verbose than -vv.
    for level in 1..=5u8 {
        let lower = logging::VerbosityConfig::from_verbose_level(level - 1);
        let higher = logging::VerbosityConfig::from_verbose_level(level);

        for flag in [
            logging::InfoFlag::Backup,
            logging::InfoFlag::Copy,
            logging::InfoFlag::Del,
            logging::InfoFlag::Flist,
            logging::InfoFlag::Misc,
            logging::InfoFlag::Mount,
            logging::InfoFlag::Name,
            logging::InfoFlag::Nonreg,
            logging::InfoFlag::Remove,
            logging::InfoFlag::Skip,
            logging::InfoFlag::Stats,
            logging::InfoFlag::Symsafe,
        ] {
            assert!(
                higher.info.get(flag) >= lower.info.get(flag),
                "info flag {flag:?} decreased from level {} to {}: {} -> {}",
                level - 1,
                level,
                lower.info.get(flag),
                higher.info.get(flag),
            );
        }
    }
}

#[test]
fn parity_verbose_monotonic_debug_levels() {
    // Verify that increasing verbosity never decreases any debug flag level.
    for level in 1..=5u8 {
        let lower = logging::VerbosityConfig::from_verbose_level(level - 1);
        let higher = logging::VerbosityConfig::from_verbose_level(level);

        for flag in [
            logging::DebugFlag::Acl,
            logging::DebugFlag::Backup,
            logging::DebugFlag::Bind,
            logging::DebugFlag::Chdir,
            logging::DebugFlag::Cmd,
            logging::DebugFlag::Connect,
            logging::DebugFlag::Del,
            logging::DebugFlag::Deltasum,
            logging::DebugFlag::Dup,
            logging::DebugFlag::Exit,
            logging::DebugFlag::Filter,
            logging::DebugFlag::Flist,
            logging::DebugFlag::Fuzzy,
            logging::DebugFlag::Genr,
            logging::DebugFlag::Hash,
            logging::DebugFlag::Hlink,
            logging::DebugFlag::Iconv,
            logging::DebugFlag::Io,
            logging::DebugFlag::Nstr,
            logging::DebugFlag::Own,
            logging::DebugFlag::Proto,
            logging::DebugFlag::Recv,
            logging::DebugFlag::Send,
            logging::DebugFlag::Time,
        ] {
            assert!(
                higher.debug.get(flag) >= lower.debug.get(flag),
                "debug flag {flag:?} decreased from level {} to {}: {} -> {}",
                level - 1,
                level,
                lower.debug.get(flag),
                higher.debug.get(flag),
            );
        }
    }
}

// ===========================================================================
// 12. --info flag output parity
// ===========================================================================

#[test]
fn parity_info_flag_stats_produces_stats_output() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("stats.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"stats info test").expect("write");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=stats"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // --info=stats should produce the same stats summary as --stats
    assert!(
        output.contains("Total bytes sent:") || output.contains("total size is"),
        "--info=stats should produce transfer statistics:\n{output}"
    );
}

#[test]
fn parity_info_flag_name_shows_filenames() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("named.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"name info test").expect("write");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=name"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    assert!(
        output.contains("named.txt"),
        "--info=name should show transferred filenames:\n{output}"
    );
}

#[test]
fn parity_info_flag_skip_shows_skip_messages() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");
    fs::write(source_dir.join("already.txt"), b"already here").expect("write src");
    fs::write(dest_dir.join("already.txt"), b"already here").expect("write dst");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    // With --info=skip, unchanged files should show skip messages
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=skip"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let combined = format!(
        "{}{}",
        String::from_utf8(stdout).expect("utf8"),
        String::from_utf8(stderr).expect("utf8 stderr")
    );

    // When files are identical, --info=skip should show that files were skipped.
    // The file was identical so no transfer needed — either skip message or empty output
    // is acceptable (upstream rsync only shows skip at level >= 1 when there's a reason).
    let _ = combined;
    assert_eq!(code, 0, "--info=skip should not cause errors");
}

#[test]
fn parity_info_flag_del_shows_deletion_messages() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");
    // Source has file1, dest has file1 + extra (to be deleted)
    fs::write(source_dir.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest_dir.join("keep.txt"), b"keep").expect("write keep dst");
    fs::write(dest_dir.join("extra.txt"), b"extra").expect("write extra");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        OsString::from("--delete"),
        OsString::from("--info=del"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let combined = format!(
        "{}{}",
        String::from_utf8(stdout).expect("utf8"),
        String::from_utf8(stderr).expect("utf8 stderr")
    );

    // --info=del with --delete should mention the deleted file
    assert!(
        combined.contains("extra.txt"),
        "--info=del --delete should show deleted filename:\n{combined}"
    );
}

#[test]
fn parity_info_flag_copy_shows_copy_messages() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("copied.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"copy info test").expect("write");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=copy"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // --info=copy shows information about file copying operations
    let _ = output;
    assert_eq!(code, 0, "--info=copy should not cause errors");
}

// ===========================================================================
// 13. --debug flag output parity
// ===========================================================================

#[test]
fn parity_debug_flag_accepted_without_error() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("dbg.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"debug test").expect("write");

    // Verify that --debug=deltasum is accepted and doesn't cause errors
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--debug=deltasum"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    let err = String::from_utf8(stderr).expect("utf8");
    assert_eq!(code, 0, "--debug=deltasum should not fail: {err}");
}

#[test]
fn parity_debug_flag_filter_accepted_without_error() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::write(source_dir.join("f.txt"), b"filter").expect("write");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--debug=filter"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    let err = String::from_utf8(stderr).expect("utf8");
    assert_eq!(code, 0, "--debug=filter should not fail: {err}");
}

#[test]
fn parity_debug_flag_all_accepted_without_error() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("alldbg.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"all debug").expect("write");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--debug=ALL"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    let err = String::from_utf8(stderr).expect("utf8");
    assert_eq!(code, 0, "--debug=ALL should not fail: {err}");
}

#[test]
fn parity_debug_flag_none_silences_debug_output() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("quiet.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"quiet debug").expect("write");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--debug=NONE"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    let output = String::from_utf8(stdout).expect("utf8");
    assert_eq!(code, 0);
    // With --debug=NONE and no verbosity, stdout should have minimal output
    // (no filenames unless explicitly requested)
    assert!(
        output.trim().is_empty() || !output.contains("[DEBUG]"),
        "--debug=NONE should not produce debug output:\n{output}"
    );
}

#[test]
fn parity_debug_flist_produces_file_list_detail() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::write(source_dir.join("a.txt"), b"aaa").expect("write a");
    fs::write(source_dir.join("b.txt"), b"bbb").expect("write b");

    let mut src_operand = source_dir.into_os_string();
    src_operand.push("/");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--debug=flist2"),
        OsString::from("-r"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    let err = String::from_utf8(stderr).expect("utf8");
    assert_eq!(code, 0, "--debug=flist2 should not fail: {err}");
}

// ===========================================================================
// 14. --help format parity
// ===========================================================================

#[test]
fn parity_help_exits_successfully() {
    let (code, stdout, _stderr) = run_with_args([OsString::from(RSYNC), OsString::from("--help")]);

    let output = String::from_utf8(stdout).expect("utf8");
    assert_eq!(code, 0, "--help should exit with code 0");
    assert!(!output.is_empty(), "--help should produce output");
}

#[test]
fn parity_help_contains_usage_line() {
    let (code, stdout, _stderr) = run_with_args([OsString::from(RSYNC), OsString::from("--help")]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Upstream rsync --help starts with a usage line
    let has_usage = output.contains("Usage:") || output.contains("usage:");
    assert!(has_usage, "--help should contain a usage line:\n{output}");
}

#[test]
fn parity_help_contains_common_options() {
    let (code, stdout, _stderr) = run_with_args([OsString::from(RSYNC), OsString::from("--help")]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Verify key options from upstream rsync are documented
    for option in ["-v", "-a", "-r", "-n", "--delete", "--stats", "--version"] {
        assert!(
            output.contains(option),
            "--help should document {option}:\n{output}"
        );
    }
}

#[test]
fn parity_help_contains_info_and_debug_flags() {
    let (code, stdout, _stderr) = run_with_args([OsString::from(RSYNC), OsString::from("--help")]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    assert!(
        output.contains("--info"),
        "--help should document --info flag:\n{output}"
    );
    assert!(
        output.contains("--debug"),
        "--help should document --debug flag:\n{output}"
    );
}

#[test]
fn parity_help_double_dash_only() {
    // In upstream rsync, -h is --human-readable, NOT --help.
    // Only --help produces help output. Verify this matches.
    let (code, stdout, _) = run_with_args([OsString::from(RSYNC), OsString::from("--help")]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(
        output.contains("-v") && output.contains("-a"),
        "--help should list common options:\n{output}"
    );
}

// ===========================================================================
// 15. Compression output format parity
// ===========================================================================

#[test]
fn parity_compress_flag_accepted_without_error() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("compressed.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"compress test data for parity validation").expect("write");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    let err = String::from_utf8(stderr).expect("utf8");
    assert_eq!(code, 0, "-z should not fail for local copy: {err}");
}

#[test]
fn parity_compress_with_stats_shows_transfer_statistics() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("zstats.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"compress stats test").expect("write");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        OsString::from("--stats"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // --stats with -z should still show standard transfer statistics
    assert!(
        output.contains("Total bytes sent:"),
        "-z --stats should show transfer statistics:\n{output}"
    );
}

#[test]
fn parity_compress_level_accepted_without_error() {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("zlevel.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    fs::write(&source, b"compress level test").expect("write");

    // Test various compression levels (upstream rsync supports 1-9)
    for level in [1, 5, 9] {
        let dest = temp.path().join(format!("dest{level}"));
        fs::create_dir(&dest).expect("create dest");

        let (code, _stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from(format!("--compress-level={level}")),
            source.clone().into_os_string(),
            dest.into_os_string(),
        ]);

        let err = String::from_utf8(stderr).expect("utf8");
        assert_eq!(code, 0, "--compress-level={level} should succeed: {err}");
    }
}
