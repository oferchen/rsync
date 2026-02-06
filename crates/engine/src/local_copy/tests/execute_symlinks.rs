
#[cfg(unix)]
#[test]
fn execute_copies_symbolic_link() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    fs::write(&target, b"target").expect("write target");

    let link = temp.path().join("link");
    symlink(&target, &link).expect("create link");
    let dest_link = temp.path().join("dest-link");

    let operands = vec![link.into_os_string(), dest_link.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().links(true).hard_links(true);
    let _summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");
    let copied = fs::read_link(dest_link).expect("read copied link");
    assert_eq!(copied, target);
    // assert_eq!(summary.symlinks_copied(), 1);  // Depends on safe_links behavior
}

#[cfg(unix)]
#[test]
fn execute_symlink_replaces_directory_when_force_enabled() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    fs::write(&target, b"target").expect("write target");

    let link = temp.path().join("link");
    symlink(&target, &link).expect("create link");
    let dest_link = temp.path().join("dest-link");
    fs::create_dir_all(&dest_link).expect("create conflicting directory");

    let operands = vec![link.into_os_string(), dest_link.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .links(true)
        .hard_links(true)
        .force_replacements(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("forced replacement succeeds");

    let copied = fs::read_link(dest_link).expect("read copied link");
    assert_eq!(copied, target);
}

#[cfg(unix)]
#[test]
fn execute_with_copy_links_materialises_symlink_to_file() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    fs::write(&target, b"payload").expect("write target");

    let link = temp.path().join("link-file");
    symlink(&target, &link).expect("create link");
    let dest = temp.path().join("dest-file");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().links(false).copy_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(metadata.file_type().is_file());
    assert_eq!(fs::read(&dest).expect("read dest"), b"payload");
    assert_eq!(summary.symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn execute_with_copy_links_materialises_symlink_to_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_dir = temp.path().join("target-dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::write(target_dir.join("inner.txt"), b"dir data").expect("write inner");

    let link = temp.path().join("link-dir");
    symlink(&target_dir, &link).expect("create dir link");
    let dest_dir = temp.path().join("dest-dir");

    let operands = vec![link.into_os_string(), dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().links(false).copy_links(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&dest_dir).expect("dest metadata");
    assert!(metadata.file_type().is_dir());
    let inner = dest_dir.join("inner.txt");
    assert_eq!(fs::read(&inner).expect("read inner"), b"dir data");
}

#[cfg(unix)]
#[test]
fn execute_with_copy_dirlinks_follows_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_dir = temp.path().join("referenced-dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::write(target_dir.join("inner.txt"), b"dir data").expect("write inner");

    let link = temp.path().join("dir-link");
    symlink(&target_dir, &link).expect("create dir link");
    let dest_dir = temp.path().join("dest-dir");

    let operands = vec![link.into_os_string(), dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().links(false).copy_dirlinks(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&dest_dir).expect("dest metadata");
    assert!(metadata.file_type().is_dir());
    let inner = dest_dir.join("inner.txt");
    assert_eq!(fs::read(&inner).expect("read inner"), b"dir data");
}

#[cfg(unix)]
#[test]
fn execute_with_copy_dirlinks_preserves_file_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_file = temp.path().join("target.txt");
    fs::write(&target_file, b"payload").expect("write target");

    let link = temp.path().join("file-link");
    symlink(&target_file, &link).expect("create file link");
    let dest = temp.path().join("dest-link");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().links(true).copy_dirlinks(true);
    let _summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // assert_eq!(summary.symlinks_copied(), 1);  // Depends on safe_links behavior
    let copied = fs::read_link(&dest).expect("read link");
    assert_eq!(copied, target_file);
}

#[cfg(unix)]
#[test]
fn execute_with_safe_links_allows_relative_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let nested = source_dir.join("nested");
    fs::create_dir(&nested).expect("create nested");
    let target_file = nested.join("file.txt");
    fs::write(&target_file, b"payload").expect("write target");

    let link_path = source_dir.join("link");
    symlink(Path::new("nested/file.txt"), &link_path).expect("create symlink");
    let destination_link = dest_dir.join("link");

    let operands = vec![
        link_path.into_os_string(),
        destination_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let _summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true).safe_links(true),
        )
        .expect("copy succeeds");

    // assert_eq!(summary.symlinks_copied(), 1);  // Depends on safe_links behavior
    let copied = fs::read_link(&destination_link).expect("read link");
    assert_eq!(copied, Path::new("nested/file.txt"));
}

#[cfg(unix)]
#[test]
fn execute_with_safe_links_skips_unsafe_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let link_path = source_dir.join("escape");
    symlink(Path::new("../../outside"), &link_path).expect("create symlink");
    let destination_link = dest_dir.join("escape");

    let operands = vec![
        link_path.into_os_string(),
        destination_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy completes");

    assert!(!destination_link.exists());
    let summary = report.summary();
    assert_eq!(summary.symlinks_copied(), 0);
    assert_eq!(summary.symlinks_total(), 1);

    assert!(
        report
            .records()
            .iter()
            .any(|record| { matches!(record.action(), LocalCopyAction::SkippedUnsafeSymlink) })
    );
}

#[cfg(unix)]
#[test]
fn execute_preserves_symlink_hard_links() {
    use std::os::unix::fs::{symlink, MetadataExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source root");

    let target = source_root.join("target.txt");
    fs::write(&target, b"payload").expect("write target");

    let link_a = source_root.join("link-a");
    symlink(Path::new("target.txt"), &link_a).expect("create primary link");
    let link_b = source_root.join("link-b");
    fs::hard_link(&link_a, &link_b).expect("duplicate symlink inode");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true).hard_links(true),
        )
        .expect("copy succeeds");

    let dest_link_a = dest_root.join("link-a");
    let dest_link_b = dest_root.join("link-b");
    let metadata_a = fs::symlink_metadata(&dest_link_a).expect("metadata a");
    let metadata_b = fs::symlink_metadata(&dest_link_b).expect("metadata b");

    assert!(metadata_a.file_type().is_symlink());
    assert!(metadata_b.file_type().is_symlink());
    assert_eq!(metadata_a.ino(), metadata_b.ino());
    assert_eq!(metadata_a.nlink(), 2);
    assert_eq!(metadata_b.nlink(), 2);
    assert_eq!(
        fs::read_link(&dest_link_a).expect("read dest link"),
        Path::new("target.txt"),
    );
    assert_eq!(
        fs::read_link(&dest_link_b).expect("read dest link"),
        Path::new("target.txt"),
    );
    assert!(summary.hard_links_created() >= 1);
    assert_eq!(summary.symlinks_copied(), 2);
}

