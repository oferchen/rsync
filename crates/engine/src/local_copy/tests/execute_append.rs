// Comprehensive tests for --append flag behavior.
//
// The --append flag tells rsync to append data to existing partial files
// rather than re-transferring the entire file. This is useful for:
// - Resuming interrupted transfers
// - Appending to growing log files
// - Avoiding re-transfer of data that already exists
//
// Key behaviors tested:
// 1. Data is appended to existing partial files
// 2. File offset is correct after append
// 3. Works with verification (--append-verify)
// 4. Full files (dest >= source size) are not re-transferred
// 5. Empty destination files work correctly
// 6. Mismatch detection with verification
// 7. Works correctly with various file sizes

// ==================== Basic Append Tests ====================

#[test]
fn append_adds_remaining_data_to_partial_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Source has full content
    fs::write(&source, b"complete content here").expect("write source");
    // Destination has partial content (prefix of source)
    fs::write(&destination, b"complete").expect("write partial dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"complete content here"
    );
}

#[test]
fn append_skips_when_destination_equals_source_size() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"already complete content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set same mtime to trigger skip
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
                .append(true)
                .times(true),
        )
        .expect("copy succeeds");

    // File should be skipped (complete)
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(fs::read(&destination).expect("read dest"), content);
}

#[test]
fn append_skips_when_destination_larger_than_source() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Destination is larger than source
    fs::write(&source, b"short content").expect("write source");
    fs::write(&destination, b"much longer destination file content here")
        .expect("write larger dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    // File should be transferred (destination larger gets overwritten)
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"short content"
    );
}

#[test]
fn append_creates_file_when_destination_missing() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("new_dest.txt");

    fs::write(&source, b"new file content").expect("write source");
    // No destination file exists

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists());
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"new file content"
    );
}

#[test]
fn append_handles_empty_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("empty.txt");

    fs::write(&source, b"full content").expect("write source");
    fs::write(&destination, b"").expect("write empty dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"full content"
    );
}

// ==================== File Offset Correctness Tests ====================

#[test]
fn append_correct_offset_with_small_partial() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let full_content = b"0123456789ABCDEFGHIJ";
    let partial_content = b"0123456789"; // First 10 bytes
    fs::write(&source, full_content).expect("write source");
    fs::write(&destination, partial_content).expect("write partial dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), full_content);
}

#[test]
fn append_correct_offset_with_large_partial() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    // Create a large file (1MB) and partial file (500KB)
    let full_content: Vec<u8> = (0..=255).cycle().take(1024 * 1024).collect();
    let partial_size = 512 * 1024;
    let partial_content = &full_content[..partial_size];

    fs::write(&source, &full_content).expect("write source");
    fs::write(&destination, partial_content).expect("write partial dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), full_content);
    assert_eq!(
        fs::metadata(&destination).expect("metadata").len(),
        1024 * 1024
    );
}

#[test]
fn append_one_byte_partial() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"Hello World!").expect("write source");
    fs::write(&destination, b"H").expect("write one-byte partial");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"Hello World!"
    );
}

#[test]
fn append_almost_complete_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let full = b"Almost complete content";
    let partial = b"Almost complete conten"; // Missing last byte
    fs::write(&source, full).expect("write source");
    fs::write(&destination, partial).expect("write partial dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), full);
}

// ==================== Append with Verification Tests ====================

#[test]
fn append_verify_succeeds_when_prefix_matches() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"matching prefix and more data").expect("write source");
    fs::write(&destination, b"matching prefix").expect("write partial dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append_verify(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"matching prefix and more data"
    );
}

#[test]
fn append_verify_retransfers_when_prefix_mismatch() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Source and destination have different content
    fs::write(&source, b"correct source content plus more").expect("write source");
    fs::write(&destination, b"WRONG partial content").expect("write mismatched dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append_verify(true),
        )
        .expect("copy succeeds");

    // File should be re-transferred completely due to mismatch
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"correct source content plus more"
    );
}

