// Comprehensive tests for skip logic during file transfers.
//
// This module tests all the code paths that determine whether a file transfer
// should be skipped. The skip decision depends on several factors:
//
// 1. Size-only (--size-only): Skip files when sizes match, ignoring timestamps/content.
// 2. Checksum (--checksum / -c): Skip files when checksums match, ignoring timestamps.
// 3. Timestamp (default): Skip files when mtime + size both match.
// 4. Existing-only (--existing): Skip files/directories not present at destination.
// 5. Ignore-existing (--ignore-existing): Skip files that already exist at destination.
// 6. Update (--update / -u): Skip files where destination is newer.
// 7. Ignore-times (--ignore-times / -I): Force transfer regardless of matching metadata.
// 8. Modify-window: Timestamp comparison tolerance.
// 9. Min/max size filters: Skip files outside the allowed size range.
// 10. Combined flag interactions and precedence.

// ============================================================================
// Timestamp Skip Tests (default mtime+size comparison)
// ============================================================================

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
        source.into_os_string(),
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
        source.into_os_string(),
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

/// When source and destination have matching size and mtime, the file is skipped
/// even if the actual content differs (timestamp-based comparison is the default).
#[test]
fn execute_skip_timestamp_same_mtime_same_size_different_content() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size (7 bytes), different content
    fs::write(&source, b"aaaaaaa").expect("write source");
    fs::write(&destination, b"bbbbbbb").expect("write dest");

    // Align timestamps
    let shared_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, shared_mtime).expect("set source mtime");
    set_file_mtime(&destination, shared_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    // Skipped because size+mtime match (content is NOT compared by default)
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
    // Destination content is preserved (not overwritten)
    assert_eq!(fs::read(&destination).expect("read"), b"bbbbbbb");
}

/// Files with matching size but different mtimes are transferred (not skipped).
#[test]
fn execute_skip_timestamp_same_size_different_mtime_transfers() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write dest");

    let source_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    let dest_mtime = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_mtime(&source, source_mtime).expect("set source mtime");
    set_file_mtime(&destination, dest_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    // Despite identical content, different mtime triggers transfer
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

/// Files with different sizes are always transferred regardless of mtime.
#[test]
fn execute_skip_timestamp_different_size_same_mtime_transfers() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"longer content here").expect("write source");
    fs::write(&destination, b"short").expect("write dest");

    let shared_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, shared_mtime).expect("set source mtime");
    set_file_mtime(&destination, shared_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read"),
        b"longer content here"
    );
}

/// Mixed directory with some files matching and some needing transfer.
#[test]
fn execute_skip_timestamp_directory_mixed_match_and_transfer() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    let shared_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    let different_mtime = FileTime::from_unix_time(1_700_000_100, 0);

    // File 1: same size + same mtime -> skip
    fs::write(source_root.join("skip_me.txt"), b"aaaa").expect("write skip source");
    fs::write(dest_root.join("skip_me.txt"), b"bbbb").expect("write skip dest");
    set_file_mtime(source_root.join("skip_me.txt"), shared_mtime).expect("set mtime");
    set_file_mtime(dest_root.join("skip_me.txt"), shared_mtime).expect("set mtime");

    // File 2: same size + different mtime -> transfer
    fs::write(source_root.join("copy_me.txt"), b"cccc").expect("write copy source");
    fs::write(dest_root.join("copy_me.txt"), b"dddd").expect("write copy dest");
    set_file_mtime(source_root.join("copy_me.txt"), different_mtime).expect("set mtime");
    set_file_mtime(dest_root.join("copy_me.txt"), shared_mtime).expect("set mtime");

    // File 3: new file -> transfer
    fs::write(source_root.join("new.txt"), b"new file").expect("write new source");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.regular_files_matched(), 1);
    assert_eq!(summary.regular_files_total(), 3);
    // skip_me was not overwritten
    assert_eq!(
        fs::read(dest_root.join("skip_me.txt")).expect("read"),
        b"bbbb"
    );
    // copy_me was overwritten
    assert_eq!(
        fs::read(dest_root.join("copy_me.txt")).expect("read"),
        b"cccc"
    );
    assert_eq!(
        fs::read(dest_root.join("new.txt")).expect("read"),
        b"new file"
    );
}

// ============================================================================
// Checksum Skip Tests (--checksum / -c)
// ============================================================================

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
        source.into_os_string(),
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