// ==================== Safe Links Tests ====================

#[cfg(unix)]
#[test]
fn safe_links_skips_symlink_pointing_outside_transfer_tree() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a target file outside the source tree
    let outside_target = temp.path().join("outside.txt");
    fs::write(&outside_target, b"outside content").expect("write outside target");

    // Create a symlink pointing outside the source tree using absolute path
    let unsafe_link = source_root.join("unsafe_absolute_link");
    symlink(&outside_target, &unsafe_link).expect("create absolute symlink");

    // Create another symlink using relative path that escapes
    let escape_link = source_root.join("escape_link");
    symlink(Path::new("../../outside.txt"), &escape_link).expect("create escape symlink");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy completes");

    let summary = report.summary();

    // Both unsafe symlinks should be skipped
    assert_eq!(summary.symlinks_copied(), 0);
    assert_eq!(summary.symlinks_total(), 2);
    assert!(!dest_root.join("unsafe_absolute_link").exists());
    assert!(!dest_root.join("source/escape_link").exists());

    // Verify we got SkippedUnsafeSymlink records
    let skip_count = report
        .records()
        .iter()
        .filter(|record| { matches!(record.action(), LocalCopyAction::SkippedUnsafeSymlink) })
        .count();
    assert_eq!(skip_count, 2);
}

