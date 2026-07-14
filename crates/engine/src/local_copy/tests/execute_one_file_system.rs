// Tests for --one-file-system flag functionality

#[test]
fn one_file_system_traverses_same_filesystem_directories() {
    // Verify that directories on the same filesystem are fully traversed
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");

    // Create a tree with multiple nested directories
    let dir1 = source_root.join("dir1");
    let dir2 = source_root.join("dir2");
    let nested = dir1.join("nested").join("deep");

    fs::create_dir_all(&nested).expect("create nested");
    fs::create_dir_all(&dir2).expect("create dir2");

    fs::write(dir1.join("file1.txt"), b"content1").expect("write file1");
    fs::write(nested.join("file2.txt"), b"content2").expect("write file2");
    fs::write(dir2.join("file3.txt"), b"content3").expect("write file3");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // All files should be in device 1 (same filesystem)
    let summary = with_device_id_override(
        |_path, _metadata| Some(1),
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().one_file_system(true),
            )
        },
    )
    .expect("copy succeeds");

    // All files should be copied since they're on the same filesystem
    assert_eq!(summary.files_copied(), 3);
    assert!(dest_root.join("source").join("dir1").join("file1.txt").exists());
    assert!(dest_root.join("source").join("dir1").join("nested").join("deep").join("file2.txt").exists());
    assert!(dest_root.join("source").join("dir2").join("file3.txt").exists());
}

#[test]
fn one_file_system_skips_mount_point_directories() {
    // Verify that mount points to other filesystems are not crossed
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let same_fs_dir = source_root.join("same_fs");
    let mount_point = source_root.join("mount_point");
    let inside_mount = mount_point.join("subdir");

    fs::create_dir_all(&same_fs_dir).expect("create same_fs");
    fs::create_dir_all(&inside_mount).expect("create inside_mount");

    fs::write(same_fs_dir.join("local.txt"), b"same fs").expect("write local");
    fs::write(mount_point.join("mount_root.txt"), b"other fs").expect("write mount_root");
    fs::write(inside_mount.join("nested.txt"), b"other fs nested").expect("write nested");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = with_device_id_override(
        |path, _metadata| {
            // Simulate mount_point being on device 2, everything else on device 1
            if path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("mount_point")) {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .one_file_system(true)
                    .collect_events(true),
            )
        },
    )
    .expect("copy executes");

    // Only the file from same filesystem should be copied
    assert!(dest_root.join("source").join("same_fs").join("local.txt").exists());
    assert!(!dest_root.join("source").join("mount_point").exists());
    assert!(!dest_root.join("source").join("mount_point").join("mount_root.txt").exists());

    // Verify that mount point was recorded as skipped
    let records = report.records();
    let skipped_mount = records.iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedMountPoint
            && record.relative_path().to_string_lossy().contains("mount_point")
    });
    assert!(skipped_mount, "mount point should be recorded as skipped");
}

#[test]
fn one_file_system_transfers_files_on_same_filesystem() {
    // Verify that files on the same filesystem are transferred even in deep trees
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");

    // Create multiple levels with files at each level
    let level1 = source_root.join("level1");
    let level2 = level1.join("level2");
    let level3 = level2.join("level3");

    fs::create_dir_all(&level3).expect("create levels");

    fs::write(source_root.join("root.txt"), b"root").expect("write root");
    fs::write(level1.join("l1.txt"), b"level1").expect("write l1");
    fs::write(level2.join("l2.txt"), b"level2").expect("write l2");
    fs::write(level3.join("l3.txt"), b"level3").expect("write l3");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = with_device_id_override(
        |_path, _metadata| Some(1), // All on same device
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().one_file_system(true),
            )
        },
    )
    .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 4);
    assert_eq!(fs::read(dest_root.join("source").join("root.txt")).expect("read root"), b"root");
    assert_eq!(fs::read(dest_root.join("source").join("level1").join("l1.txt")).expect("read l1"), b"level1");
    assert_eq!(fs::read(dest_root.join("source").join("level1").join("level2").join("l2.txt")).expect("read l2"), b"level2");
    assert_eq!(fs::read(dest_root.join("source").join("level1").join("level2").join("level3").join("l3.txt")).expect("read l3"), b"level3");
}