/// Tests that checksum mode correctly identifies identical files and skips copying.
///
/// This test exercises the checksum comparison path, which is parallelized when
/// the `parallel` feature is enabled. The test creates multiple files with
/// identical content at source and destination to verify:
/// 1. Files with matching checksums are skipped
/// 2. Files with different checksums are copied
/// 3. Summary statistics accurately reflect the operations
#[test]
fn execute_with_checksum_skips_matching_directory_contents() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    // Create multiple files - some identical, some different
    let identical_content = b"identical content here";
    let different_source = b"different source data!";
    let different_dest = b"different dest content";

    // Identical files (should be skipped)
    fs::write(source_root.join("same1.txt"), identical_content).expect("write same1 source");
    fs::write(target_root.join("same1.txt"), identical_content).expect("write same1 dest");

    fs::write(source_root.join("same2.txt"), identical_content).expect("write same2 source");
    fs::write(target_root.join("same2.txt"), identical_content).expect("write same2 dest");

    // Different content file (same size, should be copied)
    fs::write(source_root.join("diff.txt"), different_source).expect("write diff source");
    fs::write(target_root.join("diff.txt"), different_dest).expect("write diff dest");

    // New file (no destination, should be copied)
    fs::write(source_root.join("new.txt"), b"brand new file").expect("write new source");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, target_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true).recursive(true),
        )
        .expect("copy succeeds");

    // Verify results
    assert_eq!(summary.regular_files_total(), 4, "should process 4 files");
    assert_eq!(
        summary.regular_files_matched(),
        2,
        "2 identical files should match"
    );
    assert_eq!(summary.files_copied(), 2, "2 different/new files should copy");

    // Verify file contents
    assert_eq!(
        fs::read(target_root.join("same1.txt")).expect("read same1"),
        identical_content
    );
    assert_eq!(
        fs::read(target_root.join("same2.txt")).expect("read same2"),
        identical_content
    );
    assert_eq!(
        fs::read(target_root.join("diff.txt")).expect("read diff"),
        different_source // source content should overwrite destination
    );
    assert_eq!(
        fs::read(target_root.join("new.txt")).expect("read new"),
        b"brand new file"
    );
}

#[test]
fn execute_with_checksum_handles_empty_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"").expect("write empty dest");

    let operands = vec![
        source.into_os_string(),
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
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn execute_with_checksum_copies_different_empty_and_nonempty() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"not empty").expect("write source");
    fs::write(&destination, b"").expect("write empty dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"not empty");
}

/// Checksum mode detects content differences even when timestamps match.
#[test]
fn execute_skip_checksum_detects_mismatch_despite_matching_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, different content
    fs::write(&source, b"AAAAA").expect("write source");
    fs::write(&destination, b"BBBBB").expect("write dest");

    // Align timestamps - without checksum this would be skipped
    let shared_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, shared_mtime).expect("set source mtime");
    set_file_mtime(&destination, shared_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    // Checksum detects content difference -> file is transferred
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    assert_eq!(fs::read(&destination).expect("read"), b"AAAAA");
}

/// Checksum mode skips files with matching content regardless of differing timestamps.
#[test]
fn execute_skip_checksum_skips_matching_content_despite_different_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"same content here").expect("write source");
    fs::write(&destination, b"same content here").expect("write dest");

    // Set very different mtimes
    let old_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    let new_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, old_mtime).expect("set source mtime");
    set_file_mtime(&destination, new_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    // Despite different timestamps, checksum matches -> skip
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Checksum with a large file that spans multiple read buffers.
#[test]
fn execute_skip_checksum_large_identical_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    let large_data = vec![0xCDu8; 256 * 1024]; // 256 KB
    fs::write(&source, &large_data).expect("write source");
    fs::write(&destination, &large_data).expect("write dest");

    let operands = vec![
        source.into_os_string(),
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
    assert_eq!(summary.regular_files_matched(), 1);
    assert_eq!(summary.bytes_copied(), 0);
}

/// Checksum detects single byte difference in a large file.
#[test]
fn execute_skip_checksum_large_files_single_byte_difference() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    let source_data = vec![0xABu8; 128 * 1024]; // 128 KB
    let mut dest_data = source_data.clone();
    dest_data[65536] = 0xCD; // Differ at one byte in the middle

    fs::write(&source, &source_data).expect("write source");
    fs::write(&destination, &dest_data).expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read"),
        source_data
    );
}