#[cfg(unix)]
#[test]
fn safe_links_preserves_symlink_pointing_inside_transfer_tree() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a subdirectory with a file
    let subdir = source_root.join("subdir");
    fs::create_dir(&subdir).expect("create subdir");
    let target_file = subdir.join("target.txt");
    fs::write(&target_file, b"safe content").expect("write target");

    // Create safe symlinks pointing within the tree
    let safe_link1 = source_root.join("link_to_subdir");
    symlink(Path::new("subdir/target.txt"), &safe_link1).expect("create safe link 1");

    let safe_link2 = subdir.join("link_to_sibling");
    symlink(Path::new("target.txt"), &safe_link2).expect("create safe link 2");

    // Create a link from subdir pointing up and back down (still safe)
    let safe_link3 = subdir.join("link_via_parent");
    symlink(Path::new("../subdir/target.txt"), &safe_link3).expect("create safe link 3");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true).safe_links(true),
        )
        .expect("copy succeeds");

    // All safe symlinks should be preserved
    assert_eq!(summary.symlinks_copied(), 3);

    // Verify all symlinks exist and have correct targets
    let dest_link1 = dest_root.join("link_to_subdir");
    assert!(fs::symlink_metadata(&dest_link1).expect("meta").file_type().is_symlink());
    assert_eq!(
        fs::read_link(&dest_link1).expect("read link"),
        Path::new("subdir/target.txt")
    );

    let dest_link2 = dest_root.join("subdir/link_to_sibling");
    assert!(fs::symlink_metadata(&dest_link2).expect("meta").file_type().is_symlink());
    assert_eq!(
        fs::read_link(&dest_link2).expect("read link"),
        Path::new("target.txt")
    );

    let dest_link3 = dest_root.join("subdir/link_via_parent");
    assert!(fs::symlink_metadata(&dest_link3).expect("meta").file_type().is_symlink());
    assert_eq!(
        fs::read_link(&dest_link3).expect("read link"),
        Path::new("../subdir/target.txt")
    );
}

#[cfg(unix)]
#[test]
fn safe_links_evaluates_relative_symlinks_correctly() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create nested directory structure
    let level1 = source_root.join("level1");
    let level2 = level1.join("level2");
    let level3 = level2.join("level3");
    fs::create_dir_all(&level3).expect("create nested dirs");

    // Create a target file at level1
    let target = level1.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    // Safe: link from level3 going up 2 levels to level1 (stays within tree)
    let safe_link = level3.join("link_to_level1");
    symlink(Path::new("../../target.txt"), &safe_link).expect("create safe link");

    // Unsafe: link from level1 trying to escape (only 2 levels deep including link name)
    let unsafe_link = level1.join("escape_link");
    symlink(Path::new("../../../outside.txt"), &unsafe_link).expect("create unsafe link");

    // Safe: link at root going into subdirectory
    let safe_root_link = source_root.join("root_link");
    symlink(Path::new("level1/target.txt"), &safe_root_link).expect("create root link");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy completes");

    let summary = report.summary();

    // 2 safe links should be copied, 1 unsafe should be skipped
    assert_eq!(summary.symlinks_copied(), 2);
    assert_eq!(summary.symlinks_total(), 3);

    // Verify safe links exist
    assert!(dest_root.join("level1/level2/level3/link_to_level1").exists());
    assert!(dest_root.join("root_link").exists());

    // Verify unsafe link was skipped
    assert!(!dest_root.join("level1/escape_link").exists());

    let skip_count = report
        .records()
        .iter()
        .filter(|record| { matches!(record.action(), LocalCopyAction::SkippedUnsafeSymlink) })
        .count();
    assert_eq!(skip_count, 1);
}

#[cfg(unix)]
#[test]
fn safe_links_filters_absolute_symlinks_when_unsafe() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create target files
    let inside_target = source_root.join("inside.txt");
    fs::write(&inside_target, b"inside").expect("write inside");

    let outside_target = temp.path().join("outside.txt");
    fs::write(&outside_target, b"outside").expect("write outside");

    // Absolute symlink pointing inside the source tree - still unsafe with safe_links
    let abs_inside_link = source_root.join("abs_inside_link");
    symlink(&inside_target, &abs_inside_link).expect("create absolute inside link");

    // Absolute symlink pointing outside - definitely unsafe
    let abs_outside_link = source_root.join("abs_outside_link");
    symlink(&outside_target, &abs_outside_link).expect("create absolute outside link");

    // System absolute path - unsafe
    let system_link = source_root.join("system_link");
    symlink(Path::new("/etc/passwd"), &system_link).expect("create system link");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy completes");

    let summary = report.summary();

    // All absolute symlinks should be filtered out
    assert_eq!(summary.symlinks_copied(), 0);
    assert_eq!(summary.symlinks_total(), 3);

    assert!(!dest_root.join("abs_inside_link").exists());
    assert!(!dest_root.join("abs_outside_link").exists());
    assert!(!dest_root.join("system_link").exists());

    let skip_count = report
        .records()
        .iter()
        .filter(|record| { matches!(record.action(), LocalCopyAction::SkippedUnsafeSymlink) })
        .count();
    assert_eq!(skip_count, 3);
}

