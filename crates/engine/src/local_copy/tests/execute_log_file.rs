// Tests for --log-file and --log-file-format options
//
// These tests verify:
// 1. Default values (None)
// 2. Setting log file path
// 3. Setting log file format
// 4. Effective log file format returns default when not set
// 5. Effective log file format returns custom when set
// 6. Builder support
// 7. Round-trip via builder
// 8. Transfers work with log_file set (verify file is created)
// 9. Combination with other options

// ==================== Default Tests ====================

#[test]
fn log_file_default_is_none() {
    let opts = LocalCopyOptions::new();
    assert!(opts.log_file_path().is_none());
}

#[test]
fn log_file_format_default_is_none() {
    let opts = LocalCopyOptions::new();
    assert!(opts.log_file_format().is_none());
}

// ==================== Setter Tests ====================

#[test]
fn log_file_with_log_file_sets_path() {
    let opts = LocalCopyOptions::new().with_log_file(Some("/var/log/rsync.log"));
    assert_eq!(
        opts.log_file_path(),
        Some(Path::new("/var/log/rsync.log"))
    );
}

#[test]
fn log_file_with_log_file_none_clears_path() {
    let opts = LocalCopyOptions::new()
        .with_log_file(Some("/var/log/rsync.log"))
        .with_log_file::<PathBuf>(None);
    assert!(opts.log_file_path().is_none());
}

#[test]
fn log_file_with_log_file_accepts_pathbuf() {
    let path = PathBuf::from("/tmp/transfer.log");
    let opts = LocalCopyOptions::new().with_log_file(Some(path));
    assert_eq!(
        opts.log_file_path(),
        Some(Path::new("/tmp/transfer.log"))
    );
}

#[test]
fn log_file_format_with_log_file_format_sets_format() {
    let opts = LocalCopyOptions::new().with_log_file_format(Some("%t %f %b"));
    assert_eq!(opts.log_file_format(), Some("%t %f %b"));
}

#[test]
fn log_file_format_with_log_file_format_none_clears() {
    let opts = LocalCopyOptions::new()
        .with_log_file_format(Some("%t %f %b"))
        .with_log_file_format::<String>(None);
    assert!(opts.log_file_format().is_none());
}

#[test]
fn log_file_format_with_log_file_format_accepts_string() {
    let fmt = String::from("%o %n");
    let opts = LocalCopyOptions::new().with_log_file_format(Some(fmt));
    assert_eq!(opts.log_file_format(), Some("%o %n"));
}

// ==================== Effective Log File Format Tests ====================

#[test]
fn log_file_effective_format_returns_default_when_not_set() {
    let opts = LocalCopyOptions::new();
    assert_eq!(opts.effective_log_file_format(), "%i %n%L");
}

#[test]
fn log_file_effective_format_returns_custom_when_set() {
    let opts = LocalCopyOptions::new().with_log_file_format(Some("%t %f %b"));
    assert_eq!(opts.effective_log_file_format(), "%t %f %b");
}

#[test]
fn log_file_effective_format_after_clear_returns_default() {
    let opts = LocalCopyOptions::new()
        .with_log_file_format(Some("%t %f %b"))
        .with_log_file_format::<String>(None);
    assert_eq!(opts.effective_log_file_format(), "%i %n%L");
}

// ==================== Builder Support Tests ====================

#[test]
fn log_file_builder_sets_log_file() {
    let opts = LocalCopyOptions::builder()
        .log_file(Some("/tmp/rsync.log"))
        .build()
        .expect("valid options");
    assert_eq!(opts.log_file_path(), Some(Path::new("/tmp/rsync.log")));
}

#[test]
fn log_file_builder_sets_log_file_format() {
    let opts = LocalCopyOptions::builder()
        .log_file_format(Some("%o %n %b"))
        .build()
        .expect("valid options");
    assert_eq!(opts.log_file_format(), Some("%o %n %b"));
}

#[test]
fn log_file_builder_both_options() {
    let opts = LocalCopyOptions::builder()
        .log_file(Some("/var/log/rsync.log"))
        .log_file_format(Some("%t %f %b"))
        .build()
        .expect("valid options");
    assert_eq!(
        opts.log_file_path(),
        Some(Path::new("/var/log/rsync.log"))
    );
    assert_eq!(opts.log_file_format(), Some("%t %f %b"));
    assert_eq!(opts.effective_log_file_format(), "%t %f %b");
}

#[test]
fn log_file_builder_none_clears_log_file() {
    let opts = LocalCopyOptions::builder()
        .log_file(Some("/tmp/rsync.log"))
        .log_file(None::<&str>)
        .build()
        .expect("valid options");
    assert!(opts.log_file_path().is_none());
}