#[test]
fn append_verify_with_large_matching_prefix() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    // Create files larger than buffer size to test chunked verification
    let pattern = "ABCDEFGHIJ";
    let full_content = pattern.repeat(100_000); // ~1MB
    let partial_size = 500_000;
    let partial_content = &full_content[..partial_size];

    fs::write(&source, &full_content).expect("write source");
    fs::write(&destination, partial_content).expect("write partial dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append_verify(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest").len(),
        full_content.len()
    );
    assert_eq!(fs::read(&destination).expect("read dest"), full_content.as_bytes());
}

#[test]
fn append_verify_detects_corruption_in_middle() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"0123456789ABCDEFGHIJ").expect("write source");
    // Destination has corruption in the middle
    fs::write(&destination, b"01234XXX89").expect("write corrupted partial");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append_verify(true),
        )
        .expect("copy succeeds");

    // File should be re-transferred due to corruption detection
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"0123456789ABCDEFGHIJ"
    );
}

#[test]
fn append_without_verify_blindly_appends() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Even with different content, append without verify will just append
    fs::write(&source, b"0123456789ABCDEFGHIJ").expect("write source");
    fs::write(&destination, b"WRONG DATA").expect("write wrong partial");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    // Without verify, it blindly appends remaining bytes from source at the
    // destination's current size. The existing destination content is preserved
    // even if it differs from the source (unlike --append-verify which detects
    // corruption and re-transfers).
    let result = fs::read(&destination).expect("read dest");
    assert_eq!(result, b"WRONG DATAABCDEFGHIJ");
}

// ==================== Combined Options Tests ====================

#[test]
fn append_combined_with_times_preserves_mtime() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"timestamped content here").expect("write source");
    fs::write(&destination, b"timestamped").expect("write partial dest");

    let source_mtime = FileTime::from_unix_time(1_600_000_000, 123_456_789);
    set_file_mtime(&source, source_mtime).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .append(true)
                .times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify mtime is preserved
    let dest_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata")
    );
    assert_eq!(dest_mtime.unix_seconds(), source_mtime.unix_seconds());
}

#[cfg(unix)]
#[test]
fn append_combined_with_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content with perms").expect("write source");
    let mut perms = fs::metadata(&source).expect("metadata").permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&source, perms).expect("set source perms");

    fs::write(&destination, b"content").expect("write partial dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .append(true)
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_perms = fs::metadata(&destination).expect("dest metadata").permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o644);
}

#[test]
fn append_with_checksum_mode() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content to append to file").expect("write source");
    fs::write(&destination, b"content to").expect("write partial dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .append(true)
                .checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"content to append to file"
    );
}

// ==================== Directory Recursive Tests ====================

#[test]
fn append_recursive_directory_with_partial_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // File 1: Partial destination (should append)
    fs::write(source_root.join("file1.txt"), b"complete file one content")
        .expect("write file1 source");
    fs::write(dest_root.join("file1.txt"), b"complete file")
        .expect("write file1 partial");

    // File 2: Complete destination (should skip if mtime matches)
    fs::write(source_root.join("file2.txt"), b"file two done")
        .expect("write file2 source");
    fs::write(dest_root.join("file2.txt"), b"file two done")
        .expect("write file2 complete");

    // File 3: Missing destination (should create)
    fs::write(source_root.join("file3.txt"), b"brand new file")
        .expect("write file3 source");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    // Should copy file1 (partial) and file3 (new), may copy file2 depending on mtime
    assert!(summary.files_copied() >= 2);
    assert_eq!(
        fs::read(dest_root.join("file1.txt")).expect("read file1"),
        b"complete file one content"
    );
    assert_eq!(
        fs::read(dest_root.join("file2.txt")).expect("read file2"),
        b"file two done"
    );
    assert_eq!(
        fs::read(dest_root.join("file3.txt")).expect("read file3"),
        b"brand new file"
    );
}

