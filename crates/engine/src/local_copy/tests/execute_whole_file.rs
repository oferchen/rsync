
#[test]
fn execute_whole_file_transfers_complete_file_without_delta() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Create source file with known content
    let source_content = b"complete file transfer without delta matching";
    fs::write(&source, source_content).expect("write source");

    // Create existing destination with different content that could match
    fs::write(&destination, b"some existing content").expect("write destination");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("set dest mtime");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(true),
        )
        .expect("whole file copy succeeds");

    // Verify complete file transfer
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), source_content.len() as u64);
    assert_eq!(summary.matched_bytes(), 0, "whole file should not match any blocks");
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        source_content
    );
}

#[test]
fn execute_whole_file_ignores_basis_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    // Create files with common prefix that could be used for delta transfer
    let common_prefix = vec![b'X'; 1024];
    let unique_suffix = vec![b'Y'; 512];

    let mut dest_content = common_prefix.clone();
    dest_content.extend_from_slice(&vec![b'Z'; 512]);
    fs::write(&destination, &dest_content).expect("write destination");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("set dest mtime");

    let mut source_content = common_prefix.clone();
    source_content.extend_from_slice(&unique_suffix);
    fs::write(&source, &source_content).expect("write source");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(true),
        )
        .expect("whole file copy succeeds");

    // Verify that no delta matching occurred despite common prefix
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), source_content.len() as u64);
    assert_eq!(summary.matched_bytes(), 0, "basis file should be ignored");
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        source_content
    );
}

#[test]
fn execute_whole_file_transfers_large_file_completely() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.bin");
    let destination = temp.path().join("dest.bin");

    // Create a large file (1MB) that's bigger than typical buffers
    let large_content: Vec<u8> = (0..=255).cycle().take(1024 * 1024).collect();
    fs::write(&source, &large_content).expect("write large source");

    // Create destination with partially matching content
    let partial_content: Vec<u8> = (0..=255).cycle().take(512 * 1024).collect();
    fs::write(&destination, &partial_content).expect("write destination");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("set dest mtime");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(true),
        )
        .expect("large file copy succeeds");

    // Verify complete transfer of large file
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 1024 * 1024);
    assert_eq!(summary.matched_bytes(), 0, "large file should not use basis");
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        large_content
    );
}

#[test]
fn execute_whole_file_works_for_new_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"new file content";
    fs::write(&source, content).expect("write source");
    // Destination does not exist

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(true),
        )
        .expect("new file copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), content.len() as u64);
    assert_eq!(summary.matched_bytes(), 0);
    assert_eq!(fs::read(&destination).expect("read destination"), content);
}

#[test]
fn execute_whole_file_in_recursive_directory_copy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Create multiple files in source
    fs::write(source_root.join("file1.txt"), b"first file").expect("write file1");
    fs::write(source_root.join("file2.txt"), b"second file").expect("write file2");

    let subdir = source_root.join("subdir");
    fs::create_dir_all(&subdir).expect("create subdir");
    fs::write(subdir.join("file3.txt"), b"third file").expect("write file3");

    // Create corresponding destination directory structure
    let dest_source = dest_root.join("source");
    fs::create_dir_all(&dest_source).expect("create dest source dir");
    fs::write(dest_source.join("file1.txt"), b"old content 1").expect("write dest file1");
    fs::write(dest_source.join("file2.txt"), b"old content 2").expect("write dest file2");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(true).ignore_times(true),
        )
        .expect("recursive whole file copy succeeds");

    // All files should be transferred completely
    assert!(summary.files_copied() >= 3);
    assert_eq!(summary.matched_bytes(), 0, "recursive copy should not use delta");

    // Verify content
    assert_eq!(
        fs::read(dest_root.join("source/file1.txt")).expect("read file1"),
        b"first file"
    );
    assert_eq!(
        fs::read(dest_root.join("source/file2.txt")).expect("read file2"),
        b"second file"
    );
    assert_eq!(
        fs::read(dest_root.join("source/subdir/file3.txt")).expect("read file3"),
        b"third file"
    );
}

#[test]
fn execute_no_whole_file_forces_delta_transfer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    // Create files with significant overlap for delta matching
    let common_prefix = vec![b'A'; 2048];
    let common_suffix = vec![b'B'; 2048];
    let middle_part = vec![b'C'; 1024];

    let mut dest_content = Vec::new();
    dest_content.extend_from_slice(&common_prefix);
    dest_content.extend_from_slice(&common_suffix);
    fs::write(&destination, &dest_content).expect("write destination");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("set dest mtime");

    let mut source_content = Vec::new();
    source_content.extend_from_slice(&common_prefix);
    source_content.extend_from_slice(&middle_part);
    source_content.extend_from_slice(&common_suffix);
    let source_len = source_content.len() as u64;
    fs::write(&source, &source_content).expect("write source");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(false),
        )
        .expect("delta transfer succeeds");

    // Verify delta transfer was used
    assert_eq!(summary.files_copied(), 1);
    assert!(
        summary.matched_bytes() > 0,
        "delta mode should match blocks from basis file"
    );
    assert!(
        summary.bytes_copied() < source_len,
        "delta should copy less than full file size"
    );
    assert_eq!(
        summary.bytes_copied() + summary.matched_bytes(),
        source_len,
        "copied + matched should equal source size"
    );
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        source_content
    );
}