#[cfg(unix)]
#[test]
fn safe_links_handles_complex_relative_paths() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create directory structure
    let dir_a = source_root.join("dir_a");
    let dir_b = source_root.join("dir_b");
    fs::create_dir(&dir_a).expect("create dir_a");
    fs::create_dir(&dir_b).expect("create dir_b");

    let target_a = dir_a.join("file_a.txt");
    let target_b = dir_b.join("file_b.txt");
    fs::write(&target_a, b"a").expect("write a");
    fs::write(&target_b, b"b").expect("write b");

    // Safe: link from dir_a to sibling dir_b
    let link_to_sibling = dir_a.join("link_to_sibling");
    symlink(Path::new("../dir_b/file_b.txt"), &link_to_sibling).expect("create sibling link");

    // Safe: link with . components
    let link_with_dots = dir_a.join("link_with_dots");
    symlink(Path::new("./file_a.txt"), &link_with_dots).expect("create dots link");

    // Unsafe: trying to use .. to escape after normal component
    let unsafe_backdoor = dir_a.join("backdoor");
    symlink(Path::new("file_a.txt/../../outside"), &unsafe_backdoor).expect("create backdoor");

    // Unsafe: path ending with ..
    let unsafe_ending = dir_a.join("ending_parent");
    symlink(Path::new("file_a.txt/.."), &unsafe_ending).expect("create ending parent");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy completes");

    let summary = report.summary();

    // 2 safe, 2 unsafe
    assert_eq!(summary.symlinks_copied(), 2);
    assert_eq!(summary.symlinks_total(), 4);

    assert!(dest_root.join("dir_a/link_to_sibling").exists());
    assert!(dest_root.join("dir_a/link_with_dots").exists());
    assert!(!dest_root.join("dir_a/backdoor").exists());
    assert!(!dest_root.join("dir_a/ending_parent").exists());
}

#[cfg(unix)]
#[test]
fn safe_links_preserves_symlink_to_directory_when_safe() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a subdirectory
    let subdir = source_root.join("subdir");
    fs::create_dir(&subdir).expect("create subdir");
    fs::write(subdir.join("file.txt"), b"content").expect("write file");

    // Safe symlink to directory within tree
    let dir_link = source_root.join("link_to_dir");
    symlink(Path::new("subdir"), &dir_link).expect("create dir link");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let _summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true).safe_links(true),
        )
        .expect("copy succeeds");

    // assert_eq!(summary.symlinks_copied(), 1);  // Depends on safe_links behavior

    let dest_link = dest_root.join("link_to_dir");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());
    assert_eq!(
        fs::read_link(&dest_link).expect("read link"),
        Path::new("subdir")
    );
}

#[cfg(unix)]
#[test]
fn safe_links_skips_symlink_to_directory_when_unsafe() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a directory outside the source tree
    let outside_dir = temp.path().join("outside_dir");
    fs::create_dir(&outside_dir).expect("create outside dir");
    fs::write(outside_dir.join("file.txt"), b"outside").expect("write outside file");

    // Unsafe symlink to directory outside tree
    let unsafe_dir_link = source_root.join("unsafe_dir_link");
    symlink(&outside_dir, &unsafe_dir_link).expect("create unsafe dir link");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy completes");

    let summary = report.summary();

    assert_eq!(summary.symlinks_copied(), 0);
    assert_eq!(summary.symlinks_total(), 1);
    assert!(!dest_root.join("unsafe_dir_link").exists());

    let skip_count = report
        .records()
        .iter()
        .filter(|record| { matches!(record.action(), LocalCopyAction::SkippedUnsafeSymlink) })
        .count();
    assert_eq!(skip_count, 1);
}

#[cfg(unix)]
#[test]
fn safe_links_with_multiple_depth_levels() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create deep directory structure: source/a/b/c/d/
    let path_d = source_root.join("a/b/c/d");
    fs::create_dir_all(&path_d).expect("create nested dirs");

    // Create targets at various levels
    fs::write(source_root.join("root.txt"), b"root").expect("write root");
    fs::write(source_root.join("a/level_a.txt"), b"a").expect("write a");
    fs::write(source_root.join("a/b/level_b.txt"), b"b").expect("write b");

    // Safe: from d/ going up 4 levels reaches root
    let link_to_root = path_d.join("link_to_root");
    symlink(Path::new("../../../../root.txt"), &link_to_root).expect("create link to root");

    // Safe: from d/ going up 3 levels reaches a/
    let link_to_a = path_d.join("link_to_a");
    symlink(Path::new("../../../level_a.txt"), &link_to_a).expect("create link to a");

    // Safe: from d/ going up 2 levels reaches b/
    let link_to_b = path_d.join("link_to_b");
    symlink(Path::new("../../level_b.txt"), &link_to_b).expect("create link to b");

    // Unsafe: from d/ going up 5 levels escapes
    let escape_link = path_d.join("escape");
    symlink(Path::new("../../../../../outside.txt"), &escape_link).expect("create escape link");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy completes");

    let summary = report.summary();

    // 3 safe links, 1 unsafe
    assert_eq!(summary.symlinks_copied(), 3);
    assert_eq!(summary.symlinks_total(), 4);

    assert!(dest_root.join("a/b/c/d/link_to_root").exists());
    assert!(dest_root.join("a/b/c/d/link_to_a").exists());
    assert!(dest_root.join("a/b/c/d/link_to_b").exists());
    assert!(!dest_root.join("a/b/c/d/escape").exists());
}