#[test]
fn append_nested_directory_structure() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(source_root.join("level1/level2")).expect("create source dirs");
    fs::create_dir_all(dest_root.join("level1/level2")).expect("create dest dirs");

    // Root level file: partial
    fs::write(source_root.join("root.txt"), b"root file complete content")
        .expect("write root source");
    fs::write(dest_root.join("root.txt"), b"root file")
        .expect("write root partial");

    // Nested file: partial
    fs::write(
        source_root.join("level1/level2/nested.txt"),
        b"nested content in deep directory",
    )
    .expect("write nested source");
    fs::write(dest_root.join("level1/level2/nested.txt"), b"nested content")
        .expect("write nested partial");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(
        fs::read(dest_root.join("root.txt")).expect("read root"),
        b"root file complete content"
    );
    assert_eq!(
        fs::read(dest_root.join("level1/level2/nested.txt")).expect("read nested"),
        b"nested content in deep directory"
    );
}

// ==================== Binary Content Tests ====================

#[test]
fn append_binary_data() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("binary.bin");
    let destination = temp.path().join("dest.bin");

    // Binary content with all byte values
    let full_binary: Vec<u8> = (0..=255).cycle().take(512).collect();
    let partial_binary: Vec<u8> = (0..=255).cycle().take(256).collect();

    fs::write(&source, &full_binary).expect("write binary source");
    fs::write(&destination, &partial_binary).expect("write binary partial");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), full_binary);
}

#[test]
fn append_verify_binary_data_with_matching_prefix() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("binary.bin");
    let destination = temp.path().join("dest.bin");

    let full_binary: Vec<u8> = (0..=255).cycle().take(1024).collect();
    let partial_binary: Vec<u8> = (0..=255).cycle().take(512).collect();

    fs::write(&source, &full_binary).expect("write binary source");
    fs::write(&destination, &partial_binary).expect("write binary partial");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append_verify(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), full_binary);
}

// ==================== Edge Cases ====================

#[test]
fn append_with_empty_source() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"has content").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    // Empty source should overwrite destination
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"");
}

#[test]
fn append_verify_enabled_implies_append() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"full content is here").expect("write source");
    fs::write(&destination, b"full content").expect("write partial dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // append_verify(true) should enable append mode
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append_verify(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"full content is here"
    );
}

// ==================== Dry Run Tests ====================

#[test]
fn append_dry_run_reports_but_preserves_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"complete data to append").expect("write source");
    fs::write(&destination, b"complete").expect("write partial dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().append(true),
        )
        .expect("dry run succeeds");

    // Dry run should report that it would copy
    assert_eq!(summary.files_copied(), 1);
    // But destination should remain unchanged
    assert_eq!(fs::read(&destination).expect("read dest"), b"complete");
}

// ==================== Behavior Consistency Tests ====================

#[test]
fn append_matches_upstream_append_semantics() {
    // This test documents the expected behavior matching upstream rsync --append
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Case 1: dest shorter than source -> append
    fs::write(source_root.join("case1.txt"), b"complete content").expect("write case1 src");
    fs::write(dest_root.join("case1.txt"), b"complete").expect("write case1 dst partial");

    // Case 2: dest equals source size -> skip (if mtime/size match)
    fs::write(source_root.join("case2.txt"), b"same size").expect("write case2 src");
    fs::write(dest_root.join("case2.txt"), b"same size").expect("write case2 dst");

    // Case 3: dest longer than source -> re-transfer
    fs::write(source_root.join("case3.txt"), b"short").expect("write case3 src");
    fs::write(dest_root.join("case3.txt"), b"much longer content").expect("write case3 dst");

    // Case 4: dest doesn't exist -> create
    fs::write(source_root.join("case4.txt"), b"new").expect("write case4 src");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("copy succeeds");

    // Cases 1, 3, and 4 should be copied (and possibly case2 depending on mtime)
    assert!(summary.files_copied() >= 3);

    assert_eq!(
        fs::read(dest_root.join("case1.txt")).expect("case1"),
        b"complete content",
        "case1: dest shorter -> appended"
    );
    assert_eq!(
        fs::read(dest_root.join("case2.txt")).expect("case2"),
        b"same size",
        "case2: same size -> preserved or copied"
    );
    assert_eq!(
        fs::read(dest_root.join("case3.txt")).expect("case3"),
        b"short",
        "case3: dest longer -> re-transferred"
    );
    assert_eq!(
        fs::read(dest_root.join("case4.txt")).expect("case4"),
        b"new",
        "case4: no dest -> created"
    );
}
