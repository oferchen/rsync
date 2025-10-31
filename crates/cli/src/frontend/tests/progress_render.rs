use super::*;
use super::{
    NameOutputLevel, OutFormatContext, ProgressSetting, emit_transfer_summary, parse_out_format,
};
use rsync_core::client::{ClientConfig, ClientSummary, HumanReadableMode, run_client};
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
        .transfer_args([source_file.clone(), dest_dir.clone()])
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
        true,
        false,
        true,
        None,
        &OutFormatContext::default(),
        NameOutputLevel::UpdatedAndUnchanged,
        false,
        HumanReadableMode::Enabled,
        false,
        &mut rendered,
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
        false,
        false,
        false,
        None,
        &OutFormatContext::default(),
        NameOutputLevel::UpdatedAndUnchanged,
        false,
        HumanReadableMode::Enabled,
        false,
        &mut rendered,
    )
    .expect("render summary");

    let output = String::from_utf8(rendered).expect("utf8");
    assert!(output.contains("(xfr#1, to-chk="));
    assert!(output.contains("copied:"));
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
        true,
        false,
        false,
        Some(&format),
        &OutFormatContext::default(),
        NameOutputLevel::Disabled,
        false,
        HumanReadableMode::Disabled,
        false,
        &mut rendered,
    )
    .expect("render summary");

    let output = String::from_utf8(rendered).expect("utf8");
    assert!(output.starts_with("sample.txt"));
    assert!(output.contains("sample.txt\n\nNumber of files"));
    assert!(output.contains("Total bytes sent"));
}