#[test]
fn execute_whole_file_default_behavior() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"default behavior test";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, b"old").expect("write destination");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("set dest mtime");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Default options should have whole_file enabled
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("default copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), content.len() as u64);
    assert_eq!(
        summary.matched_bytes(),
        0,
        "default should use whole file mode"
    );
}

#[test]
fn execute_whole_file_with_compression() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Highly compressible content
    let content = b"aaaaaaaaaa".repeat(100);
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .whole_file(true)
                .compress(true)
                .with_compression_algorithm(CompressionAlgorithm::Zlib)
                .with_compression_level(CompressionLevel::Default),
        )
        .expect("compressed whole file copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), content.len() as u64);
    assert_eq!(summary.matched_bytes(), 0);
    assert!(
        summary.compression_used(),
        "compression should be used"
    );
    assert_eq!(fs::read(&destination).expect("read destination"), content);
}

#[test]
fn execute_whole_file_preserves_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"timestamp test";
    fs::write(&source, content).expect("write source");

    let source_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, source_time).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .whole_file(true)
                .times(true),
        )
        .expect("copy with times succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.matched_bytes(), 0);

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_mtime, source_time, "mtime should be preserved");
}

#[test]
fn execute_whole_file_with_inplace() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let new_content = b"inplace whole file content";
    fs::write(&source, new_content).expect("write source");
    fs::write(&destination, b"existing data that will be overwritten").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .whole_file(true)
                .inplace(true),
        )
        .expect("inplace whole file copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), new_content.len() as u64);
    assert_eq!(summary.matched_bytes(), 0);
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        new_content
    );
}

#[test]
fn execute_whole_file_dry_run_reports_transfer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"dry run test content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, b"old").expect("write destination");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("set dest mtime");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().whole_file(true),
        )
        .expect("dry run succeeds");

    // Dry run should report what would be done
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), content.len() as u64);
    assert_eq!(summary.matched_bytes(), 0);

    // Destination should remain unchanged
    assert_eq!(fs::read(&destination).expect("read destination"), b"old");
}

#[test]
fn execute_whole_file_with_checksum_comparison() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"checksum comparison test";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write identical destination");

    // Set same mtime so time-based comparison would skip
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source mtime");
    set_file_mtime(&destination, timestamp).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .whole_file(true)
                .checksum(true)
                .times(true),
        )
        .expect("checksum copy succeeds");

    // File should be skipped because content is identical
    assert_eq!(
        summary.files_copied(),
        0,
        "identical file should be skipped with checksum comparison"
    );
}

#[test]
fn execute_whole_file_empty_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"existing content").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(true),
        )
        .expect("empty file copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 0);
    assert_eq!(summary.matched_bytes(), 0);
    assert_eq!(fs::read(&destination).expect("read destination"), b"");
}

#[test]
fn execute_whole_file_vs_delta_transfer_comparison() {
    let temp = tempdir().expect("tempdir");

    // Setup for whole file mode
    let source_whole = temp.path().join("source_whole.bin");
    let dest_whole = temp.path().join("dest_whole.bin");

    // Setup for delta mode
    let source_delta = temp.path().join("source_delta.bin");
    let dest_delta = temp.path().join("dest_delta.bin");

    // Create identical source and destination pairs with common blocks
    let common = vec![b'X'; 4096];
    let different = vec![b'Y'; 1024];

    let dest_content = common.clone();
    fs::write(&dest_whole, &dest_content).expect("write dest whole");
    fs::write(&dest_delta, &dest_content).expect("write dest delta");
    set_file_mtime(&dest_whole, FileTime::from_unix_time(1, 0)).expect("set dest whole mtime");
    set_file_mtime(&dest_delta, FileTime::from_unix_time(1, 0)).expect("set dest delta mtime");

    let mut source_content = common.clone();
    source_content.extend_from_slice(&different);
    let source_len = source_content.len() as u64;
    fs::write(&source_whole, &source_content).expect("write source whole");
    fs::write(&source_delta, &source_content).expect("write source delta");
    set_file_mtime(&source_whole, FileTime::from_unix_time(2, 0)).expect("set source whole mtime");
    set_file_mtime(&source_delta, FileTime::from_unix_time(2, 0)).expect("set source delta mtime");

    // Execute with whole file mode
    let operands_whole = vec![
        source_whole.into_os_string(),
        dest_whole.clone().into_os_string(),
    ];
    let plan_whole = LocalCopyPlan::from_operands(&operands_whole).expect("plan whole");
    let summary_whole = plan_whole
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(true),
        )
        .expect("whole file copy succeeds");

    // Execute with delta mode
    let operands_delta = vec![
        source_delta.into_os_string(),
        dest_delta.clone().into_os_string(),
    ];
    let plan_delta = LocalCopyPlan::from_operands(&operands_delta).expect("plan delta");
    let summary_delta = plan_delta
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(false),
        )
        .expect("delta copy succeeds");

    // Compare results
    assert_eq!(summary_whole.matched_bytes(), 0, "whole file should not match");
    assert_eq!(summary_whole.bytes_copied(), source_len, "whole file copies everything");

    assert!(summary_delta.matched_bytes() > 0, "delta should match common blocks");
    assert!(summary_delta.bytes_copied() < source_len, "delta copies less data");

    // Both should produce identical results
    assert_eq!(
        fs::read(&dest_whole).expect("read dest whole"),
        fs::read(&dest_delta).expect("read dest delta")
    );
}


