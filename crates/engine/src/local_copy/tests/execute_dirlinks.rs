
#[cfg(unix)]
#[test]
fn execute_with_copy_unsafe_links_materialises_file_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let outside_file = temp.path().join("outside.txt");
    fs::write(&outside_file, b"payload").expect("write outside file");

    let link_path = source_dir.join("escape");
    symlink(&outside_file, &link_path).expect("create symlink");
    let destination_path = dest_dir.join("escape");

    let operands = vec![
        link_path.into_os_string(),
        destination_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .safe_links(true)
                .copy_unsafe_links(true),
        )
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&destination_path).expect("materialised metadata");
    assert!(metadata.file_type().is_file());
    assert!(!metadata.file_type().is_symlink());
    assert_eq!(
        fs::read(&destination_path).expect("read materialised file"),
        b"payload"
    );
    assert_eq!(summary.symlinks_total(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_with_copy_unsafe_links_materialises_directory_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let outside_dir = temp.path().join("outside-dir");
    fs::create_dir(&outside_dir).expect("create outside dir");
    let outside_file = outside_dir.join("file.txt");
    fs::write(&outside_file, b"external").expect("write outside file");

    let link_path = source_dir.join("dirlink");
    symlink(&outside_dir, &link_path).expect("create dir symlink");
    let destination_path = dest_dir.join("dirlink");

    let operands = vec![
        link_path.into_os_string(),
        destination_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .safe_links(true)
                .copy_unsafe_links(true),
        )
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&destination_path).expect("materialised metadata");
    assert!(metadata.file_type().is_dir());
    assert!(!metadata.file_type().is_symlink());
    let copied_file = destination_path.join("file.txt");
    assert_eq!(
        fs::read(&copied_file).expect("read copied file"),
        b"external"
    );
    assert_eq!(summary.symlinks_total(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_with_keep_dirlinks_allows_destination_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src-dir");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"payload").expect("write source file");

    let actual_destination = temp.path().join("actual-destination");
    fs::create_dir(&actual_destination).expect("create destination dir");
    let destination_link = temp.path().join("dest-link");
    symlink(&actual_destination, &destination_link).expect("create destination link");

    let operands = vec![
        source_dir.into_os_string(),
        destination_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().keep_dirlinks(true),
        )
        .expect("copy succeeds");

    let copied_file = actual_destination.join("src-dir").join("file.txt");
    assert_eq!(
        fs::read(&copied_file).expect("read copied file"),
        b"payload"
    );
    assert!(
        fs::symlink_metadata(&destination_link)
            .expect("destination link metadata")
            .file_type()
            .is_symlink()
    );
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_without_keep_dirlinks_rejects_destination_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src-dir");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"payload").expect("write source file");

    let actual_destination = temp.path().join("actual-destination");
    fs::create_dir(&actual_destination).expect("create destination dir");
    let destination_link = temp.path().join("dest-link");
    symlink(&actual_destination, &destination_link).expect("create destination link");

    let operands = vec![
        source_dir.into_os_string(),
        destination_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default());

    let error = result.expect_err("keep-dirlinks disabled should reject destination symlink");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(
            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory
        )
    ));
    assert!(
        fs::symlink_metadata(&destination_link)
            .expect("destination link metadata")
            .file_type()
            .is_symlink()
    );
    assert!(!actual_destination.join("src-dir").join("file.txt").exists());
}

// ============================================================================
// Test 1: keep_dirlinks preserves symlink-to-dir subdirectory during recursive copy
// ============================================================================

