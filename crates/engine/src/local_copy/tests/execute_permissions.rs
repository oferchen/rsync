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

    fs::write(&source, b"new content").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set source mode 0000");
    let source_time = FileTime::from_unix_time(1_700_000_200, 0);
    set_file_times(&source, source_time, source_time).expect("set source times");

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

    fs::write(&source, b"new data").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set source mode 0000");

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

    let backup_path = temp.path().join("dest.txt~");
    assert!(backup_path.exists());
    assert_eq!(fs::read(&backup_path).expect("read backup"), b"old data");

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
    assert!(
        !destination.exists(),
        "destination should not be created in dry run"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn mode_0000_with_sparse_file() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

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

    let options = LocalCopyOptions::default().permissions(true).sparse(true);

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

    let dest_files = [
        (
            dest_root.join("source").join("source/root.txt"),
            b"root" as &[u8],
        ),
        (
            dest_root.join("source").join("source/level1/one.txt"),
            b"level1",
        ),
        (
            dest_root
                .join("source")
                .join("source/level1/level2/two.txt"),
            b"level2",
        ),
        (
            dest_root
                .join("source")
                .join("source/level1/level2/level3/three.txt"),
            b"level3",
        ),
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

    let target = source_dir.join("target.txt");
    fs::write(&target, b"target content").expect("write target");
    fs::set_permissions(&target, PermissionsExt::from_mode(0o000)).expect("set target mode 0000");

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

    let dest_target = dest_dir.join("source").join("source/target.txt");
    let metadata = fs::metadata(&dest_target).expect("target metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);

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

    fs::write(&source_file, content).expect("write source");
    fs::set_permissions(&source_file, PermissionsExt::from_mode(0o000)).expect("set source mode");
    set_file_times(&source_file, timestamp, timestamp).expect("set source times");

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

    fs::write(&source, content).expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set source mode");

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

    fs::write(&source, content).expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set source mode");
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, timestamp, timestamp).expect("set source times");

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

    fs::write(&source, b"new").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set source mode");
    let new_time = FileTime::from_unix_time(1_700_000_200, 0);
    set_file_times(&source, new_time, new_time).expect("set source times");

    fs::write(&destination, b"old").expect("write dest");
    fs::set_permissions(&destination, PermissionsExt::from_mode(0o644)).expect("set dest mode");
    let old_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&destination, old_time, old_time).expect("set dest times");

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

    fs::set_permissions(&parent, PermissionsExt::from_mode(0o2770)).expect("chmod parent");

    // Probe whether the filesystem supports directory setgid inheritance.
    let probe = parent.join("probe");
    fs::create_dir(&probe).expect("create probe");
    let probe_mode = fs::metadata(&probe)
        .expect("probe meta")
        .permissions()
        .mode();
    if probe_mode & 0o2000 == 0 {
        // Filesystem does not propagate setgid - skip gracefully.
        return;
    }
    fs::remove_dir(&probe).expect("remove probe");

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

/// A fresh directory must land upstream's `dest_mode()` result, not the
/// `create_dir` umask default.
///
/// Source mode 0o500 discriminates under every sane umask: `dest_mode()` gives
/// `0o500 & (~CHMOD_BITS | dflt_perms)` = 0o500 (dflt_perms always carries the
/// owner bits), while the pre-fix behaviour left the mkdir default
/// (`0o777 & ~umask`, owner-writable). The file inside proves the transfer
/// ordering: the strict 0o500 lands only after the contents are written, the
/// during-transfer raise (generator.c:1904-1912) makes 0o700 of it, and
/// touch_up_dirs restores 0o500 last because the owner-write bit is absent
/// (generator.c:2594 fix_dir_perms).
// upstream: generator.c:1856 - file->mode = dest_mode(...) runs for
// directories even when !preserve_perms; rsync.c:481-485 masks the fresh arm.
#[cfg(unix)]
#[test]
fn fresh_directory_without_perms_lands_dest_mode_not_umask_default() {
    use std::os::unix::fs::PermissionsExt;

    if ::metadata::am_root() {
        return; // root skips the raise/restore dance; the cells differ there.
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let sub = source.join("sub");
    fs::create_dir_all(&sub).expect("create source tree");
    fs::write(sub.join("file.txt"), b"payload").expect("write file");
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o500)).expect("chmod source sub");

    // Non-vacuity guard: the fixture only discriminates while the mkdir
    // default differs from the expected dest_mode() result.
    let probe = temp.path().join("probe");
    fs::create_dir(&probe).expect("create probe");
    let probe_mode = fs::metadata(&probe)
        .expect("probe meta")
        .permissions()
        .mode()
        & 0o777;
    assert_ne!(probe_mode, 0o500, "umask makes this fixture vacuous");

    let dest = temp.path().join("dst");
    let mut source_operand = source.clone().into_os_string();
    source_operand.push("/");
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().recursive(true),
        )
        .expect("copy succeeds");
    assert_eq!(summary.files_copied(), 1, "the file inside must transfer");

    let sub_mode = fs::metadata(dest.join("sub"))
        .expect("dest sub meta")
        .permissions()
        .mode()
        & 0o7777;
    // Unlock before asserting so tempdir cleanup works even on failure paths.
    let _ = fs::set_permissions(dest.join("sub"), fs::Permissions::from_mode(0o755));
    let _ = fs::set_permissions(&sub, fs::Permissions::from_mode(0o755));
    assert_eq!(
        sub_mode, 0o500,
        "a fresh dir takes dest_mode(source), not the mkdir umask default"
    );
}