/// Dry run with checksum reports all files as would-copy without modifying files.
/// Note: dry run does not evaluate should_skip_copy (checksum/size_only/mtime),
/// so all existing-dest files are reported as DataCopied rather than matched.
#[test]
fn execute_skip_checksum_dry_run_reports_correctly() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Matching file
    fs::write(source_root.join("same.txt"), b"identical").expect("write");
    fs::write(dest_root.join("same.txt"), b"identical").expect("write");

    // Different file
    fs::write(source_root.join("diff.txt"), b"source_v").expect("write");
    fs::write(dest_root.join("diff.txt"), b"dest_vvv").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("dry run succeeds");

    // Dry run does not perform should_skip_copy analysis; all files are
    // reported as would-be-copied (DataCopied).
    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.regular_files_matched(), 0);
    // Files unchanged in dry run
    assert_eq!(fs::read(dest_root.join("same.txt")).expect("read"), b"identical");
    assert_eq!(fs::read(dest_root.join("diff.txt")).expect("read"), b"dest_vvv");
}

// ============================================================================
// Size-Only Skip Tests (--size-only)
// ============================================================================

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
        source_path.into_os_string(),
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
fn execute_with_size_only_copies_different_size_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"longer content").expect("write source");
    fs::write(&destination, b"short").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"longer content");
}

#[test]
fn execute_with_size_only_handles_empty_vs_nonempty() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"non-empty").expect("write source");
    fs::write(&destination, b"").expect("write empty dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"non-empty");
}

#[test]
fn execute_with_size_only_skips_same_size_different_mtime() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, different content, different mtime
    fs::write(&source, b"abc").expect("write source");
    fs::write(&destination, b"xyz").expect("write dest");

    // Set significantly different mtimes
    let older = FileTime::from_unix_time(1_600_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, older).expect("set source mtime");
    set_file_mtime(&destination, newer).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    // Should skip because sizes match, regardless of mtime difference
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"xyz");
}

#[test]
fn execute_with_size_only_handles_both_empty() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Both files empty (size = 0)
    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"").expect("write empty dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn execute_with_size_only_and_update_skips_same_size() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"abc").expect("write source");
    fs::write(&destination, b"xyz").expect("write dest");

    // Destination is newer, but size-only should skip anyway
    let older = FileTime::from_unix_time(1_600_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, older).expect("set source mtime");
    set_file_mtime(&destination, newer).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true).update(true),
        )
        .expect("copy succeeds");

    // size_only skips same-size files
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"xyz");
}

#[test]
fn execute_with_size_only_and_checksum_skips_same_size() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, different content - checksum overrides size_only
    fs::write(&source, b"aaa").expect("write source");
    fs::write(&destination, b"bbb").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true).checksum(true),
        )
        .expect("copy succeeds");

    // checksum overrides size_only - file is copied due to checksum mismatch
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    assert_eq!(fs::read(&destination).expect("read"), b"aaa");
}

#[test]
fn execute_with_size_only_and_ignore_times_skips_same_size() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, different content
    // ignore_times overrides size_only, forcing transfer
    fs::write(&source, b"abc").expect("write source");
    fs::write(&destination, b"xyz").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .size_only(true)
                .ignore_times(true),
        )
        .expect("copy succeeds");

    // ignore_times overrides size_only - file is transferred
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    assert_eq!(fs::read(&destination).expect("read"), b"abc");
}

#[test]
fn execute_with_size_only_and_times_preserves_metadata() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, different content
    fs::write(&source, b"abc").expect("write source");
    fs::write(&destination, b"xyz").expect("write dest");

    // Set different mtime on source
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
            LocalCopyOptions::default().size_only(true).times(true),
        )
        .expect("copy succeeds");

    // File should be skipped but metadata should still be updated
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"xyz");

    // Times should be updated even though content wasn't copied
    let dest_meta = fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_mtime, source_time);
}