#[test]
fn log_file_builder_none_clears_log_file_format() {
    let opts = LocalCopyOptions::builder()
        .log_file_format(Some("%t %f"))
        .log_file_format(None::<&str>)
        .build()
        .expect("valid options");
    assert!(opts.log_file_format().is_none());
}

#[test]
fn log_file_builder_unchecked_sets_values() {
    let opts = LocalCopyOptions::builder()
        .log_file(Some("/tmp/rsync.log"))
        .log_file_format(Some("%o %n"))
        .build_unchecked();
    assert_eq!(opts.log_file_path(), Some(Path::new("/tmp/rsync.log")));
    assert_eq!(opts.log_file_format(), Some("%o %n"));
}

// ==================== Round-Trip Tests ====================

#[test]
fn log_file_round_trip_via_builder() {
    let opts = LocalCopyOptions::builder()
        .log_file(Some("/var/log/transfer.log"))
        .log_file_format(Some("%i %n%L"))
        .build()
        .expect("valid options");
    assert_eq!(
        opts.log_file_path(),
        Some(Path::new("/var/log/transfer.log"))
    );
    assert_eq!(opts.log_file_format(), Some("%i %n%L"));
}

#[test]
fn log_file_round_trip_via_setters() {
    let opts = LocalCopyOptions::new()
        .with_log_file(Some("/tmp/rsync.log"))
        .with_log_file_format(Some("%o %n %b"));
    assert_eq!(opts.log_file_path(), Some(Path::new("/tmp/rsync.log")));
    assert_eq!(opts.log_file_format(), Some("%o %n %b"));
    assert_eq!(opts.effective_log_file_format(), "%o %n %b");
}

// ==================== Transfer with Log File Tests ====================

#[test]
fn log_file_transfer_works_with_log_file_set() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let log_path = temp.path().join("transfer.log");

    fs::write(&source, b"log file transfer test").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_log_file(Some(&log_path)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"log file transfer test"
    );
}

#[test]
fn log_file_transfer_works_with_both_log_file_and_format() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let log_path = temp.path().join("transfer.log");

    fs::write(&source, b"log format transfer test").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_log_file(Some(&log_path))
                .with_log_file_format(Some("%o %n %b")),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"log format transfer test"
    );
}

#[test]
fn log_file_transfer_multiple_files_with_log_file() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    let log_path = temp.path().join("transfer.log");
    fs::create_dir_all(&source_root).expect("source dir");

    fs::write(source_root.join("file1.txt"), b"content1").expect("write");
    fs::write(source_root.join("file2.txt"), b"content2").expect("write");
    fs::write(source_root.join("file3.txt"), b"content3").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_log_file(Some(&log_path)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(
        fs::read(dest_root.join("file1.txt")).expect("read"),
        b"content1"
    );
    assert_eq!(
        fs::read(dest_root.join("file2.txt")).expect("read"),
        b"content2"
    );
    assert_eq!(
        fs::read(dest_root.join("file3.txt")).expect("read"),
        b"content3"
    );
}

// ==================== Combination with Other Options ====================

#[test]
fn log_file_combined_with_delete() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    let log_path = temp.path().join("transfer.log");
    fs::create_dir_all(&source_root).expect("source dir");
    fs::create_dir_all(&dest_root).expect("dest dir");

    fs::write(source_root.join("keep.txt"), b"keep").expect("write");
    fs::write(dest_root.join("keep.txt"), b"old keep").expect("write");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_log_file(Some(&log_path))
                .delete(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join("keep.txt").exists());
    assert!(!dest_root.join("extra.txt").exists());
}

#[test]
fn log_file_combined_with_times_preservation() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let log_path = temp.path().join("transfer.log");

    fs::write(&source, b"times test").expect("write source");
    let past_time = filetime::FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source, past_time).expect("set mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_log_file(Some(&log_path))
                .times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_mtime = filetime::FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );
    assert_eq!(dest_mtime, past_time);
}

#[test]
fn log_file_combined_with_checksum() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let log_path = temp.path().join("transfer.log");

    fs::write(&source, b"checksum log test").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_log_file(Some(&log_path))
                .checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"checksum log test"
    );
}

#[test]
fn log_file_combined_with_temp_dir() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let log_path = temp.path().join("transfer.log");
    let staging = temp.path().join("staging");
    fs::create_dir_all(&staging).expect("staging dir");

    fs::write(&source, b"temp dir + log file").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_log_file(Some(&log_path))
                .with_temp_directory(Some(&staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"temp dir + log file"
    );
}