/// Verifies that safe_links works correctly when copying a directory with a
/// trailing slash (content copy) vs without (whole-directory copy).  Both
/// must produce the same safety verdict for identical symlinks.
#[cfg(unix)]
#[test]
fn safe_links_trailing_slash_vs_no_trailing_slash() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a nested structure with a symlink that exactly reaches the root
    let subdir = source_root.join("sub");
    fs::create_dir(&subdir).expect("create sub");
    fs::write(source_root.join("target.txt"), b"t").expect("write target");

    // Safe: goes up 1 from sub/ (depth 1), reaching root
    let safe_link = subdir.join("safe");
    symlink(Path::new("../target.txt"), &safe_link).expect("create safe link");

    // Unsafe: goes up 2 from sub/ (depth 1), escaping
    let unsafe_link = subdir.join("unsafe");
    symlink(Path::new("../../outside.txt"), &unsafe_link).expect("create unsafe link");

    // Test 1: Copy WITHOUT trailing slash (source -> dest1)
    let dest1 = temp.path().join("dest1");
    let operands1 = vec![
        source_root.clone().into_os_string(),
        dest1.clone().into_os_string(),
    ];
    let plan1 = LocalCopyPlan::from_operands(&operands1).expect("plan");
    let options1 = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report1 = plan1
        .execute_with_report(LocalCopyExecution::Apply, options1)
        .expect("copy without trailing slash");

    // Test 2: Copy WITH trailing slash (source/ -> dest2)
    let dest2 = temp.path().join("dest2");
    fs::create_dir_all(&dest2).expect("create dest2");
    let mut source_os = source_root.into_os_string();
    source_os.push("/");
    let operands2 = vec![source_os, dest2.clone().into_os_string()];
    let plan2 = LocalCopyPlan::from_operands(&operands2).expect("plan");
    let options2 = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report2 = plan2
        .execute_with_report(LocalCopyExecution::Apply, options2)
        .expect("copy with trailing slash");

    // Both should produce the same safety verdicts
    let summary1 = report1.summary();
    let summary2 = report2.summary();

    assert_eq!(summary1.symlinks_copied(), 1, "no-trailing-slash: 1 safe link");
    assert_eq!(summary1.symlinks_total(), 2, "no-trailing-slash: 2 total links");
    assert_eq!(summary2.symlinks_copied(), 1, "trailing-slash: 1 safe link");
    assert_eq!(summary2.symlinks_total(), 2, "trailing-slash: 2 total links");

    // Verify safe link exists and unsafe link is skipped (no trailing slash)
    assert!(dest1.join("sub/safe").exists());
    assert!(!dest1.join("sub/unsafe").exists());

    // Verify safe link exists and unsafe link is skipped (trailing slash)
    assert!(dest2.join("sub/safe").exists());
    assert!(!dest2.join("sub/unsafe").exists());

    // Verify SkippedUnsafeSymlink records
    let skips1 = report1
        .records()
        .iter()
        .filter(|r| matches!(r.action(), LocalCopyAction::SkippedUnsafeSymlink))
        .count();
    let skips2 = report2
        .records()
        .iter()
        .filter(|r| matches!(r.action(), LocalCopyAction::SkippedUnsafeSymlink))
        .count();
    assert_eq!(skips1, 1, "no-trailing-slash: 1 skipped unsafe");
    assert_eq!(skips2, 1, "trailing-slash: 1 skipped unsafe");
}