#[cfg(unix)]
#[test]
fn keep_dirlinks_preserves_symlink_subdir_during_recursive_copy() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("subdir")).expect("create source subdir");
    fs::write(source_root.join("subdir/file.txt"), b"through-link").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Create a real directory as the symlink target, then make dest/subdir a symlink to it
    let real_target = temp.path().join("real-target");
    fs::create_dir(&real_target).expect("create real target");
    symlink(&real_target, dest_root.join("subdir")).expect("create symlink subdir");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().keep_dirlinks(true),
    )
    .expect("copy with keep_dirlinks succeeds");

    // The symlink should still be a symlink
    let subdir_meta = fs::symlink_metadata(dest_root.join("subdir")).expect("subdir metadata");
    assert!(
        subdir_meta.file_type().is_symlink(),
        "subdir should remain a symlink with -K"
    );

    // The file should have been placed through the symlink into the real target
    assert_eq!(
        fs::read(real_target.join("file.txt")).expect("read file through symlink"),
        b"through-link"
    );
}

// ============================================================================
// Test 2: Without -K + --force: symlink-to-dir is replaced with real directory
// ============================================================================

#[cfg(unix)]
#[test]
fn without_keep_dirlinks_force_replaces_symlink_subdir_with_real_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("subdir")).expect("create source subdir");
    fs::write(source_root.join("subdir/file.txt"), b"replaced").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Create a real directory as the symlink target, then make dest/subdir a symlink to it
    let real_target = temp.path().join("real-target");
    fs::create_dir(&real_target).expect("create real target");
    symlink(&real_target, dest_root.join("subdir")).expect("create symlink subdir");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .keep_dirlinks(false)
            .force_replacements(true),
    )
    .expect("copy with force succeeds");

    // Without -K, --force should replace the symlink with a real directory
    let subdir_meta = fs::symlink_metadata(dest_root.join("subdir")).expect("subdir metadata");
    assert!(
        subdir_meta.file_type().is_dir(),
        "symlink should be replaced by real directory with --force"
    );
    assert!(
        !subdir_meta.file_type().is_symlink(),
        "should no longer be a symlink"
    );

    // The file should exist in the real directory (not the original symlink target)
    assert_eq!(
        fs::read(dest_root.join("subdir/file.txt")).expect("read file"),
        b"replaced"
    );
    // The original symlink target should not have the file
    assert!(
        !real_target.join("file.txt").exists(),
        "file should not appear in original symlink target"
    );
}

// ============================================================================
// Test 3: -K with symlink-to-file (not dir): correctly errors
// ============================================================================

#[cfg(unix)]
#[test]
fn keep_dirlinks_with_symlink_to_file_errors() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("subdir")).expect("create source subdir");
    fs::write(source_root.join("subdir/child.txt"), b"data").expect("write child");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Make dest/subdir a symlink to a regular file (not a directory)
    let file_target = temp.path().join("target-file.txt");
    fs::write(&file_target, b"not-a-dir").expect("write target file");
    symlink(&file_target, dest_root.join("subdir")).expect("create symlink-to-file");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().keep_dirlinks(true),
        )
        .expect_err("should fail when symlink target is a file, not a directory");

    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(
            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory
        )
    ));

    // The symlink should remain untouched
    let meta = fs::symlink_metadata(dest_root.join("subdir")).expect("subdir metadata");
    assert!(meta.file_type().is_symlink(), "symlink should remain");
}

// ============================================================================
// Test 4: -K + --force with symlink-to-file: force-replaces with real directory
// ============================================================================

#[cfg(unix)]
#[test]
fn keep_dirlinks_force_replaces_symlink_to_file_with_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("subdir")).expect("create source subdir");
    fs::write(source_root.join("subdir/child.txt"), b"forced").expect("write child");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Make dest/subdir a symlink to a regular file (not a directory)
    let file_target = temp.path().join("target-file.txt");
    fs::write(&file_target, b"not-a-dir").expect("write target file");
    symlink(&file_target, dest_root.join("subdir")).expect("create symlink-to-file");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .keep_dirlinks(true)
            .force_replacements(true),
    )
    .expect("copy with -K and --force succeeds");

    // The symlink-to-file should be replaced by a real directory
    let meta = fs::symlink_metadata(dest_root.join("subdir")).expect("subdir metadata");
    assert!(
        meta.file_type().is_dir(),
        "symlink-to-file should be replaced by real directory"
    );
    assert!(
        !meta.file_type().is_symlink(),
        "should no longer be a symlink"
    );
    assert_eq!(
        fs::read(dest_root.join("subdir/child.txt")).expect("read child"),
        b"forced"
    );
}