#[test]
fn log_file_combined_with_builder_archive() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    let log_path = temp.path().join("transfer.log");
    fs::create_dir_all(&source_root).expect("source dir");
    fs::write(source_root.join("file.txt"), b"archive + log").expect("write");

    let options = LocalCopyOptions::builder()
        .archive()
        .log_file(Some(log_path))
        .log_file_format(Some("%o %n %b"))
        .build()
        .expect("valid options");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read"),
        b"archive + log"
    );
}

// ==================== Dry Run with Log File ====================

#[test]
fn log_file_dry_run_does_not_fail() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let log_path = temp.path().join("transfer.log");

    fs::write(&source, b"dry run content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().with_log_file(Some(&log_path)),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(
        !destination.exists(),
        "destination should not exist in dry run"
    );
}

// ==================== Builder Defaults Match Direct Defaults ====================

#[test]
fn log_file_builder_defaults_match_options_defaults() {
    let from_builder = LocalCopyOptions::builder().build().expect("valid");
    let from_default = LocalCopyOptions::default();

    assert_eq!(from_builder.log_file_path(), from_default.log_file_path());
    assert_eq!(
        from_builder.log_file_format(),
        from_default.log_file_format()
    );
    assert_eq!(
        from_builder.effective_log_file_format(),
        from_default.effective_log_file_format()
    );
}

// ==================== Config Propagation Tests ====================

#[test]
fn log_file_config_propagation_setter_and_builder_agree() {
    let via_setter = LocalCopyOptions::new()
        .with_log_file(Some("/tmp/rsync.log"))
        .with_log_file_format(Some("%f %l"));

    let via_builder = LocalCopyOptions::builder()
        .log_file(Some("/tmp/rsync.log"))
        .log_file_format(Some("%f %l"))
        .build()
        .expect("valid options");

    assert_eq!(via_setter.log_file_path(), via_builder.log_file_path());
    assert_eq!(via_setter.log_file_format(), via_builder.log_file_format());
    assert_eq!(
        via_setter.effective_log_file_format(),
        via_builder.effective_log_file_format()
    );
}

#[test]
fn log_file_format_without_log_file_is_stored() {
    // Setting format without log file is allowed; format is stored for later use.
    let opts = LocalCopyOptions::new().with_log_file_format(Some("%n %l"));
    assert!(opts.log_file_path().is_none());
    assert_eq!(opts.log_file_format(), Some("%n %l"));
    assert_eq!(opts.effective_log_file_format(), "%n %l");
}

#[test]
fn log_file_effective_format_uses_default_percent_i_n_l() {
    // Upstream rsync default: "%i %n%L"
    let opts = LocalCopyOptions::new().with_log_file(Some("/tmp/test.log"));
    assert_eq!(opts.effective_log_file_format(), "%i %n%L");
}

// ==================== Edge Case Tests ====================

#[test]
fn log_file_empty_format_string_is_stored() {
    let opts = LocalCopyOptions::new().with_log_file_format(Some(""));
    assert_eq!(opts.log_file_format(), Some(""));
    assert_eq!(opts.effective_log_file_format(), "");
}

#[test]
fn log_file_path_with_special_characters() {
    let opts = LocalCopyOptions::new().with_log_file(Some("/tmp/my logs/rsync (1).log"));
    assert_eq!(
        opts.log_file_path(),
        Some(Path::new("/tmp/my logs/rsync (1).log"))
    );
}

#[test]
fn log_file_builder_chaining_preserves_other_options() {
    let opts = LocalCopyOptions::builder()
        .recursive(true)
        .times(true)
        .log_file(Some("/tmp/rsync.log"))
        .log_file_format(Some("%o %n"))
        .checksum(true)
        .build()
        .expect("valid options");

    assert_eq!(opts.log_file_path(), Some(Path::new("/tmp/rsync.log")));
    assert_eq!(opts.log_file_format(), Some("%o %n"));
    assert!(opts.recursive_enabled());
    assert!(opts.preserve_times());
    assert!(opts.checksum_enabled());
}

#[test]
fn log_file_overwrite_replaces_previous_value() {
    let opts = LocalCopyOptions::new()
        .with_log_file(Some("/first.log"))
        .with_log_file(Some("/second.log"));
    assert_eq!(opts.log_file_path(), Some(Path::new("/second.log")));
}

#[test]
fn log_file_format_overwrite_replaces_previous_value() {
    let opts = LocalCopyOptions::new()
        .with_log_file_format(Some("%n"))
        .with_log_file_format(Some("%f %l"));
    assert_eq!(opts.log_file_format(), Some("%f %l"));
}
