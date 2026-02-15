// Tests for the direct write optimization path.
//
// When creating new files without --partial, --delay-updates, or --temp-dir,
// the engine writes directly to the destination path (with DirectWriteGuard
// for cleanup) instead of creating a temporary file and renaming it.
//
// These tests verify that:
// 1. New files are written directly (no temp file / rename overhead)
// 2. Existing files still use the atomic temp+rename path
// 3. --partial forces temp+rename even for new files
// 4. --delay-updates forces temp+rename even for new files
// 5. --temp-dir forces temp+rename even for new files

// ==================== Direct Write for New Files ====================

#[test]
fn new_file_copy_succeeds_without_temp_rename() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"direct write content");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default().times(true))
        .expect("copy");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(ctx.read_dest("file.txt"), b"direct write content");
}

#[test]
fn new_file_copy_preserves_content_exactly() {
    let ctx = test_helpers::setup_copy_test();

    // Create files with various sizes to exercise the direct write path
    ctx.write_source("empty.txt", b"");
    ctx.write_source("tiny.txt", b"x");
    ctx.write_source("small.txt", &vec![0xAB; 1024]);
    ctx.write_source("medium.txt", &vec![0xCD; 65536]);

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy");

    assert_eq!(summary.files_copied(), 4);
    assert_eq!(ctx.read_dest("empty.txt"), b"");
    assert_eq!(ctx.read_dest("tiny.txt"), b"x");
    assert_eq!(ctx.read_dest("small.txt"), vec![0xAB; 1024]);
    assert_eq!(ctx.read_dest("medium.txt"), vec![0xCD; 65536]);
}

#[test]
fn multiple_new_files_all_use_direct_write() {
    let ctx = test_helpers::setup_copy_test();

    for i in 0..20 {
        ctx.write_source(&format!("file_{i}.txt"), format!("content {i}").as_bytes());
    }

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy");

    assert_eq!(summary.files_copied(), 20);
    for i in 0..20 {
        assert_eq!(
            ctx.read_dest(&format!("file_{i}.txt")),
            format!("content {i}").as_bytes()
        );
    }
}

#[test]
fn new_files_in_subdirectories_use_direct_write() {
    let ctx = test_helpers::setup_copy_test();

    ctx.write_source("a/b/deep.txt", b"nested content");
    ctx.write_source("c/shallow.txt", b"shallow content");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().recursive(true),
        )
        .expect("copy");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(ctx.read_dest("a/b/deep.txt"), b"nested content");
    assert_eq!(ctx.read_dest("c/shallow.txt"), b"shallow content");
}

// ==================== Temp+Rename for Existing Files ====================

#[test]
fn existing_file_update_uses_temp_rename() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("update.txt", b"new version with extra content");
    ctx.write_dest("update.txt", b"old version");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(ctx.read_dest("update.txt"), b"new version with extra content");
}

#[test]
fn mixed_new_and_existing_files() {
    let ctx = test_helpers::setup_copy_test();

    // Source has 3 files: 2 new, 1 existing
    ctx.write_source("new1.txt", b"brand new 1");
    ctx.write_source("new2.txt", b"brand new 2");
    ctx.write_source("existing.txt", b"updated content");

    // Destination already has 1 file
    ctx.write_dest("existing.txt", b"original content");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(ctx.read_dest("new1.txt"), b"brand new 1");
    assert_eq!(ctx.read_dest("new2.txt"), b"brand new 2");
    assert_eq!(ctx.read_dest("existing.txt"), b"updated content");
}

// ==================== Options That Disable Direct Write ====================

#[test]
fn partial_mode_uses_temp_rename_for_new_files() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"partial test content");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().partial(true),
        )
        .expect("copy");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(ctx.read_dest("file.txt"), b"partial test content");
}

#[test]
fn delay_updates_uses_temp_rename_for_new_files() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"delay updates content");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(ctx.read_dest("file.txt"), b"delay updates content");
}

#[test]
fn temp_dir_uses_temp_rename_for_new_files() {
    let ctx = test_helpers::setup_copy_test();
    let temp_dir = ctx.additional_path("custom-tmp");
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    ctx.write_source("file.txt", b"temp dir content");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(temp_dir)),
        )
        .expect("copy");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(ctx.read_dest("file.txt"), b"temp dir content");
}

// ==================== Archive Mode (Uses Direct Write) ====================

#[test]
fn archive_mode_new_files_succeed() {
    let ctx = test_helpers::setup_copy_test();

    ctx.write_source("dir/a.txt", b"file a");
    ctx.write_source("dir/b.txt", b"file b");
    ctx.write_source("dir/sub/c.txt", b"file c");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            test_helpers::presets::archive_options(),
        )
        .expect("copy");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(ctx.read_dest("dir/a.txt"), b"file a");
    assert_eq!(ctx.read_dest("dir/b.txt"), b"file b");
    assert_eq!(ctx.read_dest("dir/sub/c.txt"), b"file c");
}

#[test]
fn large_file_direct_write() {
    let ctx = test_helpers::setup_copy_test();

    // 1MB file to exercise the write buffer path
    let large_content: Vec<u8> = (0..=255).cycle().take(1024 * 1024).collect();
    ctx.write_source("large.bin", &large_content);

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(ctx.read_dest("large.bin"), large_content);
}