// ============================================================================
// Test 5: Deeply nested: parent directory is symlink-to-dir, child files placed correctly
// ============================================================================

#[cfg(unix)]
#[test]
fn keep_dirlinks_deeply_nested_parent_symlink_to_dir() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("a/b/c")).expect("create source nested dirs");
    fs::write(source_root.join("a/b/c/deep.txt"), b"deep-payload").expect("write deep file");
    fs::write(source_root.join("a/b/shallow.txt"), b"shallow").expect("write shallow file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(dest_root.join("a")).expect("create dest/a");

    // Make dest/a/b a symlink to a real directory
    let real_b = temp.path().join("real-b");
    fs::create_dir_all(real_b.join("c")).expect("create real-b/c");
    symlink(&real_b, dest_root.join("a/b")).expect("create symlink a/b -> real-b");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().keep_dirlinks(true),
    )
    .expect("copy with keep_dirlinks succeeds for nested structure");

    // The symlink at dest/a/b should be preserved
    let b_meta = fs::symlink_metadata(dest_root.join("a/b")).expect("a/b metadata");
    assert!(
        b_meta.file_type().is_symlink(),
        "a/b should remain a symlink with -K"
    );

    // Files should be placed through the symlink into the real target
    assert_eq!(
        fs::read(real_b.join("shallow.txt")).expect("read shallow"),
        b"shallow"
    );
    assert_eq!(
        fs::read(real_b.join("c/deep.txt")).expect("read deep"),
        b"deep-payload"
    );
}

// ============================================================================
// Test 6: Dry-run mode: succeeds without error, no filesystem modifications
// ============================================================================

#[cfg(unix)]
#[test]
fn keep_dirlinks_dry_run_no_modifications() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("subdir")).expect("create source subdir");
    fs::write(source_root.join("subdir/file.txt"), b"new-data").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Create a real directory as the symlink target
    let real_target = temp.path().join("real-target");
    fs::create_dir(&real_target).expect("create real target");
    symlink(&real_target, dest_root.join("subdir")).expect("create symlink subdir");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::DryRun,
        LocalCopyOptions::default().keep_dirlinks(true),
    )
    .expect("dry-run with keep_dirlinks succeeds");

    // The symlink should still exist
    let subdir_meta = fs::symlink_metadata(dest_root.join("subdir")).expect("subdir metadata");
    assert!(
        subdir_meta.file_type().is_symlink(),
        "symlink should remain after dry-run"
    );

    // No file should have been written
    assert!(
        !real_target.join("file.txt").exists(),
        "file should not be created during dry-run"
    );
}

// ============================================================================
// Test 7: --delete -K: symlink-to-dir is preserved, extraneous files deleted
// ============================================================================

#[cfg(unix)]
#[test]
fn keep_dirlinks_delete_preserves_symlink_removes_extraneous() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("subdir")).expect("create source subdir");
    fs::write(source_root.join("subdir/keep.txt"), b"keep").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Create a real directory as the symlink target, with extraneous content
    let real_target = temp.path().join("real-target");
    fs::create_dir(&real_target).expect("create real target");
    fs::write(real_target.join("extra.txt"), b"extraneous").expect("write extraneous file");
    symlink(&real_target, dest_root.join("subdir")).expect("create symlink subdir");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .keep_dirlinks(true)
                .delete(true),
        )
        .expect("copy with -K and --delete succeeds");

    // The symlink should be preserved
    let subdir_meta = fs::symlink_metadata(dest_root.join("subdir")).expect("subdir metadata");
    assert!(
        subdir_meta.file_type().is_symlink(),
        "subdir symlink should be preserved with -K and --delete"
    );

    // The kept file should exist through the symlink
    assert_eq!(
        fs::read(real_target.join("keep.txt")).expect("read keep"),
        b"keep"
    );

    // The extraneous file should be deleted
    assert!(
        !real_target.join("extra.txt").exists(),
        "extraneous file should be deleted"
    );
    assert!(summary.items_deleted() >= 1, "should report at least one deletion");
}

