
#[test]
fn execute_delta_copy_reuses_existing_blocks() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.bin");
    let dest_path = target_root.join("file.bin");

    let mut prefix = vec![b'A'; 700];
    let mut suffix = vec![b'B'; 700];
    let mut replacement = vec![b'C'; 700];
    let prefix_len = prefix.len() as u64;
    let replacement_len = replacement.len() as u64;

    let mut initial = Vec::new();
    initial.append(&mut prefix.clone());
    initial.append(&mut suffix);
    fs::write(&dest_path, &initial).expect("write initial destination");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("set destination mtime");

    let mut updated = Vec::new();
    updated.append(&mut prefix);
    updated.append(&mut replacement);
    fs::write(&source_path, &updated).expect("write updated source");
    set_file_mtime(&source_path, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(false),
        )
        .expect("delta copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), replacement_len);
    assert_eq!(summary.matched_bytes(), prefix_len);
    assert_eq!(fs::read(&dest_path).expect("read destination"), updated);
}

/// GUARD (#185/#186): the local `--no-whole-file` delta summary must split a
/// transferred file into literal + matched bytes exactly as upstream's
/// `stats.literal_data` (match.c:436) and `stats.matched_data` (match.c:121),
/// which together always cover the whole file. #185 MEASURED these already
/// match upstream on a local delta copy (Literal 65,536 / Matched 983,040), so
/// this test is a regression guard, not a fix: a future matcher change that
/// mis-splits the file (e.g. losing the prefix match and inflating literal)
/// must fail here rather than silently diverge from upstream stats.
#[test]
fn delta_summary_literal_matched_partition_matches_upstream() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.bin");
    let dest_path = target_root.join("file.bin");

    // Basis = 700 A + 700 B; updated source = 700 A + 700 C. The leading 700-byte
    // block is reusable; the trailing 700 bytes changed, so upstream matches the
    // prefix and sends the suffix as literal data.
    let prefix = vec![b'A'; 700];
    let mut initial = prefix.clone();
    initial.extend(std::iter::repeat_n(b'B', 700));
    fs::write(&dest_path, &initial).expect("write basis");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("backdate basis");

    let mut updated = prefix.clone();
    updated.extend(std::iter::repeat_n(b'C', 700));
    fs::write(&source_path, &updated).expect("write source");
    set_file_mtime(&source_path, FileTime::from_unix_time(2, 0)).expect("bump source mtime");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(false),
        )
        .expect("delta copy succeeds");

    assert_eq!(
        summary.bytes_copied(),
        700,
        "literal data = the changed 700-byte suffix"
    );
    assert_eq!(
        summary.matched_bytes(),
        700,
        "matched data = the reused 700-byte prefix block"
    );
    assert_eq!(
        summary.bytes_copied() + summary.matched_bytes(),
        updated.len() as u64,
        "literal + matched must cover the whole transferred file, exactly as \
         upstream's stats.literal_data + stats.matched_data do"
    );
    assert_eq!(fs::read(&dest_path).expect("read dest"), updated);
}

/// GUARD (#186): a whole-file local copy reports every byte as literal and none
/// matched - upstream's sender matches against an empty basis, so the entire
/// file is `stats.literal_data`. Pins the other end of the literal/matched
/// partition so whole-file accounting cannot silently regress either.
#[test]
fn whole_file_summary_is_all_literal_none_matched() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("whole.bin");
    let dest = temp.path().join("dest.bin");
    let payload = vec![b'Z'; 4096];
    fs::write(&source, &payload).expect("write source");

    let operands = vec![source.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(true),
        )
        .expect("whole-file copy succeeds");

    assert_eq!(
        summary.bytes_copied(),
        payload.len() as u64,
        "whole-file copy counts every byte as literal data"
    );
    assert_eq!(
        summary.matched_bytes(),
        0,
        "a fresh destination matches nothing"
    );
}

#[test]
fn execute_with_report_dry_run_records_file_event() {
    use std::fs;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"dry-run").expect("write source");
    let destination = temp.path().join("dest.txt");

    let operands = vec![
        source.into_os_string(),
        destination.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.action(), &LocalCopyAction::DataCopied);
    assert_eq!(record.relative_path(), Path::new("source.txt"));
    assert_eq!(record.bytes_transferred(), 7);
}

#[test]
fn execute_with_report_dry_run_records_directory_event() {
    use std::fs;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("tree");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"data").expect("write nested file");
    let destination = temp.path().join("target");

    let operands = vec![
        source_dir.into_os_string(),
        destination.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let records = report.records();
    assert!(records.iter().any(|record| {
        record.action() == &LocalCopyAction::DirectoryCreated
            && record.relative_path() == Path::new("tree")
    }));
}

#[test]
fn execute_with_report_records_min_size_skip_notice_without_copying() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("tiny.txt");
    fs::write(&source, b"abc").expect("write source");
    let destination = temp.path().join("dest.txt");

    let operands = vec![
        source.into_os_string(),
        destination.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .collect_events(true)
        .min_file_size(Some(10));
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    // A `--min-size` skip records a `SkippedUnderMinSize` notice action, exactly
    // like the other generator-phase skips (`SkippedNewerDestination`,
    // `SkippedExisting`, `SkippedMissingDestination`) upstream emits during the
    // generator loop. The record is the only channel that carries the
    // "%s is under min-size" notice to the renderer, which prints it ahead of
    // the statistics block (upstream: generator.c:1712-1719; ClientEvents derive
    // from records() in core `ClientSummary::from_report`). It is a skip notice,
    // not a copy: no data is written and no file is counted as copied.
    let records = report.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].action(), &LocalCopyAction::SkippedUnderMinSize);
    assert_eq!(report.summary().files_copied(), 0);
    assert_eq!(report.summary().regular_files_total(), 1);
    assert_eq!(report.summary().bytes_copied(), 0);
}