#[test]
fn one_file_system_respects_flag_during_directory_walking() {
    // Verify that the flag is properly respected during directory tree traversal
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");

    // Create a complex tree with multiple potential mount points
    let dir_a = source_root.join("dir_a");
    let dir_b = source_root.join("dir_b");
    let mount1 = dir_a.join("mount1");
    let mount2 = dir_b.join("mount2");
    let regular_a = dir_a.join("regular");
    let regular_b = dir_b.join("regular");

    fs::create_dir_all(&mount1).expect("create mount1");
    fs::create_dir_all(&mount2).expect("create mount2");
    fs::create_dir_all(&regular_a).expect("create regular_a");
    fs::create_dir_all(&regular_b).expect("create regular_b");

    fs::write(dir_a.join("file_a.txt"), b"a").expect("write a");
    fs::write(mount1.join("mount1_file.txt"), b"m1").expect("write m1");
    fs::write(regular_a.join("reg_a.txt"), b"reg_a").expect("write reg_a");
    fs::write(dir_b.join("file_b.txt"), b"b").expect("write b");
    fs::write(mount2.join("mount2_file.txt"), b"m2").expect("write m2");
    fs::write(regular_b.join("reg_b.txt"), b"reg_b").expect("write reg_b");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = with_device_id_override(
        |path, _metadata| {
            // mount1 is device 2, mount2 is device 3, everything else is device 1
            if path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("mount1")) {
                Some(2)
            } else if path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("mount2")) {
                Some(3)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .one_file_system(true)
                    .collect_events(true),
            )
        },
    )
    .expect("copy executes");

    // Files on device 1 should be copied
    assert!(dest_root.join("source").join("dir_a").join("file_a.txt").exists());
    assert!(dest_root.join("source").join("dir_a").join("regular").join("reg_a.txt").exists());
    assert!(dest_root.join("source").join("dir_b").join("file_b.txt").exists());
    assert!(dest_root.join("source").join("dir_b").join("regular").join("reg_b.txt").exists());

    // Files on other devices should be skipped
    assert!(!dest_root.join("source").join("dir_a").join("mount1").join("mount1_file.txt").exists());
    assert!(!dest_root.join("source").join("dir_b").join("mount2").join("mount2_file.txt").exists());

    // Verify both mount points were skipped
    let records = report.records();
    let mount_skips: Vec<_> = records
        .iter()
        .filter(|r| r.action() == &LocalCopyAction::SkippedMountPoint)
        .collect();

    assert_eq!(mount_skips.len(), 2, "should have skipped exactly 2 mount points");
}

#[test]
fn one_file_system_disabled_crosses_filesystem_boundaries() {
    // Verify that without the flag, all directories are traversed
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let same_fs = source_root.join("same_fs");
    let other_fs = source_root.join("other_fs");

    fs::create_dir_all(&same_fs).expect("create same_fs");
    fs::create_dir_all(&other_fs).expect("create other_fs");

    fs::write(same_fs.join("file1.txt"), b"same").expect("write file1");
    fs::write(other_fs.join("file2.txt"), b"other").expect("write file2");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = with_device_id_override(
        |path, _metadata| {
            // other_fs is on device 2, everything else on device 1
            if path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("other_fs")) {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().one_file_system(false), // Disabled
            )
        },
    )
    .expect("copy succeeds");

    // Both files should be copied when flag is disabled
    assert_eq!(summary.files_copied(), 2);
    assert!(dest_root.join("source").join("same_fs").join("file1.txt").exists());
    assert!(dest_root.join("source").join("other_fs").join("file2.txt").exists());
}

