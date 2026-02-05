// Tests for --copy-links flag which dereferences symlinks and copies target content

#[cfg(unix)]
#[test]
fn copy_links_follows_file_symlink_and_copies_content() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    fs::write(&target, b"target content").expect("write target");

    let link = temp.path().join("link");
    symlink(&target, &link).expect("create symlink");

    let dest = temp.path().join("dest");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Destination should be a regular file, not a symlink
    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(metadata.file_type().is_file());
    assert!(!metadata.file_type().is_symlink());

    // Content should match target
    assert_eq!(fs::read(&dest).expect("read dest"), b"target content");

    // Should not count as a symlink copy
    assert_eq!(summary.symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn copy_links_follows_directory_symlink_recursively() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_dir = temp.path().join("target_dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::write(target_dir.join("file1.txt"), b"content1").expect("write file1");
    fs::write(target_dir.join("file2.txt"), b"content2").expect("write file2");

    let subdir = target_dir.join("subdir");
    fs::create_dir(&subdir).expect("create subdir");
    fs::write(subdir.join("file3.txt"), b"content3").expect("write file3");

    let link = temp.path().join("dir_link");
    symlink(&target_dir, &link).expect("create dir symlink");

    let dest = temp.path().join("dest");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .copy_links(true)
        .recursive(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Destination should be a regular directory, not a symlink
    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(metadata.file_type().is_dir());
    assert!(!metadata.file_type().is_symlink());

    // All files should be copied
    assert_eq!(fs::read(dest.join("file1.txt")).expect("read file1"), b"content1");
    assert_eq!(fs::read(dest.join("file2.txt")).expect("read file2"), b"content2");
    assert_eq!(
        fs::read(dest.join("subdir/file3.txt")).expect("read file3"),
        b"content3"
    );
}

#[cfg(unix)]
#[test]
fn copy_links_handles_dangling_symlink_with_error() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let nonexistent = temp.path().join("nonexistent");

    let link = temp.path().join("dangling_link");
    symlink(&nonexistent, &link).expect("create dangling symlink");

    let dest = temp.path().join("dest");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_links(true);
    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // Should fail because the symlink target doesn't exist
    assert!(result.is_err());

    // Destination should not be created
    assert!(!dest.exists());
}

#[cfg(unix)]
#[test]
fn copy_links_skips_dangling_symlink_in_directory_tree() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    // Create a good file
    fs::write(source_dir.join("good.txt"), b"good content").expect("write good");

    // Create a dangling symlink
    let nonexistent = temp.path().join("nonexistent");
    let dangling = source_dir.join("dangling");
    symlink(&nonexistent, &dangling).expect("create dangling");

    // Create another good file
    fs::write(source_dir.join("also_good.txt"), b"also good").expect("write also_good");

    let dest = temp.path().join("dest");

    let operands = vec![
        source_dir.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .copy_links(true)
        .recursive(true);
    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // The operation should fail when encountering the dangling symlink
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn copy_links_detects_direct_symlink_loop() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let link = temp.path().join("self_link");

    // Create a symlink that points to itself
    symlink(&link, &link).expect("create self-referencing symlink");

    let dest = temp.path().join("dest");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_links(true);
    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // Should fail because following the symlink creates a loop
    assert!(result.is_err());

    // Destination should not be created
    assert!(!dest.exists());
}

#[cfg(unix)]
#[test]
fn copy_links_detects_indirect_symlink_loop() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let link_a = temp.path().join("link_a");
    let link_b = temp.path().join("link_b");

    // Create a cycle: link_a -> link_b -> link_a
    symlink(&link_b, &link_a).expect("create link_a");
    symlink(&link_a, &link_b).expect("create link_b");

    let dest = temp.path().join("dest");

    let operands = vec![link_a.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_links(true);
    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // Should fail because following the symlink chain creates a loop
    assert!(result.is_err());

    // Destination should not be created
    assert!(!dest.exists());
}

#[cfg(unix)]
#[test]
fn copy_links_detects_directory_loop_via_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let parent = temp.path().join("parent");
    fs::create_dir(&parent).expect("create parent");

    let child = parent.join("child");
    fs::create_dir(&child).expect("create child");

    // Create a symlink inside child that points back to parent
    let loop_link = child.join("back_to_parent");
    symlink(&parent, &loop_link).expect("create loop symlink");

    let dest = temp.path().join("dest");

    let operands = vec![parent.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .copy_links(true)
        .recursive(true);
    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // When copy_links is enabled, the symlink should be followed, which creates
    // an infinite directory structure. The implementation should detect this.
    // The exact behavior depends on how the filesystem handles this, but it
    // should not hang or create an infinite directory tree.
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn copy_links_follows_chain_of_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");

    // Create the actual target
    let target = temp.path().join("target.txt");
    fs::write(&target, b"final content").expect("write target");

    // Create a chain: link1 -> link2 -> target
    let link2 = temp.path().join("link2");
    symlink(&target, &link2).expect("create link2");

    let link1 = temp.path().join("link1");
    symlink(&link2, &link1).expect("create link1");

    let dest = temp.path().join("dest");

    let operands = vec![link1.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_links(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Should follow the entire chain and copy the final target
    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(metadata.file_type().is_file());
    assert_eq!(fs::read(&dest).expect("read dest"), b"final content");
}

#[cfg(unix)]
#[test]
fn copy_links_works_with_relative_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let dir = temp.path().join("dir");
    fs::create_dir(&dir).expect("create dir");

    let target = dir.join("target.txt");
    fs::write(&target, b"relative target").expect("write target");

    // Create a relative symlink
    let link = dir.join("link");
    symlink(Path::new("target.txt"), &link).expect("create relative symlink");

    let dest = temp.path().join("dest");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_links(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Should resolve the relative symlink and copy the target content
    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(metadata.file_type().is_file());
    assert_eq!(fs::read(&dest).expect("read dest"), b"relative target");
}

#[cfg(unix)]
#[test]
fn copy_links_follows_absolute_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");

    let target = temp.path().join("target.txt");
    fs::write(&target, b"absolute target").expect("write target");

    // Create an absolute symlink
    let link = temp.path().join("link");
    symlink(&target, &link).expect("create absolute symlink");

    let dest = temp.path().join("dest");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().copy_links(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Should follow the absolute symlink and copy the target content
    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(metadata.file_type().is_file());
    assert_eq!(fs::read(&dest).expect("read dest"), b"absolute target");
}

#[cfg(unix)]
#[test]
fn copy_links_with_mixed_files_and_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    // Create a regular file
    fs::write(source_dir.join("regular.txt"), b"regular").expect("write regular");

    // Create a target and symlink to it
    let target = temp.path().join("target.txt");
    fs::write(&target, b"linked").expect("write target");
    let link = source_dir.join("link.txt");
    symlink(&target, &link).expect("create symlink");

    // Create a subdirectory
    let subdir = source_dir.join("subdir");
    fs::create_dir(&subdir).expect("create subdir");
    fs::write(subdir.join("nested.txt"), b"nested").expect("write nested");

    let dest = temp.path().join("dest");

    let operands = vec![
        source_dir.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .copy_links(true)
        .recursive(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Regular file should be copied normally
    let regular_meta = fs::symlink_metadata(dest.join("regular.txt")).expect("regular meta");
    assert!(regular_meta.file_type().is_file());
    assert_eq!(fs::read(dest.join("regular.txt")).expect("read"), b"regular");

    // Symlink should be followed and copied as a regular file
    let link_meta = fs::symlink_metadata(dest.join("link.txt")).expect("link meta");
    assert!(link_meta.file_type().is_file());
    assert!(!link_meta.file_type().is_symlink());
    assert_eq!(fs::read(dest.join("link.txt")).expect("read"), b"linked");

    // Nested file should be copied
    assert_eq!(
        fs::read(dest.join("subdir/nested.txt")).expect("read"),
        b"nested"
    );
}

#[cfg(unix)]
#[test]
fn copy_links_without_recursive_follows_single_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_dir = temp.path().join("target_dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::write(target_dir.join("file.txt"), b"content").expect("write file");

    let link = temp.path().join("dir_link");
    symlink(&target_dir, &link).expect("create dir symlink");

    let dest = temp.path().join("dest");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .copy_links(true)
        .recursive(false)
        .dirs(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Without recursive, should create the directory but not copy contents
    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(metadata.file_type().is_dir());
    assert!(!metadata.file_type().is_symlink());
}

#[cfg(unix)]
#[test]
fn copy_links_preserves_file_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let temp = tempdir().expect("tempdir");

    let target = temp.path().join("target.txt");
    fs::write(&target, b"content").expect("write target");

    // Set specific permissions on target
    let mut perms = fs::metadata(&target).expect("metadata").permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&target, perms).expect("set permissions");

    let link = temp.path().join("link");
    symlink(&target, &link).expect("create symlink");

    let dest = temp.path().join("dest");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .copy_links(true)
        .permissions(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Permissions should be copied from the target, not the symlink
    let dest_perms = fs::metadata(&dest).expect("dest metadata").permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o644);
}

#[cfg(unix)]
#[test]
fn copy_links_does_not_follow_symlinks_in_tree_when_disabled() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    let target = temp.path().join("target.txt");
    fs::write(&target, b"target").expect("write target");

    let link = source_dir.join("link");
    symlink(&target, &link).expect("create symlink");

    let dest = temp.path().join("dest");

    let operands = vec![
        source_dir.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Without copy_links, symlinks should be preserved
    let options = LocalCopyOptions::default()
        .copy_links(false)
        .links(true)
        .recursive(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Should be copied as a symlink
    let link_meta = fs::symlink_metadata(dest.join("link")).expect("link meta");
    assert!(link_meta.file_type().is_symlink());
    assert_eq!(summary.symlinks_copied(), 1);
}

#[cfg(unix)]
#[test]
fn copy_links_with_symlink_to_empty_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_dir = temp.path().join("empty_dir");
    fs::create_dir(&target_dir).expect("create empty dir");

    let link = temp.path().join("dir_link");
    symlink(&target_dir, &link).expect("create dir symlink");

    let dest = temp.path().join("dest");

    let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .copy_links(true)
        .recursive(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Should create an empty directory, not a symlink
    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(metadata.file_type().is_dir());
    assert!(!metadata.file_type().is_symlink());

    // Should be empty
    let entries: Vec<_> = fs::read_dir(&dest).expect("read dir").collect();
    assert_eq!(entries.len(), 0);
}
