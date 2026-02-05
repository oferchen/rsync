
// Tests for --remove-source-files flag

#[test]
fn remove_source_files_removes_successfully_transferred_file() {
    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("file.txt"), b"content").expect("write file");

    let operands = vec![
        ctx.source.join("file.txt").into_os_string(),
        ctx.dest.join("file.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().remove_source_files(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.sources_removed(), 1);
    assert!(!ctx.source.join("file.txt").exists(), "source file should be removed");
    assert_eq!(fs::read(ctx.dest.join("file.txt")).expect("read dest"), b"content");
}

#[test]
fn remove_source_files_removes_multiple_files() {
    let ctx = test_helpers::setup_copy_test();
    test_helpers::create_test_tree(&ctx.source, &[
        ("file1.txt", Some(b"content1")),
        ("file2.txt", Some(b"content2")),
        ("file3.txt", Some(b"content3")),
    ]);
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let mut source_operand = ctx.source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().remove_source_files(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(summary.sources_removed(), 3);
    assert!(!ctx.source.join("file1.txt").exists());
    assert!(!ctx.source.join("file2.txt").exists());
    assert!(!ctx.source.join("file3.txt").exists());
    assert!(ctx.dest.join("file1.txt").exists());
    assert!(ctx.dest.join("file2.txt").exists());
    assert!(ctx.dest.join("file3.txt").exists());
}

#[test]
fn remove_source_files_preserves_unchanged_files() {
    let ctx = test_helpers::setup_copy_test();
    let content = b"unchanged";
    fs::write(ctx.source.join("file.txt"), content).expect("write source");
    fs::create_dir_all(&ctx.dest).expect("create dest");
    fs::write(ctx.dest.join("file.txt"), content).expect("write dest");

    // Set same mtime on both files so they're considered identical
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&ctx.source.join("file.txt"), timestamp, timestamp)
        .expect("set source times");
    set_file_times(&ctx.dest.join("file.txt"), timestamp, timestamp)
        .expect("set dest times");

    let operands = vec![
        ctx.source.join("file.txt").into_os_string(),
        ctx.dest.join("file.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .times(true),
        )
        .expect("execution succeeds");

    assert_eq!(summary.files_copied(), 0, "file should not be copied");
    assert_eq!(summary.sources_removed(), 0, "unchanged files should not be removed");
    assert!(ctx.source.join("file.txt").exists(), "source should remain");
    assert!(ctx.dest.join("file.txt").exists(), "dest should remain");
}

#[test]
fn remove_source_files_does_not_remove_directories() {
    let ctx = test_helpers::setup_copy_test();
    test_helpers::create_test_tree(&ctx.source, &[
        ("dir1/file.txt", Some(b"content1")),
        ("dir2/file.txt", Some(b"content2")),
    ]);
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let mut source_operand = ctx.source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().remove_source_files(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.sources_removed(), 2);
    // Files should be removed
    assert!(!ctx.source.join("dir1/file.txt").exists());
    assert!(!ctx.source.join("dir2/file.txt").exists());
    // But directories should remain
    assert!(ctx.source.join("dir1").is_dir(), "directory should not be removed");
    assert!(ctx.source.join("dir2").is_dir(), "directory should not be removed");
}

#[test]
fn remove_source_files_with_filter_only_removes_transferred() {
    let ctx = test_helpers::setup_copy_test();
    test_helpers::create_test_tree(&ctx.source, &[
        ("include.txt", Some(b"included")),
        ("exclude.log", Some(b"excluded")),
    ]);
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let mut source_operand = ctx.source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let filters = FilterSet::from_rules([FilterRule::exclude("*.log")])
        .expect("compile filters");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .filters(Some(filters)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.sources_removed(), 1);
    // Included file should be removed
    assert!(!ctx.source.join("include.txt").exists());
    // Excluded file should remain (wasn't transferred)
    assert!(ctx.source.join("exclude.log").exists());
    // Dest should only have included file
    assert!(ctx.dest.join("include.txt").exists());
    assert!(!ctx.dest.join("exclude.log").exists());
}

#[test]
fn remove_source_files_with_update_preserves_newer_dest() {
    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("file.txt"), b"old content").expect("write source");
    fs::create_dir_all(&ctx.dest).expect("create dest");
    fs::write(ctx.dest.join("file.txt"), b"new content").expect("write dest");

    // Make source older than dest
    let old_time = FileTime::from_unix_time(1_600_000_000, 0);
    let new_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&ctx.source.join("file.txt"), old_time, old_time)
        .expect("set source times");
    set_file_times(&ctx.dest.join("file.txt"), new_time, new_time)
        .expect("set dest times");

    let operands = vec![
        ctx.source.join("file.txt").into_os_string(),
        ctx.dest.join("file.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .update(true)
                .times(true),
        )
        .expect("execution succeeds");

    assert_eq!(summary.files_copied(), 0, "older file should not overwrite newer");
    assert_eq!(summary.sources_removed(), 0, "source should not be removed when not transferred");
    assert!(ctx.source.join("file.txt").exists(), "source should remain");
    assert_eq!(
        fs::read(ctx.dest.join("file.txt")).expect("read dest"),
        b"new content",
        "dest should not be modified"
    );
}

#[test]
fn remove_source_files_with_ignore_existing_preserves_source() {
    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("file.txt"), b"source content").expect("write source");
    fs::create_dir_all(&ctx.dest).expect("create dest");
    fs::write(ctx.dest.join("file.txt"), b"existing content").expect("write dest");

    let operands = vec![
        ctx.source.join("file.txt").into_os_string(),
        ctx.dest.join("file.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .ignore_existing(true),
        )
        .expect("execution succeeds");

    assert_eq!(summary.files_copied(), 0, "file should not be copied when dest exists");
    assert_eq!(summary.sources_removed(), 0, "source should not be removed when not transferred");
    assert!(ctx.source.join("file.txt").exists(), "source should remain");
    assert_eq!(
        fs::read(ctx.dest.join("file.txt")).expect("read dest"),
        b"existing content",
        "dest should not be modified"
    );
}

#[test]
fn remove_source_files_dry_run_does_not_remove() {
    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("file.txt"), b"content").expect("write file");

    let operands = vec![
        ctx.source.join("file.txt").into_os_string(),
        ctx.dest.join("file.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().remove_source_files(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.sources_removed(), 0, "dry run should not remove sources");
    assert!(ctx.source.join("file.txt").exists(), "source should remain in dry run");
    assert!(!ctx.dest.join("file.txt").exists(), "dest should not exist in dry run");
}

#[test]
fn remove_source_files_with_inplace_removes_after_update() {
    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("file.txt"), b"updated content").expect("write source");
    fs::create_dir_all(&ctx.dest).expect("create dest");
    fs::write(ctx.dest.join("file.txt"), b"old content").expect("write dest");

    let operands = vec![
        ctx.source.join("file.txt").into_os_string(),
        ctx.dest.join("file.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.sources_removed(), 1);
    assert!(!ctx.source.join("file.txt").exists());
    assert_eq!(
        fs::read(ctx.dest.join("file.txt")).expect("read dest"),
        b"updated content"
    );
}

#[test]
fn remove_source_files_removes_nested_files() {
    let ctx = test_helpers::setup_copy_test();
    test_helpers::create_test_tree(&ctx.source, &[
        ("a/b/c/file1.txt", Some(b"deep1")),
        ("a/b/file2.txt", Some(b"mid")),
        ("a/file3.txt", Some(b"shallow")),
    ]);
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let mut source_operand = ctx.source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().remove_source_files(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(summary.sources_removed(), 3);
    // All files removed
    assert!(!ctx.source.join("a/b/c/file1.txt").exists());
    assert!(!ctx.source.join("a/b/file2.txt").exists());
    assert!(!ctx.source.join("a/file3.txt").exists());
    // All directories remain
    assert!(ctx.source.join("a").is_dir());
    assert!(ctx.source.join("a/b").is_dir());
    assert!(ctx.source.join("a/b/c").is_dir());
    // All files copied
    assert!(ctx.dest.join("a/b/c/file1.txt").exists());
    assert!(ctx.dest.join("a/b/file2.txt").exists());
    assert!(ctx.dest.join("a/file3.txt").exists());
}

#[cfg(unix)]
#[test]
fn remove_source_files_removes_symlinks() {
    use std::os::unix::fs::symlink;

    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("target.txt"), b"target").expect("write target");
    symlink(
        ctx.source.join("target.txt"),
        ctx.source.join("link.txt"),
    )
    .expect("create symlink");
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let operands = vec![
        ctx.source.join("link.txt").into_os_string(),
        ctx.dest.join("link.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .links(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
    assert_eq!(summary.sources_removed(), 1);
    assert!(!ctx.source.join("link.txt").exists(), "symlink should be removed");
    assert!(ctx.source.join("target.txt").exists(), "target should remain");
    assert!(ctx.dest.join("link.txt").exists(), "symlink should be copied");
}

#[test]
fn remove_source_files_with_whole_file_removes_after_copy() {
    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("file.txt"), b"content").expect("write source");
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let operands = vec![
        ctx.source.join("file.txt").into_os_string(),
        ctx.dest.join("file.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .whole_file(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.sources_removed(), 1);
    assert!(!ctx.source.join("file.txt").exists());
    assert!(ctx.dest.join("file.txt").exists());
}

#[test]
fn remove_source_files_with_min_size_only_removes_matching() {
    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("small.txt"), b"x").expect("write small");
    fs::write(ctx.source.join("large.txt"), b"x".repeat(1024)).expect("write large");
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let mut source_operand = ctx.source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .min_file_size(Some(100)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1, "only large file should be copied");
    assert_eq!(summary.sources_removed(), 1, "only large file should be removed");
    assert!(ctx.source.join("small.txt").exists(), "small file should remain");
    assert!(!ctx.source.join("large.txt").exists(), "large file should be removed");
    assert!(!ctx.dest.join("small.txt").exists());
    assert!(ctx.dest.join("large.txt").exists());
}

#[test]
fn remove_source_files_with_max_size_only_removes_matching() {
    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("small.txt"), b"x").expect("write small");
    fs::write(ctx.source.join("large.txt"), b"x".repeat(1024)).expect("write large");
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let mut source_operand = ctx.source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .max_file_size(Some(100)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1, "only small file should be copied");
    assert_eq!(summary.sources_removed(), 1, "only small file should be removed");
    assert!(!ctx.source.join("small.txt").exists(), "small file should be removed");
    assert!(ctx.source.join("large.txt").exists(), "large file should remain");
    assert!(ctx.dest.join("small.txt").exists());
    assert!(!ctx.dest.join("large.txt").exists());
}

#[test]
fn remove_source_files_with_existing_only_when_updated() {
    let ctx = test_helpers::setup_copy_test();
    // Use different sizes to trigger transfer
    fs::write(ctx.source.join("update.txt"), b"new_data").expect("write update");
    fs::write(ctx.source.join("new.txt"), b"new file").expect("write new");
    fs::create_dir_all(&ctx.dest).expect("create dest");
    fs::write(ctx.dest.join("update.txt"), b"old_value").expect("write old");

    let mut source_operand = ctx.source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .existing_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1, "only existing file should be updated");
    assert_eq!(summary.sources_removed(), 1, "only updated file should be removed");
    assert!(!ctx.source.join("update.txt").exists(), "updated file should be removed");
    assert!(ctx.source.join("new.txt").exists(), "new file should remain (not transferred)");
    assert!(ctx.dest.join("update.txt").exists());
    assert!(!ctx.dest.join("new.txt").exists());
}

#[cfg(unix)]
#[test]
fn remove_source_files_with_permissions_removes_after_copy() {
    use std::os::unix::fs::PermissionsExt;

    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("file.txt"), b"content").expect("write source");

    let mut perms = fs::metadata(ctx.source.join("file.txt"))
        .expect("source metadata")
        .permissions();
    perms.set_mode(0o600);
    fs::set_permissions(ctx.source.join("file.txt"), perms).expect("set source perms");
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let operands = vec![
        ctx.source.join("file.txt").into_os_string(),
        ctx.dest.join("file.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.sources_removed(), 1);
    assert!(!ctx.source.join("file.txt").exists());

    let dest_perms = fs::metadata(ctx.dest.join("file.txt"))
        .expect("dest metadata")
        .permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o600, "permissions should be preserved");
}

#[test]
fn remove_source_files_summary_tracks_count() {
    let ctx = test_helpers::setup_copy_test();
    test_helpers::create_test_tree(&ctx.source, &[
        ("file1.txt", Some(b"one")),
        ("file2.txt", Some(b"two")),
        ("file3.txt", Some(b"three")),
    ]);
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let mut source_operand = ctx.source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().remove_source_files(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.sources_removed(), 3);
    assert_eq!(summary.files_copied(), 3);
}

#[test]
fn remove_source_files_with_relative_paths() {
    let ctx = test_helpers::setup_copy_test();
    test_helpers::create_test_tree(&ctx.source, &[
        ("dir1/file1.txt", Some(b"content1")),
        ("dir2/file2.txt", Some(b"content2")),
    ]);
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let operands = vec![
        ctx.source.join(".").join("dir1").join("file1.txt").into_os_string(),
        ctx.source.join(".").join("dir2").join("file2.txt").into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .remove_source_files(true)
                .relative_paths(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.sources_removed(), 2);
    assert!(!ctx.source.join("dir1/file1.txt").exists());
    assert!(!ctx.source.join("dir2/file2.txt").exists());
    assert!(ctx.dest.join("dir1/file1.txt").exists());
    assert!(ctx.dest.join("dir2/file2.txt").exists());
}