#[test]
fn execute_with_size_only_directory_tree() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Create files with various size relationships
    // Same size, should skip
    fs::write(source_root.join("same_size1.txt"), b"aaa").expect("write same_size1 source");
    fs::write(dest_root.join("same_size1.txt"), b"zzz").expect("write same_size1 dest");

    fs::write(source_root.join("same_size2.txt"), b"12345").expect("write same_size2 source");
    fs::write(dest_root.join("same_size2.txt"), b"67890").expect("write same_size2 dest");

    // Different size, should copy
    fs::write(source_root.join("diff_size1.txt"), b"short").expect("write diff_size1 source");
    fs::write(dest_root.join("diff_size1.txt"), b"much longer content")
        .expect("write diff_size1 dest");

    fs::write(source_root.join("diff_size2.txt"), b"longer content here")
        .expect("write diff_size2 source");
    fs::write(dest_root.join("diff_size2.txt"), b"tiny").expect("write diff_size2 dest");

    // New file, should copy
    fs::write(source_root.join("new.txt"), b"new file").expect("write new source");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    // 2 same size should be matched, 3 should be copied (2 diff size + 1 new)
    assert_eq!(summary.regular_files_matched(), 2);
    assert_eq!(summary.files_copied(), 3);

    // Verify same-size files weren't changed
    assert_eq!(
        fs::read(dest_root.join("same_size1.txt")).expect("read"),
        b"zzz"
    );
    assert_eq!(
        fs::read(dest_root.join("same_size2.txt")).expect("read"),
        b"67890"
    );

    // Verify different-size files were updated
    assert_eq!(
        fs::read(dest_root.join("diff_size1.txt")).expect("read"),
        b"short"
    );
    assert_eq!(
        fs::read(dest_root.join("diff_size2.txt")).expect("read"),
        b"longer content here"
    );
    assert_eq!(
        fs::read(dest_root.join("new.txt")).expect("read"),
        b"new file"
    );
}

#[test]
fn execute_with_size_only_copies_larger_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"source is larger").expect("write source");
    fs::write(&destination, b"small").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read"),
        b"source is larger"
    );
}

#[test]
fn execute_with_size_only_copies_smaller_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"tiny").expect("write source");
    fs::write(&destination, b"destination is much larger")
        .expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"tiny");
}

/// Size-only in dry run mode reports would-copy without modifying files.
/// Note: dry run does not evaluate should_skip_copy (checksum/size_only/mtime),
/// so same-size files are still reported as DataCopied rather than matched.
#[test]
fn execute_skip_size_only_dry_run() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"abc").expect("write source");
    fs::write(&destination, b"xyz").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("dry run succeeds");

    // Dry run does not perform should_skip_copy analysis; file is
    // reported as would-be-copied (DataCopied).
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    // Content unchanged in dry run
    assert_eq!(fs::read(&destination).expect("read"), b"xyz");
}

/// Size-only skips when destination is missing (new file is always transferred).
#[test]
fn execute_skip_size_only_new_file_transferred() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content").expect("write source");
    // No destination file

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"new content");
}

// ============================================================================
// Ignore Times Skip Tests (--ignore-times / -I)
// ============================================================================

#[test]
fn execute_with_ignore_times_rewrites_matching_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("destination.txt");

    fs::write(&source, b"newdata").expect("write source");
    fs::write(&destination, b"olddata").expect("write destination");

    let timestamp = FileTime::from_unix_time(1_700_200_000, 0);
    set_file_times(&source, timestamp, timestamp).expect("set source times");
    set_file_times(&destination, timestamp, timestamp).expect("set destination times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"newdata"
    );
}

/// Ignore-times forces transfer even when content, size, and mtime all match.
#[test]
fn execute_skip_ignore_times_forces_rewrite_of_identical_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"identical").expect("write source");
    fs::write(&destination, b"identical").expect("write dest");

    let shared_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, shared_mtime).expect("set source mtime");
    set_file_mtime(&destination, shared_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_times(true),
        )
        .expect("copy succeeds");

    // Force transfer regardless of matching metadata
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

// ============================================================================
// Update Skip Tests (--update / -u)
// ============================================================================

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
        source_path.into_os_string(),
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

#[test]
fn execute_with_update_copies_when_destination_older() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"updated").expect("write source");
    fs::write(&destination, b"stale").expect("write dest");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, newer, newer).expect("set source times");
    set_file_times(&destination, older, older).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"updated");
}

#[test]
fn execute_with_update_copies_when_destination_missing() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new file").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"new file");
}

/// Update combined with checksum still respects the newer-destination check.
#[test]
fn execute_skip_update_with_checksum_still_skips_newer_dest() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Different content, but destination is newer
    fs::write(&source, b"source data").expect("write source");
    fs::write(&destination, b"dest data!!").expect("write dest");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, older, older).expect("set source times");
    set_file_times(&destination, newer, newer).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true).checksum(true),
        )
        .expect("copy succeeds");

    // Update takes precedence - destination is newer, skip
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"dest data!!");
}