#[test]
fn one_file_system_handles_multiple_mount_points_in_single_directory() {
    // Test case with multiple mount points as siblings
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");

    let local1 = source_root.join("local1");
    let mount1 = source_root.join("mount1");
    let local2 = source_root.join("local2");
    let mount2 = source_root.join("mount2");
    let local3 = source_root.join("local3");

    fs::create_dir_all(&local1).expect("create local1");
    fs::create_dir_all(&mount1).expect("create mount1");
    fs::create_dir_all(&local2).expect("create local2");
    fs::create_dir_all(&mount2).expect("create mount2");
    fs::create_dir_all(&local3).expect("create local3");

    fs::write(local1.join("f1.txt"), b"1").expect("write f1");
    fs::write(mount1.join("m1.txt"), b"m1").expect("write m1");
    fs::write(local2.join("f2.txt"), b"2").expect("write f2");
    fs::write(mount2.join("m2.txt"), b"m2").expect("write m2");
    fs::write(local3.join("f3.txt"), b"3").expect("write f3");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = with_device_id_override(
        |path, _metadata| {
            let path_str = path.to_string_lossy();
            if path_str.contains("mount1") {
                Some(2)
            } else if path_str.contains("mount2") {
                Some(3)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .one_file_system(true)
                    .collect_events(true),
            )
        },
    )
    .expect("copy executes");

    // Local files should be copied
    assert!(dest_root.join("source").join("local1").join("f1.txt").exists());
    assert!(dest_root.join("source").join("local2").join("f2.txt").exists());
    assert!(dest_root.join("source").join("local3").join("f3.txt").exists());

    // Mount point files should not be copied
    assert!(!dest_root.join("source").join("mount1").join("m1.txt").exists());
    assert!(!dest_root.join("source").join("mount2").join("m2.txt").exists());

    // Verify skip events
    let records = report.records();
    let skip_count = records
        .iter()
        .filter(|r| r.action() == &LocalCopyAction::SkippedMountPoint)
        .count();
    assert_eq!(skip_count, 2);
}

#[test]
fn one_file_system_with_empty_directories_on_same_fs() {
    // Test that empty directories on same filesystem are created
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");

    let empty1 = source_root.join("empty1");
    let empty2 = source_root.join("subdir").join("empty2");
    let with_file = source_root.join("with_file");

    fs::create_dir_all(&empty1).expect("create empty1");
    fs::create_dir_all(&empty2).expect("create empty2");
    fs::create_dir_all(&with_file).expect("create with_file");
    fs::write(with_file.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = with_device_id_override(
        |_path, _metadata| Some(1),
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().one_file_system(true),
            )
        },
    )
    .expect("copy succeeds");

    assert!(dest_root.join("source").join("empty1").is_dir());
    assert!(dest_root.join("source").join("subdir").join("empty2").is_dir());
    assert!(dest_root.join("source").join("with_file").join("file.txt").exists());
    assert!(summary.directories_created() >= 3);
}

#[test]
fn one_file_system_dry_run_reports_would_skip_mount() {
    // Verify dry run mode properly reports mount point skipping
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let same = source_root.join("same");
    let mount = source_root.join("mount");

    fs::create_dir_all(&same).expect("create same");
    fs::create_dir_all(&mount).expect("create mount");
    fs::write(same.join("local.txt"), b"local").expect("write local");
    fs::write(mount.join("other.txt"), b"other").expect("write other");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = with_device_id_override(
        |path, _metadata| {
            if path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("mount")) {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::DryRun,
                LocalCopyOptions::default()
                    .one_file_system(true)
                    .collect_events(true),
            )
        },
    )
    .expect("dry run succeeds");

    // Nothing should actually be copied in dry run
    assert!(!dest_root.exists());

    // But the report should show what would happen
    let records = report.records();
    let would_skip = records.iter().any(|r| {
        r.action() == &LocalCopyAction::SkippedMountPoint
            && r.relative_path().to_string_lossy().contains("mount")
    });
    assert!(would_skip, "dry run should report mount point would be skipped");
}

#[test]
fn one_file_system_with_filters_skips_mount_first() {
    // Verify that mount point checking happens before filter evaluation
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let mount = source_root.join("mount");

    fs::create_dir_all(&mount).expect("create mount");
    fs::write(mount.join("test.txt"), b"content").expect("write file");
    fs::write(source_root.join("local.txt"), b"local").expect("write local");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Include filter that would match files in mount
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::include("*.txt")),
    ])
    .expect("compile program");

    let report = with_device_id_override(
        |path, _metadata| {
            if path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("mount")) {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .one_file_system(true)
                    .with_filter_program(Some(program))
                    .collect_events(true),
            )
        },
    )
    .expect("copy executes");

    // Mount should be skipped before filter is evaluated
    assert!(!dest_root.join("source").join("mount").exists());
    assert!(dest_root.join("source").join("local.txt").exists());

    let records = report.records();
    let mount_skipped = records.iter().any(|r| {
        r.action() == &LocalCopyAction::SkippedMountPoint
            && r.relative_path().to_string_lossy().contains("mount")
    });
    assert!(mount_skipped);
}

