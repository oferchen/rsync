
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

    let mut initial = Vec::new();
    initial.append(&mut prefix.clone());
    initial.append(&mut suffix);
    fs::write(&dest_path, &initial).expect("write initial destination");

    let mut updated = Vec::new();
    updated.append(&mut prefix);
    updated.append(&mut replacement);
    fs::write(&source_path, &updated).expect("write updated source");

    let operands = vec![
        source_path.clone().into_os_string(),
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
    assert_eq!(fs::read(&dest_path).expect("read destination"), updated);
}

#[test]
fn execute_with_report_dry_run_records_file_event() {
    use std::fs;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"dry-run").expect("write source");
    let destination = temp.path().join("dest.txt");

    let operands = vec![
        source.clone().into_os_string(),
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
        source_dir.clone().into_os_string(),
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
fn execute_with_report_dry_run_skips_records_for_filtered_small_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("tiny.txt");
    fs::write(&source, b"abc").expect("write source");
    let destination = temp.path().join("dest.txt");

    let operands = vec![
        source.clone().into_os_string(),
        destination.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .collect_events(true)
        .min_file_size(Some(10));
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    assert!(report.records().is_empty());
    assert_eq!(report.summary().files_copied(), 0);
    assert_eq!(report.summary().regular_files_total(), 1);
    assert_eq!(report.summary().bytes_copied(), 0);
}