/// Update in a recursive directory tree with mixed timestamp scenarios.
#[test]
fn execute_skip_update_directory_mixed_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);

    // File 1: source newer -> copy
    fs::write(source_root.join("newer_src.txt"), b"new data").expect("write");
    fs::write(dest_root.join("newer_src.txt"), b"old data").expect("write");
    set_file_mtime(source_root.join("newer_src.txt"), newer).expect("set mtime");
    set_file_mtime(dest_root.join("newer_src.txt"), older).expect("set mtime");

    // File 2: dest newer -> skip
    fs::write(source_root.join("newer_dst.txt"), b"old data").expect("write");
    fs::write(dest_root.join("newer_dst.txt"), b"new data").expect("write");
    set_file_mtime(source_root.join("newer_dst.txt"), older).expect("set mtime");
    set_file_mtime(dest_root.join("newer_dst.txt"), newer).expect("set mtime");

    // File 3: no dest -> copy
    fs::write(source_root.join("brand_new.txt"), b"fresh").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(
        fs::read(dest_root.join("newer_src.txt")).expect("read"),
        b"new data"
    );
    assert_eq!(
        fs::read(dest_root.join("newer_dst.txt")).expect("read"),
        b"new data" // preserved
    );
    assert_eq!(
        fs::read(dest_root.join("brand_new.txt")).expect("read"),
        b"fresh"
    );
}

// ============================================================================
// Existing-Only Skip Tests (--existing)
// ============================================================================

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

    let mut source_operand = source_root.into_os_string();
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

/// Existing-only updates files that already exist at the destination.
#[test]
fn execute_skip_existing_only_updates_present_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // File present at dest -> should be updated
    fs::write(source_root.join("present.txt"), b"updated content").expect("write");
    fs::write(dest_root.join("present.txt"), b"old content").expect("write");

    // File absent at dest -> should be skipped
    fs::write(source_root.join("absent.txt"), b"new file").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().existing_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_skipped_missing(), 1);
    assert_eq!(
        fs::read(dest_root.join("present.txt")).expect("read"),
        b"updated content"
    );
    assert!(!dest_root.join("absent.txt").exists());
}

/// Existing-only combined with update: both filters apply.
#[test]
fn execute_skip_existing_only_with_update() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);

    // File 1: exists, source newer -> copy
    fs::write(source_root.join("update_me.txt"), b"updated").expect("write");
    fs::write(dest_root.join("update_me.txt"), b"old____").expect("write");
    set_file_mtime(source_root.join("update_me.txt"), newer).expect("set mtime");
    set_file_mtime(dest_root.join("update_me.txt"), older).expect("set mtime");

    // File 2: exists, dest newer -> skip (update)
    fs::write(source_root.join("skip_newer.txt"), b"older__").expect("write");
    fs::write(dest_root.join("skip_newer.txt"), b"newer__").expect("write");
    set_file_mtime(source_root.join("skip_newer.txt"), older).expect("set mtime");
    set_file_mtime(dest_root.join("skip_newer.txt"), newer).expect("set mtime");

    // File 3: absent at dest -> skip (existing_only)
    fs::write(source_root.join("absent.txt"), b"new").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .existing_only(true)
                .update(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(summary.regular_files_skipped_missing(), 1);
    assert_eq!(
        fs::read(dest_root.join("update_me.txt")).expect("read"),
        b"updated"
    );
    assert_eq!(
        fs::read(dest_root.join("skip_newer.txt")).expect("read"),
        b"newer__"
    );
    assert!(!dest_root.join("absent.txt").exists());
}

// ============================================================================
// Ignore Existing Skip Tests (--ignore-existing)
// ============================================================================

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
        source_path.into_os_string(),
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
fn execute_with_ignore_existing_creates_new_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_existing(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"new content");
}

#[test]
fn execute_with_multiple_sources_and_ignore_existing() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("new.txt"), b"new").expect("write new");
    fs::write(source_root.join("exists.txt"), b"updated").expect("write exists");
    fs::write(source_root.join("another_new.txt"), b"also new").expect("write another");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::write(dest_root.join("exists.txt"), b"original").expect("write existing");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_existing(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.regular_files_ignored_existing(), 1);
    assert_eq!(fs::read(dest_root.join("new.txt")).expect("read"), b"new");
    assert_eq!(fs::read(dest_root.join("another_new.txt")).expect("read"), b"also new");
    assert_eq!(fs::read(dest_root.join("exists.txt")).expect("read"), b"original");
}

/// Ignore-existing takes precedence over update (source newer, but dest exists -> skip).
#[test]
fn execute_skip_ignore_existing_takes_precedence_over_update() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"newer source").expect("write source");
    fs::write(&destination, b"older dest").expect("write dest");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, newer, newer).expect("set source times");
    set_file_times(&destination, older, older).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .ignore_existing(true)
                .update(true),
        )
        .expect("copy succeeds");

    // ignore_existing wins: file exists, skip
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_ignored_existing(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 0);
    assert_eq!(fs::read(&destination).expect("read"), b"older dest");
}