/// A pre-existing read-only destination directory must be raised to owner-rwx
/// for the transfer and restored afterwards - without `--perms`.
///
/// `dest_mode()`'s exists arm keeps the directory's own 0o555
/// (rsync.c:470-480); the raise (generator.c:1904-1912) is what lets the file
/// land, and fix_dir_perms restores 0o555 because owner-write is absent
/// (generator.c:2594). Measured against rsync 3.5.0: rc 0, file transferred,
/// directory back at 0o555; the pre-fix behaviour failed the write with
/// EACCES (exit 23) and transferred nothing.
#[cfg(unix)]
#[test]
fn preexisting_readonly_subdir_without_perms_transfers_and_is_restored() {
    use std::os::unix::fs::PermissionsExt;

    if ::metadata::am_root() {
        return; // root writes into 0o555 regardless; the fixture cannot fail.
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    fs::create_dir_all(source.join("sub")).expect("create source tree");
    fs::write(source.join("sub/file.txt"), b"payload").expect("write file");

    let dest = temp.path().join("dst");
    fs::create_dir_all(dest.join("sub")).expect("create dest tree");
    fs::set_permissions(dest.join("sub"), fs::Permissions::from_mode(0o555))
        .expect("chmod dest sub");

    let mut source_operand = source.clone().into_os_string();
    source_operand.push("/");
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().recursive(true),
    );

    let sub_mode = fs::metadata(dest.join("sub"))
        .expect("dest sub meta")
        .permissions()
        .mode()
        & 0o7777;
    let payload = fs::read(dest.join("sub/file.txt"));
    let _ = fs::set_permissions(dest.join("sub"), fs::Permissions::from_mode(0o755));

    let summary = result.expect("copy into a read-only pre-existing dir succeeds");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(payload.expect("file landed").as_slice(), b"payload");
    assert_eq!(
        sub_mode, 0o555,
        "the restrictive pre-existing mode must be restored after the transfer"
    );
}

/// The transfer ROOT variant: a pre-existing 0o555 destination root must not
/// fail the copy. Measured against rsync 3.5.0 (`-r`, fresh files): rc 0, both
/// files land, the root is restored to 0o555.
#[cfg(unix)]
#[test]
fn preexisting_readonly_dest_root_without_perms_transfers_and_is_restored() {
    use std::os::unix::fs::PermissionsExt;

    if ::metadata::am_root() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    fs::create_dir_all(&source).expect("create source");
    fs::write(source.join("f1"), b"one").expect("write f1");
    fs::write(source.join("f2"), b"two").expect("write f2");

    let dest = temp.path().join("dst");
    fs::create_dir_all(&dest).expect("create dest");
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).expect("chmod dest root");

    let mut source_operand = source.clone().into_os_string();
    source_operand.push("/");
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().recursive(true),
    );

    let root_mode = fs::metadata(&dest).expect("dest meta").permissions().mode() & 0o7777;
    let f1 = fs::read(dest.join("f1"));
    let f2 = fs::read(dest.join("f2"));
    let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o755));

    let summary = result.expect("copy into a read-only pre-existing root succeeds");
    assert_eq!(summary.files_copied(), 2);
    assert!(f1.is_ok() && f2.is_ok(), "both files must land");
    assert_eq!(
        root_mode, 0o555,
        "the restrictive pre-existing root mode must be restored last"
    );
}

/// Upstream's raise residue: an owner-writable-but-not-executable directory
/// keeps the transient owner-rwx bits on disk, because fix_dir_perms restores
/// only when `!(file->mode & S_IWUSR)` (generator.c:2594) and 0o644 has the
/// owner-write bit. Measured against rsync 3.5.0: source dir 0o644 under
/// `-rp` lands 0o744 on a fresh destination (umask 022 / 077 / 002 alike).
/// The pre-fix behaviour restored the strict 0o644 and diverged.
// upstream: generator.c:1904-1912 raise + generator.c:2594 restore condition.
#[cfg(unix)]
#[test]
fn owner_writable_nonexec_dir_keeps_the_raise_residue_under_perms() {
    use std::os::unix::fs::PermissionsExt;

    if ::metadata::am_root() {
        return; // the raise is gated on !am_root; root lands 0o644 verbatim.
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    // The 0o644 dir stays EMPTY: without owner-x even its own entries could
    // not be stat'd on the source side, which would add an unrelated failure.
    fs::create_dir_all(source.join("sub")).expect("create source tree");
    fs::set_permissions(source.join("sub"), fs::Permissions::from_mode(0o644))
        .expect("chmod source sub");

    let dest = temp.path().join("dst");
    let mut source_operand = source.clone().into_os_string();
    source_operand.push("/");
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .recursive(true)
            .permissions(true),
    )
    .expect("copy succeeds");

    let sub_mode = fs::metadata(dest.join("sub"))
        .expect("dest sub meta")
        .permissions()
        .mode()
        & 0o7777;
    let _ = fs::set_permissions(source.join("sub"), fs::Permissions::from_mode(0o755));
    assert_eq!(
        sub_mode, 0o744,
        "0o644 | S_IRWXU raise sticks: owner-write blocks the touch-up restore"
    );
}