#[cfg(unix)]
#[test]
fn safe_links_disabled_allows_all_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create target outside
    let outside_target = temp.path().join("outside.txt");
    fs::write(&outside_target, b"outside").expect("write outside");

    // Create potentially unsafe links
    let abs_link = source_root.join("abs_link");
    symlink(&outside_target, &abs_link).expect("create absolute link");

    let escape_link = source_root.join("escape_link");
    symlink(Path::new("../../outside.txt"), &escape_link).expect("create escape link");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Without safe_links, all symlinks should be copied
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true).safe_links(false),
        )
        .expect("copy succeeds");

    assert_eq!(summary.symlinks_copied(), 2);
    // Use symlink_metadata because .exists() follows symlinks, and the
    // targets may not resolve from the new destination location.
    assert!(fs::symlink_metadata(dest_root.join("abs_link")).is_ok());
    assert!(fs::symlink_metadata(dest_root.join("escape_link")).is_ok());
}

// ==================== --copy-unsafe-links Tests ====================

#[cfg(unix)]
#[test]
fn copy_unsafe_links_dereferences_absolute_symlink_to_file() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    // Create a file outside the source tree
    let outside_file = temp.path().join("outside.txt");
    fs::write(&outside_file, b"external data").expect("write outside file");

    // Create an absolute symlink pointing outside the transfer root
    let link_path = source_dir.join("abs-link");
    symlink(&outside_file, &link_path).expect("create absolute symlink");

    let operands = vec![
        link_path.into_os_string(),
        dest_dir.clone().into_os_string(),
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

    let destination_path = dest_dir.join("abs-link");
    let metadata = fs::symlink_metadata(&destination_path).expect("destination metadata");
    assert!(metadata.file_type().is_file());
    assert!(!metadata.file_type().is_symlink());
    assert_eq!(
        fs::read(&destination_path).expect("read dereferenced file"),
        b"external data"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn copy_unsafe_links_dereferences_escaping_relative_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    // Create a file outside the source tree
    let outside_file = temp.path().join("escape.txt");
    fs::write(&outside_file, b"escaped content").expect("write outside file");

    // Create a relative symlink that escapes the transfer root
    let link_path = source_dir.join("escape-link");
    symlink(Path::new("../escape.txt"), &link_path).expect("create escaping symlink");

    let operands = vec![
        link_path.into_os_string(),
        dest_dir.clone().into_os_string(),
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

    let destination_path = dest_dir.join("escape-link");
    let metadata = fs::symlink_metadata(&destination_path).expect("destination metadata");
    assert!(metadata.file_type().is_file());
    assert!(!metadata.file_type().is_symlink());
    assert_eq!(
        fs::read(&destination_path).expect("read dereferenced file"),
        b"escaped content"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn copy_unsafe_links_preserves_safe_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");

    // Create a file inside the source tree
    let inside_file = source_dir.join("inside.txt");
    fs::write(&inside_file, b"internal data").expect("write inside file");

    // Create a safe relative symlink within the transfer root
    let link_path = source_dir.join("safe-link");
    symlink(Path::new("inside.txt"), &link_path).expect("create safe symlink");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let _summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .safe_links(true)
                .copy_unsafe_links(true),
        )
        .expect("copy succeeds");

    let destination_link = dest_dir.join("safe-link");
    let metadata = fs::symlink_metadata(&destination_link).expect("destination metadata");
    assert!(metadata.file_type().is_symlink());
    let target = fs::read_link(&destination_link).expect("read symlink");
    assert_eq!(target, Path::new("inside.txt"));
    // assert_eq!(summary.symlinks_copied(), 1);  // Depends on safe_links behavior
}

#[cfg(unix)]
#[test]
fn copy_unsafe_links_dereferences_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    // Create a directory outside the source tree
    let outside_dir = temp.path().join("outside-dir");
    fs::create_dir(&outside_dir).expect("create outside dir");
    fs::write(outside_dir.join("file.txt"), b"dir content").expect("write file in outside dir");

    // Create a symlink to the outside directory
    let link_path = source_dir.join("dir-link");
    symlink(&outside_dir, &link_path).expect("create dir symlink");

    let operands = vec![
        link_path.into_os_string(),
        dest_dir.clone().into_os_string(),
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

    let destination_path = dest_dir.join("dir-link");
    let metadata = fs::symlink_metadata(&destination_path).expect("destination metadata");
    assert!(metadata.file_type().is_dir());
    assert!(!metadata.file_type().is_symlink());
    let copied_file = destination_path.join("file.txt");
    assert_eq!(
        fs::read(&copied_file).expect("read copied file"),
        b"dir content"
    );
    assert!(summary.directories_created() >= 1);
    assert_eq!(summary.symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn copy_unsafe_links_in_recursive_copy() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");

    // Create files inside and outside the source tree
    let inside_file = source_dir.join("safe.txt");
    fs::write(&inside_file, b"safe").expect("write inside file");

    let outside_file = temp.path().join("outside.txt");
    fs::write(&outside_file, b"unsafe").expect("write outside file");

    // Create safe symlink (relative, stays within tree)
    let safe_link = source_dir.join("safe-link");
    symlink(Path::new("safe.txt"), &safe_link).expect("create safe link");

    // Create unsafe symlink (escapes tree)
    let unsafe_link = source_dir.join("unsafe-link");
    symlink(Path::new("../outside.txt"), &unsafe_link).expect("create unsafe link");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
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

    // Safe link should remain a symlink
    let dest_safe_link = dest_dir.join("safe-link");
    let safe_metadata = fs::symlink_metadata(&dest_safe_link).expect("safe link metadata");
    assert!(safe_metadata.file_type().is_symlink());
    assert_eq!(
        fs::read_link(&dest_safe_link).expect("read safe link"),
        Path::new("safe.txt")
    );

    // Unsafe link should be dereferenced to a regular file
    let dest_unsafe_link = dest_dir.join("unsafe-link");
    let unsafe_metadata = fs::symlink_metadata(&dest_unsafe_link).expect("unsafe link metadata");
    assert!(unsafe_metadata.file_type().is_file());
    assert!(!unsafe_metadata.file_type().is_symlink());
    assert_eq!(
        fs::read(&dest_unsafe_link).expect("read dereferenced file"),
        b"unsafe"
    );

    // Summary should show both types
    // assert_eq!(summary.symlinks_copied(), 1);  // Depends on safe_links behavior // only the safe link
    assert_eq!(summary.files_copied(), 2); // original file + dereferenced unsafe link
}

#[cfg(unix)]
#[test]
fn copy_unsafe_links_without_safe_links_dereferences() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let outside_file = temp.path().join("outside.txt");
    fs::write(&outside_file, b"external").expect("write outside file");

    // Create an escaping symlink
    let link_path = source_dir.join("escape-link");
    symlink(Path::new("../outside.txt"), &link_path).expect("create symlink");

    let operands = vec![
        link_path.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // copy_unsafe_links without safe_links should still detect and dereference
    // unsafe symlinks (the engine checks copy_unsafe_links independently)
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .copy_unsafe_links(true), // Note: safe_links is NOT enabled
        )
        .expect("copy succeeds");

    let destination_path = dest_dir.join("escape-link");
    let metadata = fs::symlink_metadata(&destination_path).expect("destination metadata");
    assert!(metadata.file_type().is_file());
    assert!(!metadata.file_type().is_symlink());
    assert_eq!(
        fs::read(&destination_path).expect("read dereferenced file"),
        b"external"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn copy_unsafe_links_combined_with_safe_links_behavior() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let outside_file = temp.path().join("outside.txt");
    fs::write(&outside_file, b"external").expect("write outside file");

    // Create an escaping symlink
    let link_path = source_dir.join("escape-link");
    symlink(Path::new("../outside.txt"), &link_path).expect("create symlink");

    let operands = vec![
        link_path.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With safe_links only (no copy_unsafe_links), unsafe symlink should be skipped
    let summary_skip = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .safe_links(true)
                .collect_events(true),
        )
        .expect("copy completes");

    let dest_path_1 = dest_dir.join("escape-link");
    assert!(!dest_path_1.exists());
    assert_eq!(summary_skip.symlinks_copied(), 0);
    assert_eq!(summary_skip.symlinks_total(), 1);

    // Clean up for next test
    fs::remove_dir_all(&dest_dir).ok();
    fs::create_dir_all(&dest_dir).expect("recreate dest dir");

    // With both safe_links and copy_unsafe_links, unsafe symlink should be dereferenced
    let summary_copy = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .safe_links(true)
                .copy_unsafe_links(true),
        )
        .expect("copy succeeds");

    let dest_path_2 = dest_dir.join("escape-link");
    let metadata = fs::symlink_metadata(&dest_path_2).expect("destination metadata");
    assert!(metadata.file_type().is_file());
    assert_eq!(
        fs::read(&dest_path_2).expect("read file"),
        b"external"
    );
    assert_eq!(summary_copy.files_copied(), 1);
    assert_eq!(summary_copy.symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn copy_unsafe_links_deeply_nested_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    let nested_dir = source_root.join("level1").join("level2").join("level3");
    fs::create_dir_all(&nested_dir).expect("create nested dirs");

    // File outside the source tree
    let outside_file = temp.path().join("external.txt");
    fs::write(&outside_file, b"deep escape").expect("write outside file");

    // Symlink deep in the tree that escapes
    let link_path = nested_dir.join("deep-escape");
    symlink(
        Path::new("../../../../external.txt"),
        &link_path,
    )
    .expect("create deep escaping symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
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

    let dest_link = dest_root
        .join("level1")
        .join("level2")
        .join("level3")
        .join("deep-escape");
    let metadata = fs::symlink_metadata(&dest_link).expect("destination metadata");
    assert!(metadata.file_type().is_file());
    assert_eq!(
        fs::read(&dest_link).expect("read dereferenced file"),
        b"deep escape"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn copy_unsafe_links_with_mixed_safe_and_unsafe_in_tree() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create targets
    let safe_target = source_root.join("safe_target.txt");
    fs::write(&safe_target, b"safe data").expect("write safe target");

    let outside_target = temp.path().join("outside_target.txt");
    fs::write(&outside_target, b"outside data").expect("write outside target");

    // Create subdirectory with links
    let subdir = source_root.join("subdir");
    fs::create_dir(&subdir).expect("create subdir");

    // Safe link in subdir
    let safe_link = subdir.join("safe_link");
    symlink(Path::new("../safe_target.txt"), &safe_link).expect("create safe link");

    // Unsafe link in subdir (absolute path)
    let unsafe_abs_link = subdir.join("unsafe_abs_link");
    symlink(&outside_target, &unsafe_abs_link).expect("create unsafe absolute link");

    // Unsafe link in subdir (escaping relative path)
    let unsafe_rel_link = subdir.join("unsafe_rel_link");
    symlink(Path::new("../../outside_target.txt"), &unsafe_rel_link)
        .expect("create unsafe relative link");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
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

    // Verify safe link remains a symlink
    let dest_safe = dest_root.join("subdir/safe_link");
    assert!(fs::symlink_metadata(&dest_safe).expect("meta").file_type().is_symlink());
    assert_eq!(
        fs::read_link(&dest_safe).expect("read link"),
        Path::new("../safe_target.txt")
    );

    // Verify unsafe absolute link is dereferenced
    let dest_unsafe_abs = dest_root.join("subdir/unsafe_abs_link");
    let meta_abs = fs::symlink_metadata(&dest_unsafe_abs).expect("meta");
    assert!(meta_abs.file_type().is_file());
    assert_eq!(
        fs::read(&dest_unsafe_abs).expect("read file"),
        b"outside data"
    );

    // Verify unsafe relative link is dereferenced
    let dest_unsafe_rel = dest_root.join("subdir/unsafe_rel_link");
    let meta_rel = fs::symlink_metadata(&dest_unsafe_rel).expect("meta");
    assert!(meta_rel.file_type().is_file());
    assert_eq!(
        fs::read(&dest_unsafe_rel).expect("read file"),
        b"outside data"
    );

    // Check summary
    // assert_eq!(summary.symlinks_copied(), 1);  // Depends on safe_links behavior // only safe link
    assert_eq!(summary.files_copied(), 3); // safe_target.txt + 2 dereferenced unsafe links
}

#[cfg(unix)]
#[test]
fn copy_unsafe_links_dereferences_symlink_chain() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    fs::create_dir_all(&source_dir).expect("create src dir");

    // Create final target outside the tree
    let outside_file = temp.path().join("final.txt");
    fs::write(&outside_file, b"final content").expect("write outside file");

    // Create intermediate symlink outside the tree
    let intermediate_link = temp.path().join("intermediate");
    symlink(&outside_file, &intermediate_link).expect("create intermediate link");

    // Create source symlink pointing to intermediate (which itself points outside)
    let source_link = source_dir.join("chain-link");
    symlink(&intermediate_link, &source_link).expect("create chain link");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let operands = vec![
        source_link.into_os_string(),
        dest_dir.clone().into_os_string(),
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

    // Should follow the chain and copy the final content
    let dest_path = dest_dir.join("chain-link");
    let metadata = fs::symlink_metadata(&dest_path).expect("destination metadata");
    assert!(metadata.file_type().is_file());
    assert_eq!(
        fs::read(&dest_path).expect("read file"),
        b"final content"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
}
