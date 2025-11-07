
#[test]
fn execute_skips_rewriting_identical_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"identical").expect("write source");
    fs::write(&destination, b"identical").expect("write destination");

    let source_metadata = fs::metadata(&source).expect("source metadata");
    let source_mtime = FileTime::from_last_modification_time(&source_metadata);
    set_file_mtime(&destination, source_mtime).expect("align destination mtime");

    let mut dest_perms = fs::metadata(&destination)
        .expect("destination metadata")
        .permissions();
    dest_perms.set_readonly(true);
    fs::set_permissions(&destination, dest_perms).expect("set destination readonly");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true).times(true),
        )
        .expect("copy succeeds without rewriting");

    let final_perms = fs::metadata(&destination)
        .expect("destination metadata")
        .permissions();
    assert!(
        !final_perms.readonly(),
        "destination permissions should match writable source"
    );
    assert_eq!(
        fs::read(&destination).expect("destination contents"),
        b"identical"
    );
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn execute_without_times_rewrites_when_checksum_disabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write destination");

    let original_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&destination, original_mtime).expect("set mtime");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    let metadata = fs::metadata(&destination).expect("dest metadata");
    let new_mtime = FileTime::from_last_modification_time(&metadata);
    assert_ne!(new_mtime, original_mtime);
}

#[test]
fn execute_without_times_skips_with_checksum() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write destination");

    let preserved_mtime = FileTime::from_unix_time(1_700_100_000, 0);
    set_file_mtime(&destination, preserved_mtime).expect("set mtime");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 1);
    let metadata = fs::metadata(&destination).expect("dest metadata");
    let final_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(final_mtime, preserved_mtime);
}

#[test]
fn execute_with_size_only_skips_same_size_different_content() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.txt");
    let dest_path = target_root.join("file.txt");
    fs::write(&source_path, b"abc").expect("write source");
    fs::write(&dest_path, b"xyz").expect("write destination");

    let operands = vec![
        source_path.clone().into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.bytes_copied(), 0);
    assert_eq!(fs::read(dest_path).expect("read destination"), b"xyz");
}

#[test]
fn execute_with_ignore_existing_skips_existing_destination() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.txt");
    let dest_path = target_root.join("file.txt");
    fs::write(&source_path, b"updated").expect("write source");
    fs::write(&dest_path, b"original").expect("write destination");

    let operands = vec![
        source_path.clone().into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_existing(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    assert_eq!(summary.regular_files_ignored_existing(), 1);
    assert_eq!(fs::read(dest_path).expect("read destination"), b"original");
}

#[test]
fn execute_with_ignore_missing_args_skips_absent_sources() {
    let temp = tempdir().expect("tempdir");
    let missing = temp.path().join("missing.txt");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&destination_root).expect("create destination root");
    let destination = destination_root.join("output.txt");
    fs::write(&destination, b"existing").expect("write destination");

    let operands = vec![
        missing.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_missing_args(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.bytes_copied(), 0);
    assert_eq!(
        fs::read(destination).expect("read destination"),
        b"existing"
    );
}

#[test]
fn execute_with_existing_only_skips_missing_entries() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested_dir = source_root.join("nested");
    fs::create_dir_all(&nested_dir).expect("create nested dir");
    fs::write(source_root.join("file.txt"), b"payload").expect("write file");
    fs::write(nested_dir.join("inner.txt"), b"nested").expect("write nested file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create destination root");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .existing_only(true)
                .collect_events(true),
        )
        .expect("execution succeeds");
    let summary = report.summary();

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_skipped_missing(), 1);
    assert_eq!(summary.directories_total(), 2);
    assert_eq!(summary.directories_created(), 0);
    assert!(!dest_root.join("file.txt").exists());
    assert!(!dest_root.join("nested").exists());

    let records = report.records();
    assert!(records.iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedMissingDestination
            && record.relative_path() == std::path::Path::new("file.txt")
    }));
    assert!(records.iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedMissingDestination
            && record.relative_path() == std::path::Path::new("nested")
    }));
}

#[test]
fn execute_skips_files_smaller_than_min_size_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("tiny.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"abc").expect("write source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().min_file_size(Some(10)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.bytes_copied(), 0);
    assert!(!destination.exists());
}

#[test]
fn execute_skips_files_larger_than_max_size_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, vec![0u8; 4096]).expect("write large source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(2048)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.bytes_copied(), 0);
    assert!(!destination.exists());
}

#[test]
fn execute_copies_files_matching_size_boundaries() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("boundary.bin");
    let destination = temp.path().join("dest.bin");

    let payload = vec![0xAA; 2048];
    fs::write(&source, &payload).expect("write boundary source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .min_file_size(Some(2048))
                .max_file_size(Some(2048)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.bytes_copied(), 2048);
    assert_eq!(fs::read(&destination).expect("read destination"), payload);
}

#[test]
fn execute_with_update_skips_newer_destination() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.txt");
    let dest_path = target_root.join("file.txt");
    fs::write(&source_path, b"updated").expect("write source");
    fs::write(&dest_path, b"existing").expect("write destination");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source_path, older, older).expect("set source times");
    set_file_times(&dest_path, newer, newer).expect("set dest times");

    let operands = vec![
        source_path.clone().into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(fs::read(dest_path).expect("read destination"), b"existing");
}
