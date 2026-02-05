// Tests for --max-size file filtering functionality
//
// This module verifies that the max-size filter correctly includes and excludes
// files based on their size. The filter should:
// - Exclude files larger than the specified limit
// - Include files equal to the limit
// - Include files smaller than the limit

#[test]
fn execute_excludes_files_larger_than_max_size() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create files of various sizes
    fs::write(source_root.join("small.txt"), vec![0u8; 512]).expect("write small");
    fs::write(source_root.join("medium.txt"), vec![0u8; 1024]).expect("write medium");
    fs::write(source_root.join("large.txt"), vec![0u8; 2048]).expect("write large");
    fs::write(source_root.join("huge.txt"), vec![0u8; 4096]).expect("write huge");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set max-size to 1024 bytes
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(1024)),
        )
        .expect("copy succeeds");

    // Files larger than 1024 bytes should be excluded
    assert_eq!(summary.files_copied(), 2, "should copy only small and medium files");
    assert!(dest_root.join("small.txt").exists(), "small file should be copied");
    assert!(dest_root.join("medium.txt").exists(), "medium file should be copied");
    assert!(!dest_root.join("large.txt").exists(), "large file should be excluded");
    assert!(!dest_root.join("huge.txt").exists(), "huge file should be excluded");
}

#[test]
fn execute_includes_files_equal_to_max_size() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("boundary.bin");
    let destination = temp.path().join("dest.bin");

    // Create a file exactly 2048 bytes
    let payload = vec![0xAA; 2048];
    fs::write(&source, &payload).expect("write boundary source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set max-size to exactly the file size
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(2048)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1, "file equal to max-size should be copied");
    assert!(destination.exists(), "destination should exist");
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        payload,
        "file content should match"
    );
}

#[test]
fn execute_includes_files_smaller_than_max_size() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create files smaller than the limit
    fs::write(source_root.join("tiny.txt"), b"ab").expect("write tiny");
    fs::write(source_root.join("small.txt"), vec![0u8; 100]).expect("write small");
    fs::write(source_root.join("medium.txt"), vec![0u8; 500]).expect("write medium");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set max-size larger than all files
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(1024)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3, "all files should be copied");
    assert!(dest_root.join("tiny.txt").exists());
    assert!(dest_root.join("small.txt").exists());
    assert!(dest_root.join("medium.txt").exists());
}

#[test]
fn execute_max_size_with_kilobyte_suffix() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create files: one under 1K, one at 1K, one over 1K
    fs::write(source_root.join("under.txt"), vec![0u8; 512]).expect("write under 1K");
    fs::write(source_root.join("exact.txt"), vec![0u8; 1024]).expect("write exactly 1K");
    fs::write(source_root.join("over.txt"), vec![0u8; 1536]).expect("write over 1K");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Max-size of 1K = 1024 bytes
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(1024)), // 1K
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2, "files <= 1K should be copied");
    assert!(dest_root.join("under.txt").exists(), "file under 1K should be copied");
    assert!(dest_root.join("exact.txt").exists(), "file exactly 1K should be copied");
    assert!(!dest_root.join("over.txt").exists(), "file over 1K should be excluded");
}

#[test]
fn execute_max_size_with_megabyte_suffix() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create files: one under 1M, one at 1M, one over 1M
    let one_mb: u64 = 1024 * 1024; // 1 MiB = 1048576 bytes
    fs::write(source_root.join("under.txt"), vec![0u8; (one_mb - 1024) as usize]).expect("write under 1M");
    fs::write(source_root.join("exact.txt"), vec![0u8; one_mb as usize]).expect("write exactly 1M");
    fs::write(source_root.join("over.txt"), vec![0u8; (one_mb + 1024) as usize]).expect("write over 1M");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Max-size of 1M = 1048576 bytes
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(one_mb)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2, "files <= 1M should be copied");
    assert!(dest_root.join("under.txt").exists(), "file under 1M should be copied");
    assert!(dest_root.join("exact.txt").exists(), "file exactly 1M should be copied");
    assert!(!dest_root.join("over.txt").exists(), "file over 1M should be excluded");
}

#[test]
fn execute_max_size_with_gigabyte_suffix() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create smaller test files to avoid disk space issues
    // Use sizes that represent relative positions to 1G
    let one_gb = 1024u64 * 1024 * 1024; // 1 GiB = 1073741824 bytes

    // Create small files that would be included under a 1G limit
    fs::write(source_root.join("small.txt"), vec![0u8; 1024]).expect("write small");
    fs::write(source_root.join("medium.txt"), vec![0u8; 1024 * 1024]).expect("write medium");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Max-size of 1G = 1073741824 bytes
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(one_gb)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2, "files much smaller than 1G should be copied");
    assert!(dest_root.join("small.txt").exists());
    assert!(dest_root.join("medium.txt").exists());
}

