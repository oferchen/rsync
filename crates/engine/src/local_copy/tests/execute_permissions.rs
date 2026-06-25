
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
        (dest_root.join("source").join("source/root.txt"), b"root" as &[u8]),
        (dest_root.join("source").join("source/level1/one.txt"), b"level1"),
        (dest_root.join("source").join("source/level1/level2/two.txt"), b"level2"),
        (dest_root.join("source").join("source/level1/level2/level3/three.txt"), b"level3"),
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
    let dest_target = dest_dir.join("source").join("source/target.txt");
    let metadata = fs::metadata(&dest_target).expect("target metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);

    // Verify symlink was created
    let dest_link = dest_dir.join("source").join("source/link.txt");
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

/// Verifies that directory setgid inheritance is preserved during copy.
///
/// Mirrors upstream's `testsuite/dir-sgid.test`: when creating destination
/// directories inside a setgid parent, the OS-level setgid inheritance from
/// `mkdir()` must survive the transfer. Without `--perms`, no chmod is
/// applied to newly-created directories, so the inherited setgid bit stays.
///
/// This test only runs on Linux because macOS/BSD do not propagate the
/// setgid bit on `mkdir()` in the same way. If the filesystem does not
/// support directory setgid inheritance (e.g., some tmpfs configurations),
/// the test is skipped gracefully.
// upstream: rsync.c:510-516 - `inherit = !preserve_perms && FLAG_DIR_CREATED`
// preserves the on-disk S_ISGID when no --perms is specified.
#[cfg(target_os = "linux")]
#[test]
fn dir_setgid_inheritance_preserved_without_perms() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let parent = temp.path().join("sgid_parent");
    fs::create_dir(&parent).expect("create parent");

    // Set the setgid bit on the parent directory.
    fs::set_permissions(&parent, PermissionsExt::from_mode(0o2770)).expect("chmod parent");

    // Probe whether the filesystem supports directory setgid inheritance.
    let probe = parent.join("probe");
    fs::create_dir(&probe).expect("create probe");
    let probe_mode = fs::metadata(&probe).expect("probe meta").permissions().mode();
    if probe_mode & 0o2000 == 0 {
        // Filesystem does not propagate setgid - skip gracefully.
        return;
    }
    fs::remove_dir(&probe).expect("remove probe");

    // Build a source tree: a directory containing a file and a subdirectory.
    let source = temp.path().join("src");
    let source_subdir = source.join("subdir");
    fs::create_dir_all(&source_subdir).expect("create source tree");
    fs::write(source.join("file.txt"), b"hello").expect("write file");
    fs::write(source_subdir.join("nested.txt"), b"world").expect("write nested");

    // Destination is inside the setgid parent - a non-existent directory
    // with a trailing slash so oc-rsync creates it as a wrapper.
    let dest = parent.join("dest");
    let mut dest_operand = dest.clone().into_os_string();
    dest_operand.push("/");

    let operands = vec![source.into_os_string(), dest_operand];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // No --perms: permissions are NOT explicitly applied to new directories.
    let options = LocalCopyOptions::default().recursive(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // The wrapper directory `dest/` should inherit setgid from the parent.
    let dest_mode = fs::metadata(&dest).expect("dest meta").permissions().mode();
    assert_ne!(
        dest_mode & 0o2000,
        0,
        "wrapper directory should inherit setgid from parent (mode {dest_mode:#o})"
    );

    // The transferred source directory inside `dest/` should also have setgid
    // (inherited from `dest/` which inherited from the sgid parent).
    let copied_src = dest.join("src");
    let copied_mode = fs::metadata(&copied_src)
        .expect("copied dir meta")
        .permissions()
        .mode();
    assert_ne!(
        copied_mode & 0o2000,
        0,
        "copied directory should inherit setgid (mode {copied_mode:#o})"
    );

    // The nested subdirectory should also have setgid.
    let nested = copied_src.join("subdir");
    let nested_mode = fs::metadata(&nested)
        .expect("nested dir meta")
        .permissions()
        .mode();
    assert_ne!(
        nested_mode & 0o2000,
        0,
        "nested directory should inherit setgid (mode {nested_mode:#o})"
    );
}

/// Verifies that setgid is NOT inherited when the parent lacks it.
///
/// The counterpart to `dir_setgid_inheritance_preserved_without_perms`:
/// when the destination parent does NOT have setgid, newly-created
/// directories should NOT have it either.
// upstream: testsuite/dir-sgid.test - "testit setgid-off 700 ..."
#[cfg(target_os = "linux")]
#[test]
fn dir_no_setgid_when_parent_lacks_it() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let parent = temp.path().join("no_sgid_parent");
    fs::create_dir(&parent).expect("create parent");

    // Parent does NOT have setgid - just normal 0770.
    fs::set_permissions(&parent, PermissionsExt::from_mode(0o0770)).expect("chmod parent");

    // Build a simple source tree.
    let source = temp.path().join("src");
    fs::create_dir_all(&source).expect("create source");
    fs::write(source.join("file.txt"), b"hello").expect("write file");

    let dest = parent.join("dest");
    let mut dest_operand = dest.clone().into_os_string();
    dest_operand.push("/");

    let operands = vec![source.into_os_string(), dest_operand];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().recursive(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Neither the wrapper nor the copied directory should have setgid.
    let dest_mode = fs::metadata(&dest).expect("dest meta").permissions().mode();
    assert_eq!(
        dest_mode & 0o2000,
        0,
        "wrapper directory should NOT have setgid (mode {dest_mode:#o})"
    );

    let copied_src = dest.join("src");
    let copied_mode = fs::metadata(&copied_src)
        .expect("copied dir meta")
        .permissions()
        .mode();
    assert_eq!(
        copied_mode & 0o2000,
        0,
        "copied directory should NOT have setgid (mode {copied_mode:#o})"
    );
}