/// Ignore-existing with report/records generates the correct action.
#[test]
fn execute_skip_ignore_existing_with_records() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    fs::write(source_root.join("existing.txt"), b"updated").expect("write");
    fs::write(dest_root.join("existing.txt"), b"original").expect("write");
    fs::write(source_root.join("new.txt"), b"brand new").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .ignore_existing(true)
                .collect_events(true),
        )
        .expect("copy succeeds");

    let summary = report.summary();
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_ignored_existing(), 1);

    let records = report.records();
    assert!(records.iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedExisting
            && record.relative_path() == std::path::Path::new("existing.txt")
    }));

    // New file was copied
    assert_eq!(
        fs::read(dest_root.join("new.txt")).expect("read"),
        b"brand new"
    );
    // Existing file was preserved
    assert_eq!(
        fs::read(dest_root.join("existing.txt")).expect("read"),
        b"original"
    );
}

// ============================================================================
// Combined Existing-Only + Ignore-Existing Tests
// ============================================================================

/// When both --existing and --ignore-existing are set, no files are transferred.
#[test]
fn execute_skip_existing_and_ignore_existing_skips_everything() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Existing file at dest
    fs::write(source_root.join("present.txt"), b"updated").expect("write");
    fs::write(dest_root.join("present.txt"), b"original").expect("write");

    // New file (not at dest)
    fs::write(source_root.join("absent.txt"), b"new file").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .existing_only(true)
                .ignore_existing(true),
        )
        .expect("copy succeeds");

    // present.txt: exists -> skipped by ignore_existing
    // absent.txt: doesn't exist -> skipped by existing_only
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(
        fs::read(dest_root.join("present.txt")).expect("read"),
        b"original"
    );
    assert!(!dest_root.join("absent.txt").exists());
}

// ============================================================================
// Min/Max Size Skip Tests
// ============================================================================

#[test]
fn execute_skips_files_smaller_than_min_size_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("tiny.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"abc").expect("write source");

    let operands = vec![
        source.into_os_string(),
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
        source.into_os_string(),
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
        source.into_os_string(),
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
fn execute_with_min_max_size_filters_correctly() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("tiny.txt"), b"ab").expect("write tiny");
    fs::write(source_root.join("small.txt"), b"1234567890").expect("write small");
    fs::write(source_root.join("medium.txt"), vec![0u8; 100]).expect("write medium");
    fs::write(source_root.join("large.txt"), vec![0u8; 500]).expect("write large");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .min_file_size(Some(5))
                .max_file_size(Some(200)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert!(!dest_root.join("tiny.txt").exists());
    assert!(dest_root.join("small.txt").exists());
    assert!(dest_root.join("medium.txt").exists());
    assert!(!dest_root.join("large.txt").exists());
}

// ============================================================================
// Modify Window Skip Tests
// ============================================================================

#[test]
fn execute_skips_within_modify_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"modify window test";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let slightly_different = FileTime::from_unix_time(1_700_000_001, 0);
    set_file_mtime(&source, base_time).expect("set source mtime");
    set_file_mtime(&destination, slightly_different).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .times(true)
                .with_modify_window(Duration::from_secs(2)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Timestamps outside the modify window cause a transfer.
#[test]
fn execute_skip_transfers_outside_modify_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"modify window test";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let far_different = FileTime::from_unix_time(1_700_000_010, 0);
    set_file_mtime(&source, base_time).expect("set source mtime");
    set_file_mtime(&destination, far_different).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .times(true)
                .with_modify_window(Duration::from_secs(2)),
        )
        .expect("copy succeeds");

    // 10 seconds difference exceeds 2 second window -> transferred
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

// ============================================================================
// Filter/Exclude Skip Tests
// ============================================================================

#[test]
fn execute_with_filter_excludes_matching_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source_root.join("skip.bak"), b"skip").expect("write skip");
    fs::write(source_root.join("also_keep.txt"), b"also").expect("write also_keep");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let program = FilterProgram::new([FilterProgramEntry::Rule(FilterRule::exclude("*.bak"))])
        .expect("compile filter");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_filter_program(Some(program)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert!(dest_root.join("keep.txt").exists());
    assert!(dest_root.join("also_keep.txt").exists());
    assert!(!dest_root.join("skip.bak").exists());
}