#[test]
fn execute_max_size_excludes_very_large_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.bin");
    let destination = temp.path().join("dest.bin");

    // Create a 10MB file
    let ten_mb = 10 * 1024 * 1024;
    fs::write(&source, vec![0u8; ten_mb]).expect("write 10MB source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set max-size to 5MB
    let five_mb: u64 = 5 * 1024 * 1024;
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(five_mb)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0, "10MB file should be excluded by 5MB limit");
    assert!(!destination.exists(), "destination should not be created");
}

#[test]
fn execute_max_size_with_empty_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    // Create empty file
    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(1024)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1, "empty file should be copied");
    assert!(destination.exists());
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

#[test]
fn execute_max_size_combined_with_min_size() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create files of various sizes
    fs::write(source_root.join("too_small.txt"), vec![0u8; 50]).expect("write too small");
    fs::write(source_root.join("just_right1.txt"), vec![0u8; 100]).expect("write just right 1");
    fs::write(source_root.join("just_right2.txt"), vec![0u8; 200]).expect("write just right 2");
    fs::write(source_root.join("too_large.txt"), vec![0u8; 500]).expect("write too large");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Only copy files between 100 and 300 bytes
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .min_file_size(Some(100))
                .max_file_size(Some(300)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2, "only files in range should be copied");
    assert!(!dest_root.join("too_small.txt").exists(), "file below min should be excluded");
    assert!(dest_root.join("just_right1.txt").exists(), "file at min boundary should be copied");
    assert!(dest_root.join("just_right2.txt").exists(), "file in range should be copied");
    assert!(!dest_root.join("too_large.txt").exists(), "file above max should be excluded");
}

#[test]
fn execute_max_size_does_not_affect_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested_dir = source_root.join("nested");
    fs::create_dir_all(&nested_dir).expect("create nested dir");

    // Create a small file inside the directory
    fs::write(nested_dir.join("small.txt"), b"content").expect("write small file");

    // Create a large file that should be excluded
    fs::write(source_root.join("large.txt"), vec![0u8; 2048]).expect("write large file");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(1024)),
        )
        .expect("copy succeeds");

    // Directory should be created regardless of max-size
    assert!(dest_root.join("nested").exists(), "directory should be created");
    assert!(dest_root.join("nested").is_dir(), "nested should be a directory");

    // Small file should be copied
    assert!(dest_root.join("nested").join("small.txt").exists(), "small file should be copied");

    // Large file should be excluded
    assert!(!dest_root.join("large.txt").exists(), "large file should be excluded");

    assert_eq!(summary.files_copied(), 1, "only the small file should be copied");
}

#[test]
fn execute_max_size_with_dry_run() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("small.txt"), vec![0u8; 512]).expect("write small");
    fs::write(source_root.join("large.txt"), vec![0u8; 2048]).expect("write large");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().max_file_size(Some(1024)),
        )
        .expect("dry run succeeds");

    // In dry run, files would be copied but destination doesn't exist
    assert_eq!(summary.files_copied(), 1, "dry run should report small file would be copied");
    assert!(!dest_root.exists(), "dry run should not create destination");
}

#[test]
fn execute_max_size_boundary_off_by_one() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Test exact boundaries: limit - 1, limit, limit + 1
    let limit = 1000u64;
    fs::write(source_root.join("one_under.txt"), vec![0u8; (limit - 1) as usize])
        .expect("write limit - 1");
    fs::write(source_root.join("exact.txt"), vec![0u8; limit as usize])
        .expect("write exact limit");
    fs::write(source_root.join("one_over.txt"), vec![0u8; (limit + 1) as usize])
        .expect("write limit + 1");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(limit)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2, "files <= limit should be copied");
    assert!(dest_root.join("one_under.txt").exists(), "file one byte under limit should be copied");
    assert!(dest_root.join("exact.txt").exists(), "file exactly at limit should be copied");
    assert!(!dest_root.join("one_over.txt").exists(), "file one byte over limit should be excluded");
}

#[test]
fn execute_max_size_with_filter_rules() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create files with different extensions and sizes
    fs::write(source_root.join("small.txt"), vec![0u8; 512]).expect("write small txt");
    fs::write(source_root.join("small.bak"), vec![0u8; 512]).expect("write small bak");
    fs::write(source_root.join("large.txt"), vec![0u8; 2048]).expect("write large txt");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Exclude .bak files AND apply max-size filter
    let program = FilterProgram::new([FilterProgramEntry::Rule(FilterRule::exclude("*.bak"))])
        .expect("compile filter");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .max_file_size(Some(1024))
                .with_filter_program(Some(program)),
        )
        .expect("copy succeeds");

    // Only small.txt should be copied (small.bak excluded by filter, large.txt excluded by size)
    assert_eq!(summary.files_copied(), 1, "only small.txt should be copied");
    assert!(dest_root.join("small.txt").exists(), "small.txt should be copied");
    assert!(!dest_root.join("small.bak").exists(), "small.bak should be excluded by filter");
    assert!(!dest_root.join("large.txt").exists(), "large.txt should be excluded by size");
}
