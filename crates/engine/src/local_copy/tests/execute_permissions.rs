
#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_file_basic_copy() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let payload = b"test content with no permissions";
    fs::write(&source, payload).expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set mode 0000");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().permissions(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o000);
    assert_eq!(dest_metadata.len(), payload.len() as u64);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_preserves_across_multiple_files() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");

    // Create multiple files with mode 0000
    let file1 = source_dir.join("file1.txt");
    let file2 = source_dir.join("file2.txt");
    let file3 = source_dir.join("file3.txt");

    for (file, content) in [
        (&file1, b"content1" as &[u8]),
        (&file2, b"content2"),
        (&file3, b"content3"),
    ] {
        fs::write(file, content).expect("write file");
        fs::set_permissions(file, PermissionsExt::from_mode(0o000)).expect("set mode 0000");
    }

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .recursive(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);

    // Verify all destination files have mode 0000
    for file_name in ["file1.txt", "file2.txt", "file3.txt"] {
        let dest_file = dest_dir.join("source").join(file_name);
        let metadata = fs::metadata(&dest_file).expect("dest file metadata");
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o000,
            "file {file_name} should have mode 0000"
        );
    }
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_with_inplace_update() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Create source with mode 0000
    fs::write(&source, b"new content").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set source mode 0000");
    let source_time = FileTime::from_unix_time(1_700_000_200, 0);
    set_file_times(&source, source_time, source_time).expect("set source times");

    // Create existing destination with different permissions
    fs::write(&destination, b"old").expect("write dest");
    fs::set_permissions(&destination, PermissionsExt::from_mode(0o644)).expect("set dest mode");
    let dest_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&destination, dest_time, dest_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .times(true)
        .inplace(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"new content");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_with_backup() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Create source with mode 0000
    fs::write(&source, b"new data").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set source mode 0000");

    // Create existing destination
    fs::write(&destination, b"old data").expect("write dest");
    fs::set_permissions(&destination, PermissionsExt::from_mode(0o644)).expect("set dest mode");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .backup(true)
        .with_backup_suffix(Some("~"));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify backup was created
    let backup_path = temp.path().join("dest.txt~");
    assert!(backup_path.exists());
    assert_eq!(fs::read(&backup_path).expect("read backup"), b"old data");

    // Verify destination has mode 0000
    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_dry_run() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"dry run test").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set mode 0000");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().permissions(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(!destination.exists(), "destination should not be created in dry run");
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_with_sparse_file() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Create a file with holes (sparse file)
    let mut file = fs::File::create(&source).expect("create source");
    use std::io::{Seek, SeekFrom, Write};
    file.write_all(b"start").expect("write start");
    file.seek(SeekFrom::Current(1024 * 1024)).expect("seek");
    file.write_all(b"end").expect("write end");
    drop(file);

    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set mode 0000");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .sparse(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_nested_directory_structure() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    // Create nested structure with mode 0000 files at various levels
    let level1 = source_root.join("level1");
    let level2 = level1.join("level2");
    let level3 = level2.join("level3");
    fs::create_dir_all(&level3).expect("create nested dirs");

    let files = [
        (source_root.join("root.txt"), b"root" as &[u8]),
        (level1.join("one.txt"), b"level1"),
        (level2.join("two.txt"), b"level2"),
        (level3.join("three.txt"), b"level3"),
    ];

    for (path, content) in &files {
        fs::write(path, content).expect("write file");
        fs::set_permissions(path, PermissionsExt::from_mode(0o000)).expect("set mode 0000");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .recursive(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 4);

    // Verify all files have mode 0000
    let dest_files = [
        (dest_root.join("source/root.txt"), b"root" as &[u8]),
        (dest_root.join("source/level1/one.txt"), b"level1"),
        (dest_root.join("source/level1/level2/two.txt"), b"level2"),
        (dest_root.join("source/level1/level2/level3/three.txt"), b"level3"),
    ];

    for (path, expected_content) in &dest_files {
        assert!(path.exists(), "file should exist: {path:?}");
        let metadata = fs::metadata(path).expect("file metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o000);
        assert_eq!(fs::read(path).expect("read file"), *expected_content);
    }
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_with_symlink_preservation() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");

    // Create a file with mode 0000
    let target = source_dir.join("target.txt");
    fs::write(&target, b"target content").expect("write target");
    fs::set_permissions(&target, PermissionsExt::from_mode(0o000)).expect("set target mode 0000");

    // Create a symlink to the mode 0000 file
    let link = source_dir.join("link.txt");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .links(true)
        .recursive(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.symlinks_copied(), 1);

    // Verify target has mode 0000
    let dest_target = dest_dir.join("source/target.txt");
    let metadata = fs::metadata(&dest_target).expect("target metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);

    // Verify symlink was created
    let dest_link = dest_dir.join("source/link.txt");
    assert!(dest_link.exists());
    let link_metadata = fs::symlink_metadata(&dest_link).expect("link metadata");
    assert!(link_metadata.file_type().is_symlink());
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_compare_dest() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");

    let source_file = source_dir.join("file.txt");
    let compare_file = compare_dir.join("file.txt");

    let content = b"matched content";
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);

    // Create source with mode 0000
    fs::write(&source_file, content).expect("write source");
    fs::set_permissions(&source_file, PermissionsExt::from_mode(0o000)).expect("set source mode");
    set_file_times(&source_file, timestamp, timestamp).expect("set source times");

    // Create compare with mode 0000 and matching content/time
    fs::write(&compare_file, content).expect("write compare");
    fs::set_permissions(&compare_file, PermissionsExt::from_mode(0o000)).expect("set compare mode");
    set_file_times(&compare_file, timestamp, timestamp).expect("set compare times");

    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .times(true)
        .extend_reference_directories([super::ReferenceDirectory::new(
            super::ReferenceDirectoryKind::Compare,
            &compare_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // File should be skipped due to match
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
    assert!(!dest_file.exists());
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_size_only_comparison() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"size match";

    // Create source with mode 0000
    fs::write(&source, content).expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set source mode");

    // Create destination with same size but different mode
    fs::write(&destination, content).expect("write dest");
    fs::set_permissions(&destination, PermissionsExt::from_mode(0o644)).expect("set dest mode");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .size_only(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // With size_only, file should be skipped despite different permissions
    assert_eq!(summary.files_copied(), 0);

    // Destination should keep its original mode
    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_checksum_comparison() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"checksum test";

    // Create source with mode 0000
    fs::write(&source, content).expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set source mode");
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, timestamp, timestamp).expect("set source times");

    // Create destination with same content and mode but different time
    fs::write(&destination, content).expect("write dest");
    fs::set_permissions(&destination, PermissionsExt::from_mode(0o000)).expect("set dest mode");
    let old_timestamp = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_times(&destination, old_timestamp, old_timestamp).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .times(true)
        .checksum(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // With checksum, file should be skipped despite different mtime
    assert_eq!(summary.files_copied(), 0);

    // Verify destination still has old timestamp
    let metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_mtime, old_timestamp);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_file_is_readable_by_owner() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let test_file = temp.path().join("test.txt");
    let content = b"owner can read despite mode 0000";

    fs::write(&test_file, content).expect("write file");
    fs::set_permissions(&test_file, PermissionsExt::from_mode(0o000)).expect("set mode 0000");

    // Verify mode is 0000
    let metadata = fs::metadata(&test_file).expect("metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);

    // As the owner, we should still be able to read the file
    let read_content = fs::read(&test_file).expect("owner can read mode 0000 file");
    assert_eq!(read_content, content);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_preserve_in_existing_file_update() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Create source with mode 0000 and newer time
    fs::write(&source, b"new").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set source mode");
    let new_time = FileTime::from_unix_time(1_700_000_200, 0);
    set_file_times(&source, new_time, new_time).expect("set source times");

    // Create existing destination with different mode and older time
    fs::write(&destination, b"old").expect("write dest");
    fs::set_permissions(&destination, PermissionsExt::from_mode(0o644)).expect("set dest mode");
    let old_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&destination, old_time, old_time).expect("set dest times");

    // Verify initial state
    let initial_metadata = fs::metadata(&destination).expect("initial dest metadata");
    assert_eq!(initial_metadata.permissions().mode() & 0o777, 0o644);

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .times(true)
        .update(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Destination should now have mode 0000
    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);
    assert_eq!(fs::read(&destination).expect("read dest"), b"new");
}