#[test]
fn whole_file_auto_defaults_to_whole_for_local_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"auto detection defaults to whole file for local";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, b"old content").expect("write destination");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("set dest mtime");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Default options: whole_file is None (auto-detect)
    let options = LocalCopyOptions::default();
    assert!(options.whole_file_raw().is_none(), "default should be auto (None)");
    assert!(options.whole_file_enabled(), "auto should resolve to true for local copy");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("auto whole file copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), content.len() as u64);
    assert_eq!(summary.matched_bytes(), 0, "auto mode should use whole file for local");
}

#[test]
fn whole_file_option_none_preserves_auto_detection() {
    let options = LocalCopyOptions::default();
    assert!(options.whole_file_raw().is_none());

    // Setting to None explicitly should keep auto mode
    let options = options.whole_file_option(None);
    assert!(options.whole_file_raw().is_none());
    assert!(options.whole_file_enabled());
}

#[test]
fn whole_file_option_some_true_forces_whole_file() {
    let options = LocalCopyOptions::default().whole_file_option(Some(true));
    assert_eq!(options.whole_file_raw(), Some(true));
    assert!(options.whole_file_enabled());
}

#[test]
fn whole_file_option_some_false_forces_delta() {
    let options = LocalCopyOptions::default().whole_file_option(Some(false));
    assert_eq!(options.whole_file_raw(), Some(false));
    assert!(!options.whole_file_enabled());
}

#[test]
fn whole_file_auto_restores_none() {
    // Start with explicitly set whole_file
    let options = LocalCopyOptions::default().whole_file(true);
    assert_eq!(options.whole_file_raw(), Some(true));

    // Restore auto mode
    let options = options.whole_file_auto();
    assert!(options.whole_file_raw().is_none());
    assert!(options.whole_file_enabled());
}

#[test]
fn whole_file_setter_overrides_auto() {
    let options = LocalCopyOptions::default();
    assert!(options.whole_file_raw().is_none());

    // .whole_file(true) should set Some(true), overriding None
    let options = options.whole_file(true);
    assert_eq!(options.whole_file_raw(), Some(true));

    // .whole_file(false) should set Some(false)
    let options = options.whole_file(false);
    assert_eq!(options.whole_file_raw(), Some(false));
    assert!(!options.whole_file_enabled());
}

#[test]
fn execute_whole_file_auto_mode_copies_correctly() {
    // Verify that auto mode (None) produces correct file content
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src.bin");
    let destination = temp.path().join("dst.bin");

    let content: Vec<u8> = (0..=255).cycle().take(4096).collect();
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Use default (auto) mode
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("auto mode copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), content);
}

#[test]
fn whole_file_builder_sets_some_true() {
    let options = LocalCopyOptions::builder()
        .whole_file(true)
        .build()
        .expect("valid options");
    assert_eq!(options.whole_file_raw(), Some(true));
    assert!(options.whole_file_enabled());
}

#[test]
fn whole_file_builder_sets_some_false() {
    let options = LocalCopyOptions::builder()
        .whole_file(false)
        .build()
        .expect("valid options");
    assert_eq!(options.whole_file_raw(), Some(false));
    assert!(!options.whole_file_enabled());
}

#[test]
fn whole_file_builder_default_is_none() {
    let options = LocalCopyOptions::builder()
        .build()
        .expect("valid options");
    assert!(options.whole_file_raw().is_none());
    // Auto mode without batch writer defaults to whole-file
    assert!(options.whole_file_enabled());
}

#[test]
fn whole_file_builder_option_none_is_auto() {
    let options = LocalCopyOptions::builder()
        .whole_file_option(None)
        .build()
        .expect("valid options");
    assert!(options.whole_file_raw().is_none());
    assert!(options.whole_file_enabled());
}

#[test]
fn execute_no_whole_file_explicit_forces_delta_even_for_local() {
    // --no-whole-file should force delta transfer even for local copies
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    let common = vec![b'M'; 4096];
    let extra = vec![b'N'; 1024];

    fs::write(&destination, &common).expect("write dest");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("set dest mtime");

    let mut source_content = common.clone();
    source_content.extend_from_slice(&extra);
    let source_len = source_content.len() as u64;
    fs::write(&source, &source_content).expect("write source");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Use whole_file_option(Some(false)) to simulate --no-whole-file
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file_option(Some(false)),
        )
        .expect("forced delta succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(
        summary.matched_bytes() > 0,
        "--no-whole-file should force delta matching"
    );
    assert!(
        summary.bytes_copied() < source_len,
        "delta should transfer less than full size"
    );
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        source_content
    );
}