#[cfg(unix)]
#[test]
fn one_file_system_with_symlinks_follows_on_same_fs() {
    // Verify that symlinks on the same filesystem are followed appropriately
    use std::os::unix::fs as unix_fs;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_dir = source_root.join("target");
    let link_dir = source_root.join("linkdir");

    fs::create_dir_all(&target_dir).expect("create target");
    fs::create_dir_all(&link_dir).expect("create linkdir");
    fs::write(target_dir.join("file.txt"), b"target").expect("write file");

    let link_path = link_dir.join("link");
    unix_fs::symlink(target_dir.join("file.txt"), &link_path).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = with_device_id_override(
        |_path, _metadata| Some(1),
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .one_file_system(true)
                    .links(true),
            )
        },
    )
    .expect("copy succeeds");

    // Both the file and symlink should be copied
    assert!(dest_root.join("source").join("target").join("file.txt").exists());
    assert!(dest_root.join("source").join("linkdir").join("link").exists());
    assert!(summary.files_copied() >= 1);
}


#[test]
fn double_one_file_system_skips_root_source_that_is_mount_point() {
    // With -xx, if the source directory itself is a mount point (its device
    // differs from its parent's device), the entire source should be skipped.
    let temp = tempdir().expect("tempdir");
    let parent_dir = temp.path().join("parent");
    let source_root = parent_dir.join("mounted_src");

    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");
    let subdir = source_root.join("sub");
    fs::create_dir_all(&subdir).expect("create subdir");
    fs::write(subdir.join("nested.txt"), b"nested").expect("write nested");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Parent directory is on device 1, source directory is on device 2 (mount point)
    let report = with_device_id_override(
        |path, _metadata| {
            if path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("mounted_src"))
                || path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("sub"))
            {
                Some(2) // mount point
            } else {
                Some(1) // parent
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .with_one_file_system_level(2)
                    .collect_events(true),
            )
        },
    )
    .expect("copy executes");

    // With -xx, the source itself is a mount point, so nothing should be copied
    assert!(!dest_root.join("file.txt").exists());
    assert!(!dest_root.join("sub").exists());

    // Verify mount point skip was recorded
    let records = report.records();
    let skipped = records
        .iter()
        .any(|r| r.action() == &LocalCopyAction::SkippedMountPoint);
    assert!(skipped, "root mount point should be recorded as skipped with -xx");
}

#[test]
fn double_one_file_system_allows_source_on_same_device_as_parent() {
    // With -xx, if the source directory is on the same device as its parent,
    // it should proceed normally (not skipped at root level).
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let subdir = source_root.join("sub");

    fs::create_dir_all(&subdir).expect("create subdir");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");
    fs::write(subdir.join("nested.txt"), b"nested").expect("write nested");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // All on device 1 (same filesystem as parent)
    let summary = with_device_id_override(
        |_path, _metadata| Some(1),
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().with_one_file_system_level(2),
            )
        },
    )
    .expect("copy succeeds");

    // All files should be copied since source is on same device as parent
    assert_eq!(summary.files_copied(), 2);
    assert!(dest_root.join("source").join("file.txt").exists());
    assert!(dest_root.join("source").join("sub").join("nested.txt").exists());
}

#[test]
fn single_x_does_not_skip_root_mount_point() {
    // With single -x (level 1), a root-level source that is a mount point
    // should still be processed -- only subdirectory crossings are skipped.
    let temp = tempdir().expect("tempdir");
    let parent_dir = temp.path().join("parent");
    let source_root = parent_dir.join("mounted_src");

    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Parent directory on device 1, source on device 2 (mount point),
    // but with single -x, root mount points are NOT skipped.
    let summary = with_device_id_override(
        |path, _metadata| {
            if path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("mounted_src")) {
                Some(2) // mount point
            } else {
                Some(1) // parent
            }
        },
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().one_file_system(true), // level 1
            )
        },
    )
    .expect("copy succeeds");

    // With single -x, the root mount point source should NOT be skipped
    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join("mounted_src").join("file.txt").exists());
}

