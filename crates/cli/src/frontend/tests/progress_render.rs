use super::*;
use super::{
    NameOutputLevel, OutFormatContext, ProgressSetting, emit_transfer_summary, parse_out_format,
};
use crate::frontend::escape::EscapeStyle;
use crate::frontend::progress::FlistBanner;
use core::client::{ClientConfig, ClientSummary, HumanReadableMode, run_client};
use tempfile::TempDir;

fn create_sample_summary() -> (ClientSummary, TempDir) {
    use std::fs;

    let temp = tempfile::tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source directory");
    fs::create_dir_all(&dest_dir).expect("create destination directory");

    let source_file = source_dir.join("sample.txt");
    fs::write(&source_file, b"transfer payload").expect("write source file");

    let config = ClientConfig::builder()
        .transfer_args([source_file, dest_dir])
        .verbosity(2)
        .progress(true)
        .stats(true)
        .human_readable(true)
        .force_event_collection(true)
        .build();

    let summary = run_client(config).expect("run_client succeeds");
    assert!(
        !summary.events().is_empty(),
        "expected sample summary to include transfer events"
    );

    (summary, temp)
}

#[test]
fn emit_transfer_summary_list_only_emits_listing_and_stats() {
    let (summary, _temp) = create_sample_summary();
    let mut rendered = Vec::new();

    emit_transfer_summary(
        &summary,
        1,
        None,
        2, // stats_level
        false,
        true,
        false, // dry_run
        false, // only_write_batch
        None,
        &OutFormatContext::default(),
        NameOutputLevel::UpdatedAndUnchanged,
        false,
        HumanReadableMode::DecimalUnits,
        false,
        FlistBanner::None,                   // flist_banner (list_only path)
        DeltaTransmissionSummary::default(), // delta_notice
        false,                               // show_copy_method
        false,                               // show_atimes
        false,                               // show_crtimes
        EscapeStyle::terminal(false),        // escape style
        &mut rendered,
        &mut PendingDiagnostics::empty(),
    )
    .expect("render summary");

    let output = String::from_utf8(rendered).expect("utf8");
    assert!(output.contains("sample.txt"));
    assert!(output.contains("Number of files"));
    assert!(output.contains("Number of created files"));
    assert!(output.contains("Total bytes sent"));
}

#[test]
fn emit_transfer_summary_with_progress_and_verbose_listing() {
    let (summary, _temp) = create_sample_summary();
    let mut rendered = Vec::new();

    emit_transfer_summary(
        &summary,
        2,
        ProgressSetting::PerFile.resolved(),
        0, // stats_level
        false,
        false,
        false, // dry_run
        false, // only_write_batch
        None,
        &OutFormatContext::default(),
        NameOutputLevel::UpdatedAndUnchanged,
        false,
        HumanReadableMode::DecimalUnits,
        false,
        FlistBanner::Incremental,            // flist_banner
        DeltaTransmissionSummary::default(), // delta_notice
        false,                               // show_copy_method
        false,                               // show_atimes
        false,                               // show_crtimes
        EscapeStyle::terminal(false),        // escape style
        &mut rendered,
        &mut PendingDiagnostics::empty(),
    )
    .expect("render summary");

    let output = String::from_utf8(rendered).expect("utf8");
    assert!(output.contains("(xfr#1, to-chk="));
    // upstream emits bare `%n%L` per-file even at -vv (options.c:2372).
    // Do not emit descriptor prefixes like `copied:` - upstream testsuite
    // `duplicates.test` greps for `^name1$` to detect duplicate copies.
    assert!(
        !output.contains("copied:"),
        "verbosity 2 must not prefix lines with `copied:` - upstream `duplicates.test` greps for bare `^<name>$`:\n{output}"
    );
    assert!(output.contains("sample.txt"));
    assert!(output.contains("sent "));
    assert!(output.contains("speedup is"));
}

#[test]
fn emit_transfer_summary_out_format_adds_separator_before_stats() {
    let (summary, _temp) = create_sample_summary();
    let format = parse_out_format(std::ffi::OsStr::new("%f")).expect("parse format");
    let mut rendered = Vec::new();

    emit_transfer_summary(
        &summary,
        1,
        None,
        2, // stats_level
        false,
        false,
        false, // dry_run
        false, // only_write_batch
        Some(&format),
        &OutFormatContext::default(),
        NameOutputLevel::Disabled,
        false,
        HumanReadableMode::Grouped,
        false,
        FlistBanner::None, // flist_banner (out_format path: starts_with assertion)
        DeltaTransmissionSummary::default(), // delta_notice
        false,             // show_copy_method
        false,             // show_atimes
        false,             // show_crtimes
        EscapeStyle::terminal(false), // escape style
        &mut rendered,
        &mut PendingDiagnostics::empty(),
    )
    .expect("render summary");

    let output = String::from_utf8(rendered).expect("utf8");
    assert!(output.starts_with("sample.txt"));
    assert!(output.contains("sample.txt\n\nNumber of files"));
    assert!(output.contains("Total bytes sent"));
}

/// upstream: main.c:337-340 `handle_stats()` gates `show_malloc_stats()` on
/// `INFO_GTE(STATS, 3)`, so `--stats`/`-vv` (levels 1 and 2) must not print it.
///
/// The level-3 assertion is an equality against sample availability rather than
/// a bare `contains`: the block is emitted exactly when the allocator can be
/// introspected, so this pins the GATE without depending on which allocator the
/// test binary happens to link.
#[test]
fn heap_statistics_are_gated_on_stats_level_three() {
    use crate::frontend::progress::emit_stats;
    use fast_io::heap_stats::heap_stats;

    let (summary, _temp) = create_sample_summary();
    let render_at = |level: u8| {
        let mut out = Vec::new();
        emit_stats(
            &summary,
            &mut out,
            HumanReadableMode::DecimalUnits,
            false,
            false,
            level,
            false,
        )
        .expect("emit_stats");
        String::from_utf8(out).expect("utf8")
    };

    for level in [1u8, 2] {
        assert!(
            !render_at(level).contains("heap statistics"),
            "level {level} must not emit the heap block"
        );
    }
    assert_eq!(
        render_at(3).contains("heap statistics"),
        heap_stats().is_some(),
        "level 3 must emit the heap block whenever a sample is available"
    );
}