// ============================================================================
// Missing Args Skip Tests
// ============================================================================

#[test]
fn execute_with_ignore_missing_args_skips_absent_sources() {
    let temp = tempdir().expect("tempdir");
    let missing = temp.path().join("missing.txt");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&destination_root).expect("create destination root");
    let destination = destination_root.join("output.txt");
    fs::write(&destination, b"existing").expect("write destination");

    let operands = vec![
        missing.into_os_string(),
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
fn execute_with_delete_missing_args_removes_destination_entries() {
    let temp = tempdir().expect("tempdir");
    let missing = temp.path().join("absent.txt");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&destination_root).expect("create destination root");
    let destination = destination_root.join("absent.txt");
    fs::write(&destination, b"stale").expect("write destination");

    let operands = vec![
        missing.into_os_string(),
        destination_root.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .ignore_missing_args(true)
                .delete_missing_args(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.items_deleted(), 1);
    assert!(!destination.exists());
}

// ============================================================================
// Dry Run Skip Tests
// ============================================================================

#[test]
fn execute_dry_run_reports_skipped_files_as_matched() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"identical";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let mtime = FileTime::from_last_modification_time(&fs::metadata(&source).expect("meta"));
    set_file_mtime(&destination, mtime).expect("align times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().times(true),
        )
        .expect("dry run succeeds");

    // In dry run mode, identical files are counted and matched
    assert_eq!(summary.regular_files_total(), 1);
    // File content remains unchanged since it's dry run
    assert_eq!(fs::read(&destination).expect("read"), content);
}

/// Dry run with update correctly counts skipped-newer without modifying files.
#[test]
fn execute_skip_dry_run_update_reports_skipped_newer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"older source").expect("write source");
    fs::write(&destination, b"newer dest").expect("write dest");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, older, older).expect("set source times");
    set_file_times(&destination, newer, newer).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().update(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    // File unchanged
    assert_eq!(fs::read(&destination).expect("read"), b"newer dest");
}

/// Dry run with existing_only correctly counts missing-destination skips.
#[test]
fn execute_skip_dry_run_existing_only_reports_skipped_missing() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    // No destination

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().existing_only(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_missing(), 1);
    assert!(!destination.exists());
}

/// Dry run with ignore_existing correctly counts ignored-existing skips.
#[test]
fn execute_skip_dry_run_ignore_existing_reports_skipped() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"updated").expect("write source");
    fs::write(&destination, b"original").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().ignore_existing(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_ignored_existing(), 1);
    // File unchanged
    assert_eq!(fs::read(&destination).expect("read"), b"original");
}

// ============================================================================
// Complex Combined Flag Interaction Tests
// ============================================================================

/// Size-only + checksum: checksum takes priority and detects content difference.
#[test]
fn execute_skip_checksum_overrides_size_only_for_matching_sizes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, identical content
    fs::write(&source, b"same!!!").expect("write source");
    fs::write(&destination, b"same!!!").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true).checksum(true),
        )
        .expect("copy succeeds");

    // Checksum match -> skipped
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// All skip options combined: existing_only + ignore_existing + update.
#[test]
fn execute_skip_triple_combined_all_files_skipped() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    let older = FileTime::from_unix_time(1_700_000_000, 0);

    // File exists, source newer -> ignore_existing skips
    fs::write(source_root.join("exists.txt"), b"updated").expect("write");
    fs::write(dest_root.join("exists.txt"), b"original").expect("write");
    set_file_mtime(source_root.join("exists.txt"), newer).expect("set");
    set_file_mtime(dest_root.join("exists.txt"), older).expect("set");

    // File missing at dest -> existing_only skips
    fs::write(source_root.join("missing.txt"), b"new").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .existing_only(true)
                .ignore_existing(true)
                .update(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(
        fs::read(dest_root.join("exists.txt")).expect("read"),
        b"original"
    );
    assert!(!dest_root.join("missing.txt").exists());
}

/// Ignore-times overrides timestamp-based skip but respects ignore_existing.
#[test]
fn execute_skip_ignore_times_respects_ignore_existing() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"old content").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .ignore_times(true)
                .ignore_existing(true),
        )
        .expect("copy succeeds");

    // ignore_existing takes precedence - file exists, skip
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_ignored_existing(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"old content");
}