#[test]
fn double_one_file_system_still_skips_subdirectory_mount_points() {
    // With -xx, mount points within the source tree should still be skipped
    // (same as single -x behavior for subdirectories).
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let same_fs = source_root.join("same_fs");
    let mount_point = source_root.join("mount_point");

    fs::create_dir_all(&same_fs).expect("create same_fs");
    fs::create_dir_all(&mount_point).expect("create mount_point");

    fs::write(same_fs.join("local.txt"), b"same fs").expect("write local");
    fs::write(mount_point.join("other.txt"), b"other fs").expect("write other");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = with_device_id_override(
        |path, _metadata| {
            if path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("mount_point")) {
                Some(2) // different device
            } else {
                Some(1) // same device
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .with_one_file_system_level(2)
                    .collect_events(true),
            )
        },
    )
    .expect("copy executes");

    // File on same filesystem should be copied
    assert!(dest_root.join("source").join("same_fs").join("local.txt").exists());
    // Mount point subdirectory should be skipped
    assert!(!dest_root.join("source").join("mount_point").join("other.txt").exists());

    // Verify skip was recorded
    let records = report.records();
    let mount_skipped = records
        .iter()
        .any(|r| r.action() == &LocalCopyAction::SkippedMountPoint);
    assert!(mount_skipped, "subdirectory mount point should still be skipped with -xx");
}

#[test]
fn level_zero_does_not_skip_any_mount_points() {
    // With level 0 (disabled), nothing should be skipped
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let mount = source_root.join("mount");

    fs::create_dir_all(&mount).expect("create mount");
    fs::write(source_root.join("local.txt"), b"local").expect("write local");
    fs::write(mount.join("other.txt"), b"other").expect("write other");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = with_device_id_override(
        |path, _metadata| {
            if path.components().any(|c| c.as_os_str() == std::ffi::OsStr::new("mount")) {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().with_one_file_system_level(0),
            )
        },
    )
    .expect("copy succeeds");

    // Everything should be copied
    assert_eq!(summary.files_copied(), 2);
    assert!(dest_root.join("source").join("local.txt").exists());
    assert!(dest_root.join("source").join("mount").join("other.txt").exists());
}

/// Verifies that pruning a cross-device child directory emits the upstream
/// `--info=MOUNT` notice through the diagnostic event queue.
///
/// Mirrors `flist.c:1319-1323` in upstream rsync 3.4.1:
/// ```c
/// if (INFO_GTE(MOUNT, 1)) {
///     rprintf(FINFO, "[%s] skipping mount-point dir %s\n",
///             who_am_i(), thisname);
/// }
/// ```
/// The role prefix is added downstream by the renderer; the payload sent into
/// the diagnostic queue carries the bare `skipping mount-point dir <path>`
/// wording.
#[test]
fn one_file_system_emits_info_mount_notice() {
    use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, init};

    // Enable MOUNT at level 1 (upstream's --info=MOUNT threshold).
    let mut cfg = VerbosityConfig::from_verbose_level(0);
    cfg.info.mount = 1;
    init(cfg);
    let _ = drain_events();

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let mount_point = source_root.join("mount_point");
    let inside_mount = mount_point.join("subdir");

    fs::create_dir_all(&inside_mount).expect("create inside_mount");
    fs::write(source_root.join("local.txt"), b"same fs").expect("write local");
    fs::write(mount_point.join("mount_root.txt"), b"other fs").expect("write mount_root");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.clone().into_os_string(),
        dest_root.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    with_device_id_override(
        |path, _metadata| {
            if path
                .components()
                .any(|c| c.as_os_str() == std::ffi::OsStr::new("mount_point"))
            {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().one_file_system(true),
            )
        },
    )
    .expect("copy succeeds");

    let messages: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Info {
                flag: InfoFlag::Mount,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();

    let expected = format!(
        "skipping mount-point dir {}",
        source_root.join("mount_point").display()
    );

    assert!(
        messages.iter().any(|m| m == &expected),
        "expected upstream-format MOUNT,1 notice {expected:?}; got {messages:?}"
    );
}