// ============================================================================
// Test 8: Mixed: some subdirs are symlinks, some are real
// ============================================================================

#[cfg(unix)]
#[test]
fn keep_dirlinks_mixed_real_and_symlink_subdirs() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("real-sub")).expect("create source real-sub");
    fs::create_dir_all(source_root.join("link-sub")).expect("create source link-sub");
    fs::write(source_root.join("real-sub/r.txt"), b"real-content").expect("write real file");
    fs::write(source_root.join("link-sub/l.txt"), b"link-content").expect("write link file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // real-sub at destination is an actual directory
    fs::create_dir(dest_root.join("real-sub")).expect("create real dest subdir");

    // link-sub at destination is a symlink to a real directory
    let link_target = temp.path().join("link-target");
    fs::create_dir(&link_target).expect("create link target");
    symlink(&link_target, dest_root.join("link-sub")).expect("create symlink link-sub");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().keep_dirlinks(true),
    )
    .expect("copy with mixed dirs and symlinks succeeds");

    // real-sub should remain a real directory with its file
    let real_meta = fs::symlink_metadata(dest_root.join("real-sub")).expect("real-sub metadata");
    assert!(
        real_meta.file_type().is_dir(),
        "real-sub should be a real directory"
    );
    assert!(
        !real_meta.file_type().is_symlink(),
        "real-sub should not be a symlink"
    );
    assert_eq!(
        fs::read(dest_root.join("real-sub/r.txt")).expect("read real file"),
        b"real-content"
    );

    // link-sub should remain a symlink (preserved by -K)
    let link_meta = fs::symlink_metadata(dest_root.join("link-sub")).expect("link-sub metadata");
    assert!(
        link_meta.file_type().is_symlink(),
        "link-sub should remain a symlink with -K"
    );
    // File should be placed through the symlink into the real target
    assert_eq!(
        fs::read(link_target.join("l.txt")).expect("read link file"),
        b"link-content"
    );
}

// ============================================================================
// Test 9: File sender entry where dest has symlink-to-dir: -K does not apply
// ============================================================================

#[cfg(unix)]
#[test]
fn keep_dirlinks_does_not_apply_when_source_sends_file_over_symlink_to_dir() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    // Source has "item" as a regular file
    fs::write(source_root.join("item"), b"file-content").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Destination has "item" as a symlink to a real directory
    let real_dir = temp.path().join("real-item-dir");
    fs::create_dir(&real_dir).expect("create real item dir");
    fs::write(real_dir.join("inside.txt"), b"inside").expect("write inside");
    symlink(&real_dir, dest_root.join("item")).expect("create symlink item -> dir");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With -K the source sends a file where dest has a symlink-to-dir.
    // -K only applies at the directory-copy level (check_destination_state).
    // The file copy path uses symlink_metadata which sees a symlink (not a dir),
    // so it simply overwrites the symlink with the regular file content.
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().keep_dirlinks(true),
        )
        .expect("file copy over symlink-to-dir succeeds because -K does not affect file copies");

    assert_eq!(summary.files_copied(), 1);

    // The symlink should have been replaced by a regular file
    let meta = fs::symlink_metadata(dest_root.join("item")).expect("item metadata");
    assert!(
        meta.file_type().is_file(),
        "symlink should be replaced by regular file"
    );
    assert!(
        !meta.file_type().is_symlink(),
        "should no longer be a symlink"
    );
    assert_eq!(
        fs::read(dest_root.join("item")).expect("read item"),
        b"file-content"
    );

    // The original symlink target directory contents should be untouched
    assert!(real_dir.join("inside.txt").exists(), "target contents should be preserved");
}