/// Checksum mode combined with times preservation: skip when checksums match
/// but still sync timestamps.
#[test]
fn execute_skip_checksum_with_times_syncs_mtime_on_skip() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"matching").expect("write source");
    fs::write(&destination, b"matching").expect("write dest");

    let source_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    let dest_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source, source_mtime).expect("set source mtime");
    set_file_mtime(&destination, dest_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true).times(true),
        )
        .expect("copy succeeds");

    // Content matches by checksum -> skip copy
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
    // But mtime should be synced from source
    let final_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );
    assert_eq!(final_mtime, source_mtime);
}

/// Update with modify-window: destination within the tolerance is not considered newer.
#[test]
fn execute_skip_update_respects_modify_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"source data").expect("write source");
    fs::write(&destination, b"dest data!!").expect("write dest");

    // Dest is 1 second newer, but with 2-second window it's "equal"
    let source_time = FileTime::from_unix_time(1_700_000_000, 0);
    let dest_time = FileTime::from_unix_time(1_700_000_001, 0);
    set_file_times(&source, source_time, source_time).expect("set source times");
    set_file_times(&destination, dest_time, dest_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .update(true)
                .with_modify_window(Duration::from_secs(2)),
        )
        .expect("copy succeeds");

    // Within the modify window, destination is NOT considered newer
    // so --update does NOT skip the file (it falls through to normal comparison)
    assert_eq!(summary.regular_files_skipped_newer(), 0);
}

// ============================================================================
// Bytes Copied Tracking in Skip Scenarios
// ============================================================================

/// When all files are skipped, bytes_copied should be zero.
#[test]
fn execute_skip_all_files_zero_bytes_copied() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Create several files with matching sizes (size-only will skip them all)
    fs::write(source_root.join("a.txt"), b"aaa").expect("write");
    fs::write(dest_root.join("a.txt"), b"zzz").expect("write");
    fs::write(source_root.join("b.txt"), b"bbbbb").expect("write");
    fs::write(dest_root.join("b.txt"), b"yyyyy").expect("write");
    fs::write(source_root.join("c.txt"), b"ccccccc").expect("write");
    fs::write(dest_root.join("c.txt"), b"xxxxxxx").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.bytes_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 3);
}

/// When some files are skipped and some are copied, bytes_copied reflects only copied files.
#[test]
fn execute_skip_partial_skip_correct_bytes_copied() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Same size -> skip (size_only)
    fs::write(source_root.join("skip.txt"), b"aaa").expect("write");
    fs::write(dest_root.join("skip.txt"), b"zzz").expect("write");

    // Different size -> copy (10 bytes)
    fs::write(source_root.join("copy.txt"), b"1234567890").expect("write");
    fs::write(dest_root.join("copy.txt"), b"short").expect("write");

    // New file -> copy (5 bytes)
    fs::write(source_root.join("new.txt"), b"brand").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.regular_files_matched(), 1);
    assert_eq!(summary.bytes_copied(), 15); // 10 + 5
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Skip logic with a single byte file.
#[test]
fn execute_skip_single_byte_files_size_only() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"A").expect("write source");
    fs::write(&destination, b"B").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    // Same size (1 byte) -> skip
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"B");
}

/// Skip logic with a single byte file using checksum.
#[test]
fn execute_skip_single_byte_files_checksum() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"A").expect("write source");
    fs::write(&destination, b"B").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    // Same size but different content -> checksum detects mismatch -> copy
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"A");
}

/// Checksum matching with binary data containing null bytes.
#[test]
fn execute_skip_checksum_binary_with_nulls() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    let content = vec![0x00, 0xFF, 0x00, 0xAB, 0x00, 0xCD, 0x00];
    fs::write(&source, &content).expect("write source");
    fs::write(&destination, &content).expect("write dest");

    let operands = vec![
        source.into_os_string(),
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
    assert_eq!(summary.regular_files_matched(), 1);
}

/// When source has no matching destination, skip options like size_only and checksum
/// do not prevent the transfer.
#[test]
fn execute_skip_new_file_always_transferred_regardless_of_skip_mode() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    // No destination

    for (label, options) in [
        ("size_only", LocalCopyOptions::default().size_only(true)),
        ("checksum", LocalCopyOptions::default().checksum(true)),
        ("times", LocalCopyOptions::default().times(true)),
    ] {
        // Clean destination
        if destination.exists() {
            fs::remove_file(&destination).expect("clean");
        }

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .unwrap_or_else(|_| panic!("{label} copy succeeds"));

        assert_eq!(
            summary.files_copied(),
            1,
            "{label}: new file should always be transferred"
        );
        assert_eq!(fs::read(&destination).expect("read"), b"content");
    }
}