/// Verifies that the default verbosity configuration (no `--info=MOUNT`)
/// suppresses the notice, matching upstream's `INFO_GTE(MOUNT, 1)` gate
/// (flist.c:1319). MOUNT is not in `info_verbosity[0]`, so it stays silent
/// unless explicitly enabled.
#[test]
fn one_file_system_default_verbosity_suppresses_info_mount_notice() {
    use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, init};

    init(VerbosityConfig::from_verbose_level(0));
    let _ = drain_events();

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let mount_point = source_root.join("mount_point");

    fs::create_dir_all(&mount_point).expect("create mount");
    fs::write(source_root.join("local.txt"), b"same fs").expect("write local");
    fs::write(mount_point.join("mount_root.txt"), b"other fs").expect("write mount_root");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    with_device_id_override(
        |path, _metadata| {
            if path
                .components()
                .any(|c| c.as_os_str() == std::ffi::OsStr::new("mount_point"))
            {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().one_file_system(true),
            )
        },
    )
    .expect("copy succeeds");

    let mount_msgs: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Info {
                flag: InfoFlag::Mount,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();

    assert!(
        mount_msgs.is_empty(),
        "default verbosity should suppress MOUNT notices; got {mount_msgs:?}"
    );
}

/// Mount-point data-loss protection on the delete pass: under
/// `--one-file-system --delete`, a mount point nested inside an otherwise
/// fully-extraneous destination directory must be preserved, and the doomed
/// directory must be pinned (not removed) because it still contains the mount.
///
/// This is the residual the plan-level exclusion alone could not reach: the
/// doomed directory is absent from the source, so the copy walk never recurses
/// into it and never sees the nested mount as a direct child. The wholesale
/// `remove_dir_all` emitter would recurse across the mount boundary and destroy
/// the mounted filesystem. Routing every `--one-file-system` run through the
/// boundary-aware leaf executor closes it: the executor checks the device
/// boundary at each recursion level, so the mount (and its contents) survive
/// while same-device extraneous entries are still removed.
///
/// upstream: generator.c:delete_in_dir() (FLAG_MOUNT_DIR skip) and
/// delete.c:89-97 delete_dir_contents() (a mount point pins the parent).
///
/// Unix-only: the device-boundary skip is gated on `st_dev`, which has no
/// Windows equivalent, so on Windows the boundary check is a no-op and the
/// nested foreign-device entry would not be preserved.
#[cfg(unix)]
#[test]
fn one_file_system_delete_preserves_mount_nested_in_doomed_dir() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

    // Pre-seed the destination tree (dest_root/source) with entries absent from
    // the source so the delete pass treats them as extraneous.
    let dest_root = temp.path().join("dest");
    let dest_tree = dest_root.join("source");
    let doomed = dest_tree.join("doomed");
    let mnt = doomed.join("mnt");
    let purge = dest_tree.join("purge");
    fs::create_dir_all(&mnt).expect("create doomed/mnt");
    fs::create_dir_all(&purge).expect("create purge");
    fs::write(doomed.join("regular.txt"), b"same-device").expect("write regular");
    fs::write(mnt.join("data.txt"), b"mounted data").expect("write mounted data");
    fs::write(purge.join("gone.txt"), b"same-device").expect("write gone");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Report the nested mount (any path component named "mnt") on a foreign
    // device; everything else is on the transfer-root device.
    let summary = with_device_id_override(
        |path, _metadata| {
            if path
                .components()
                .any(|c| c.as_os_str() == std::ffi::OsStr::new("mnt"))
            {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .one_file_system(true)
                    .delete(true),
            )
        },
    )
    .expect("copy succeeds");

    // The source file was copied and kept.
    assert!(dest_tree.join("keep.txt").exists(), "kept source file survives");

    // The nested mount and its contents are preserved: had `remove_dir_all` run
    // across the boundary, `mnt/data.txt` would be gone.
    assert!(mnt.exists(), "nested mount point must be preserved");
    assert!(
        mnt.join("data.txt").exists(),
        "mounted filesystem contents must be intact (no cross-boundary removal)",
    );

    // The doomed directory is pinned by the mount it still contains.
    assert!(
        doomed.exists(),
        "a directory holding a preserved mount must not be removed",
    );

    // Same-device extraneous entries are still removed normally.
    assert!(
        !doomed.join("regular.txt").exists(),
        "a same-device leaf inside the doomed dir must still be deleted",
    );
    assert!(
        !purge.exists(),
        "a fully same-device extraneous directory must be removed",
    );

    // Deletions were counted (regular.txt, purge/gone.txt, purge/); the mount
    // and its contents were not.
    assert!(summary.items_deleted() >= 2, "same-device extras were deleted");
}
