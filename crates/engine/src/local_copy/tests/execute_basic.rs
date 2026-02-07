
#[test]
fn execute_with_remove_source_files_deletes_source() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"move me").expect("write source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().remove_source_files(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.sources_removed(), 1);
    assert!(!source.exists(), "source should be removed");
    assert_eq!(fs::read(destination).expect("read dest"), b"move me");
}

#[test]
fn execute_with_remove_source_files_preserves_unchanged_source() {
    use filetime::{FileTime, set_file_times};

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let payload = b"stable";
    fs::write(&source, payload).expect("write source");
    fs::write(&destination, payload).expect("write destination");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, timestamp, timestamp).expect("set source times");
    set_file_times(&destination, timestamp, timestamp).expect("set dest times");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
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

    assert_eq!(summary.sources_removed(), 0, "unchanged sources remain");
    assert!(source.exists(), "source should remain when unchanged");
    assert!(destination.exists(), "destination remains present");
    assert_eq!(summary.files_copied(), 0);
}

#[test]
fn execute_file_replaces_directory_when_force_enabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("target");

    fs::write(&source, b"replacement").expect("write source");
    fs::create_dir_all(&destination).expect("create conflicting directory");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().force_replacements(true),
        )
        .expect("forced replacement succeeds");

    assert!(destination.is_file(), "directory should be replaced by file");
    assert_eq!(fs::read(&destination).expect("read destination"), b"replacement");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_with_relative_preserves_parent_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(source_root.join("foo/bar")).expect("create source tree");
    fs::create_dir_all(&destination_root).expect("create destination root");
    let source_file = source_root.join("foo").join("bar").join("nested.txt");
    fs::write(&source_file, b"relative").expect("write source");

    let operand = source_root
        .join(".")
        .join("foo")
        .join("bar")
        .join("nested.txt");

    let operands = vec![
        operand.into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().relative_paths(true),
        )
        .expect("copy succeeds");

    let copied = destination_root.join("foo").join("bar").join("nested.txt");
    assert_eq!(fs::read(copied).expect("read copied"), b"relative");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_with_relative_requires_directory_destination() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(source_root.join("dir")).expect("create source tree");
    let source_file = source_root.join("dir").join("file.txt");
    fs::write(&source_file, b"dir").expect("write source");

    let destination = temp.path().join("dest.txt");
    fs::write(&destination, b"target").expect("write destination");

    let operand = source_root.join(".").join("dir").join("file.txt");

    let operands = vec![
        operand.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().relative_paths(true),
    );

    let error = result.expect_err("relative paths require directory destination");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::DestinationMustBeDirectory)
    ));
    assert_eq!(fs::read(&destination).expect("read destination"), b"target");
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn execute_copies_file_with_xattrs() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"attr").expect("write source");
    xattr::set(&source, "user.demo", b"value").expect("set xattr");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().xattrs(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let copied = xattr::get(&destination, "user.demo")
        .expect("read dest xattr")
        .expect("xattr present");
    assert_eq!(copied, b"value");
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn execute_respects_xattr_filter_rules() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"attr").expect("write source");
    xattr::set(&source, "user.keep", b"keep").expect("set keep");
    xattr::set(&source, "user.skip", b"skip").expect("set skip");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("user.skip").with_xattr_only(true)),
        FilterProgramEntry::Rule(FilterRule::include("user.keep").with_xattr_only(true)),
    ])
    .expect("compile program");

    let options = LocalCopyOptions::default()
        .xattrs(true)
        .with_filter_program(Some(program));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let kept = xattr::get(&destination, "user.keep")
        .expect("read keep")
        .expect("keep present");
    assert_eq!(kept, b"keep");
    let skipped = xattr::get(&destination, "user.skip")
        .expect("read skip")
        .is_none();
    assert!(skipped, "excluded xattr should be absent");
}

#[cfg(all(unix, feature = "acl", not(target_vendor = "apple")))]
#[test]
fn execute_copies_file_with_acls() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"acl").expect("write source");
    let acl_text = "user::rw-\ngroup::r--\nother::r--\n";
    set_acl_from_text(&source, acl_text, acl_sys::ACL_TYPE_ACCESS);

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().acls(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // On Linux and other non-Apple Unix, ACLs are actually copied and visible.
    let copied =
        acl_to_text(&destination, acl_sys::ACL_TYPE_ACCESS).expect("dest acl");
    assert!(copied.contains("user::rw-"));
}

#[cfg(all(unix, feature = "acl", target_vendor = "apple"))]
#[test]
fn execute_copies_file_with_acls_is_noop_on_apple() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"acl").expect("write source");

    // Even if we call the ACL helper, the active strategy on Apple is a stub.
    let acl_text = "user::rw-\ngroup::r--\nother::r--\n";
    set_acl_from_text(&source, acl_text, acl_sys::ACL_TYPE_ACCESS);

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().acls(true),
        )
        .expect("copy succeeds");

    // Data copy still happens.
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"acl");

    // ACLs are effectively a no-op on Apple: we must not panic, but
    // we also don't assert on actual ACL contents.
    let maybe_acl = acl_to_text(&destination, acl_sys::ACL_TYPE_ACCESS);
    // For the stub strategy, this should be None.
    assert!(maybe_acl.is_none());
}

#[test]
fn execute_copies_directory_tree() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"tree").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");
    assert_eq!(
        fs::read(dest_root.join("nested").join("file.txt")).expect("read"),
        b"tree"
    );
    assert_eq!(summary.files_copied(), 1);
    assert!(summary.directories_created() >= 1);
}

// ==================== Empty File Tests ====================

#[test]
fn execute_copies_empty_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"");
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

#[test]
fn execute_copies_empty_file_over_existing() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"existing content").expect("write existing dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"");
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

// ==================== Large File Tests ====================

#[test]
fn execute_copies_large_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.bin");
    let destination = temp.path().join("dest.bin");

    // Create a file larger than the typical copy buffer (128KB)
    let large_content = vec![0xABu8; 256 * 1024];
    fs::write(&source, &large_content).expect("write large source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 256 * 1024);
    assert_eq!(fs::read(&destination).expect("read dest"), large_content);
}

// ==================== Multiple File Tests ====================

#[test]
fn execute_copies_multiple_files_to_directory() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source dir");
    fs::write(source_root.join("file1.txt"), b"content1").expect("write file1");
    fs::write(source_root.join("file2.txt"), b"content2").expect("write file2");
    fs::write(source_root.join("file3.txt"), b"content3").expect("write file3");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(fs::read(dest_root.join("file1.txt")).expect("read"), b"content1");
    assert_eq!(fs::read(dest_root.join("file2.txt")).expect("read"), b"content2");
    assert_eq!(fs::read(dest_root.join("file3.txt")).expect("read"), b"content3");
}

// ==================== Dry Run Tests ====================

#[test]
fn execute_dry_run_does_not_create_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"dry run content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(!destination.exists(), "dry run should not create destination");
}

#[test]
fn execute_dry_run_does_not_modify_existing_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"original").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"original");
}

// ==================== Inplace Mode Tests ====================

#[test]
fn execute_with_inplace_updates_existing_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"updated content").expect("write source");
    fs::write(&destination, b"original").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"updated content");
}

// ==================== Partial Mode Tests ====================

#[test]
fn execute_with_partial_enabled_creates_partial_file_on_success() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"partial test").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().partial(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"partial test");
}

// ==================== Permissions Tests ====================

#[cfg(unix)]
#[test]
fn execute_preserves_permissions_when_enabled() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"perms").expect("write source");

    let mut perms = fs::metadata(&source).expect("source metadata").permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&source, perms).expect("set source perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_perms = fs::metadata(&destination).expect("dest metadata").permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o600);
}

// ==================== Times Preservation Tests ====================

#[test]
fn execute_preserves_modification_time_when_enabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"times").expect("write source");

    let past_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source, past_time).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );
    assert_eq!(dest_mtime, past_time);
}

// ==================== Whole File Mode Tests ====================

#[test]
fn execute_with_whole_file_always_copies() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"whole file mode content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write identical dest");

    let source_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&source).expect("source metadata"),
    );
    set_file_mtime(&destination, source_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(true).ignore_times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
}

// ==================== Recursive Directory Tests ====================

#[test]
fn execute_copies_deeply_nested_directory() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let deep_path = source_root.join("a").join("b").join("c").join("d");
    fs::create_dir_all(&deep_path).expect("create deep path");
    fs::write(deep_path.join("deep.txt"), b"deep content").expect("write deep file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_file = dest_root.join("a").join("b").join("c").join("d").join("deep.txt");
    assert_eq!(fs::read(&dest_file).expect("read deep file"), b"deep content");
}

// ==================== Error Handling Tests ====================

#[test]
fn execute_file_copy_to_directory_places_file_inside() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("target");

    fs::write(&source, b"content").expect("write source");
    fs::create_dir_all(&destination).expect("create directory at dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    // File is placed inside the destination directory
    assert_eq!(summary.files_copied(), 1);
    let target_file = destination.join("source.txt");
    assert!(target_file.exists());
    assert_eq!(fs::read(&target_file).expect("read"), b"content");
}

// ==================== Summary Statistics Tests ====================

#[test]
fn execute_summary_tracks_total_source_bytes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"exactly 26 bytes of data!!";
    fs::write(&source, content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.total_source_bytes(), 26);
}

#[test]
fn execute_summary_tracks_directories_created() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("dir1").join("subdir")).expect("create dirs");
    fs::create_dir_all(source_root.join("dir2")).expect("create dir2");
    fs::write(source_root.join("dir1").join("subdir").join("file.txt"), b"f").expect("write");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert!(summary.directories_created() >= 3);
    assert!(dest_root.join("dir1").join("subdir").exists());
    assert!(dest_root.join("dir2").exists());
}

// ==================== Single File Copy Tests ====================

#[test]
fn execute_basic_single_file_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("file.txt");
    let destination = temp.path().join("copied.txt");

    fs::write(&source, b"basic copy test").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"basic copy test");
}

#[test]
fn execute_overwrites_existing_file_with_different_content() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content here").expect("write source");
    fs::write(&destination, b"old").expect("write dest");

    // Sleep briefly to ensure different mtimes
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Touch source to make it newer
    fs::write(&source, b"new content here").expect("update source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"new content here");
}

#[test]
fn execute_copies_file_to_nonexistent_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"content");
}

#[test]
fn execute_creates_intermediate_directories_when_needed() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("nested").join("path").join("dest.txt");

    fs::write(&source, b"nested").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"nested");
}

// ==================== Directory Copy Tests ====================

#[test]
fn execute_copies_empty_directory() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert!(dest_root.exists());
    assert!(dest_root.is_dir());
    assert_eq!(summary.files_copied(), 0);
    assert!(summary.directories_created() >= 1);
}

#[test]
fn execute_copies_directory_with_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file1.txt"), b"content1").expect("write file1");
    fs::write(source_root.join("file2.txt"), b"content2").expect("write file2");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(fs::read(dest_root.join("file1.txt")).expect("read"), b"content1");
    assert_eq!(fs::read(dest_root.join("file2.txt")).expect("read"), b"content2");
}

#[test]
fn execute_copies_nested_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let subdir = source_root.join("subdir");
    fs::create_dir_all(&subdir).expect("create subdir");
    fs::write(source_root.join("root.txt"), b"root").expect("write root");
    fs::write(subdir.join("nested.txt"), b"nested").expect("write nested");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(fs::read(dest_root.join("root.txt")).expect("read"), b"root");
    assert_eq!(fs::read(dest_root.join("subdir").join("nested.txt")).expect("read"), b"nested");
}

// ==================== Timestamp Preservation Tests ====================

#[test]
fn execute_does_not_preserve_timestamps_by_default() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"times").expect("write source");

    let past_time = FileTime::from_unix_time(1_500_000_000, 0);
    set_file_mtime(&source, past_time).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );

    // The destination mtime should be recent, not the past_time
    assert_ne!(dest_mtime, past_time);
}

#[test]
fn execute_preserves_timestamps_across_multiple_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let file1 = source_root.join("file1.txt");
    let file2 = source_root.join("file2.txt");

    fs::write(&file1, b"content1").expect("write file1");
    fs::write(&file2, b"content2").expect("write file2");

    let time1 = FileTime::from_unix_time(1_600_000_000, 0);
    let time2 = FileTime::from_unix_time(1_650_000_000, 0);
    set_file_mtime(&file1, time1).expect("set file1 mtime");
    set_file_mtime(&file2, time2).expect("set file2 mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);

    let dest_file1 = dest_root.join("file1.txt");
    let dest_file2 = dest_root.join("file2.txt");

    let dest_mtime1 = FileTime::from_last_modification_time(
        &fs::metadata(&dest_file1).expect("file1 metadata"),
    );
    let dest_mtime2 = FileTime::from_last_modification_time(
        &fs::metadata(&dest_file2).expect("file2 metadata"),
    );

    assert_eq!(dest_mtime1, time1);
    assert_eq!(dest_mtime2, time2);
}

#[test]
fn execute_preserves_very_old_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"old").expect("write source");

    // Very old timestamp (1970-01-02)
    let very_old = FileTime::from_unix_time(86400, 0);
    set_file_mtime(&source, very_old).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );
    assert_eq!(dest_mtime, very_old);
}

// ==================== Permission Preservation Tests ====================

#[cfg(unix)]
#[test]
fn execute_does_not_preserve_permissions_by_default() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"perms").expect("write source");

    let mut perms = fs::metadata(&source).expect("source metadata").permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&source, perms).expect("set source perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_perms = fs::metadata(&destination).expect("dest metadata").permissions();
    // Default permissions should be different from 0o600
    assert_ne!(dest_perms.mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn execute_preserves_executable_bit() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("script.sh");
    let destination = temp.path().join("dest.sh");

    fs::write(&source, b"#!/bin/bash\necho test").expect("write source");

    let mut perms = fs::metadata(&source).expect("source metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&source, perms).expect("set source perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_perms = fs::metadata(&destination).expect("dest metadata").permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o755);
}

#[cfg(unix)]
#[test]
fn execute_preserves_read_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("readonly.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"readonly").expect("write source");

    let mut perms = fs::metadata(&source).expect("source metadata").permissions();
    perms.set_mode(0o444);
    fs::set_permissions(&source, perms).expect("set source perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_perms = fs::metadata(&destination).expect("dest metadata").permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o444);
}

#[cfg(unix)]
#[test]
fn execute_preserves_permissions_across_directory_tree() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let file1 = source_root.join("public.txt");
    let file2 = source_root.join("private.txt");

    fs::write(&file1, b"public").expect("write file1");
    fs::write(&file2, b"private").expect("write file2");

    fs::set_permissions(&file1, PermissionsExt::from_mode(0o644)).expect("set file1 perms");
    fs::set_permissions(&file2, PermissionsExt::from_mode(0o600)).expect("set file2 perms");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);

    let dest_file1 = dest_root.join("public.txt");
    let dest_file2 = dest_root.join("private.txt");

    assert_eq!(
        fs::metadata(&dest_file1).expect("file1 metadata").permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(
        fs::metadata(&dest_file2).expect("file2 metadata").permissions().mode() & 0o777,
        0o600
    );
}

// ==================== Combined Options Tests ====================

#[test]
fn execute_preserves_both_times_and_permissions() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"combined").expect("write source");

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&source).expect("source metadata").permissions();
        perms.set_mode(0o640);
        fs::set_permissions(&source, perms).expect("set source perms");
    }

    let past_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source, past_time).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true).permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_mtime, past_time);

    #[cfg(unix)]
    {
        assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o640);
    }
}

// ==================== File Size Tests ====================

#[test]
fn execute_handles_various_file_sizes() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Test various file sizes
    let sizes = vec![
        ("tiny.bin", 1),
        ("small.bin", 100),
        ("medium.bin", 10_000),
        ("large.bin", 100_000),
    ];

    for (name, size) in &sizes {
        let content = vec![0xABu8; *size];
        fs::write(source_root.join(name), &content).expect("write file");
    }

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), sizes.len() as u64);

    for (name, size) in &sizes {
        let dest_file = dest_root.join(name);
        assert_eq!(
            fs::metadata(&dest_file).expect("metadata").len(),
            *size as u64,
            "file {} has wrong size",
            name
        );
    }
}

#[test]
fn execute_copies_file_exactly_one_block_size() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("block.bin");
    let destination = temp.path().join("dest.bin");

    // Exactly 64KB (common block size)
    let content = vec![0x42u8; 65536];
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 65536);
    assert_eq!(fs::read(&destination).expect("read dest"), content);
}

// ==================== Binary Data Tests ====================

#[test]
fn execute_copies_binary_data_correctly() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("binary.dat");
    let destination = temp.path().join("dest.dat");

    // Binary data with all byte values
    let mut binary_content = Vec::new();
    for i in 0..=255u8 {
        binary_content.push(i);
    }

    fs::write(&source, &binary_content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), binary_content);
}

#[test]
fn execute_copies_file_with_null_bytes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("nulls.bin");
    let destination = temp.path().join("dest.bin");

    let content = vec![0x00u8, 0xFF, 0x00, 0xFF, 0x00];
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), content);
}

// ==================== Statistics and Reporting Tests ====================

#[test]
fn execute_summary_counts_bytes_correctly() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("file1.txt"), b"12345").expect("write file1");
    fs::write(source_root.join("file2.txt"), b"67890").expect("write file2");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.bytes_copied(), 10);
    assert_eq!(summary.total_source_bytes(), 10);
}

#[test]
fn execute_summary_reports_zero_for_no_changes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"unchanged";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let source_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&source).expect("source metadata"),
    );
    set_file_mtime(&destination, source_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("execution succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.bytes_copied(), 0);
}

// ==================== Edge Cases ====================

#[test]
fn execute_handles_filename_with_spaces() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("file with spaces.txt");
    let destination = temp.path().join("dest with spaces.txt");

    fs::write(&source, b"spaces test").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"spaces test");
}

#[test]
fn execute_handles_filename_with_special_characters() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("file-with_special.chars!.txt");
    let destination = temp.path().join("dest-special!.txt");

    fs::write(&source, b"special").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"special");
}

#[test]
fn execute_handles_deep_directory_nesting() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");

    // Create a deeply nested directory (10 levels)
    let mut deep_path = source_root.clone();
    for i in 0..10 {
        deep_path = deep_path.join(format!("level{}", i));
    }
    fs::create_dir_all(&deep_path).expect("create deep path");
    fs::write(deep_path.join("deep.txt"), b"very deep").expect("write deep file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify the deeply nested file was copied
    let mut expected_path = dest_root.clone();
    for i in 0..10 {
        expected_path = expected_path.join(format!("level{}", i));
    }
    expected_path = expected_path.join("deep.txt");
    assert!(expected_path.exists());
    assert_eq!(fs::read(&expected_path).expect("read deep file"), b"very deep");
}

#[test]
fn execute_copies_multiple_empty_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("empty1")).expect("create empty1");
    fs::create_dir_all(source_root.join("empty2")).expect("create empty2");
    fs::create_dir_all(source_root.join("empty3")).expect("create empty3");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert!(dest_root.join("empty1").is_dir());
    assert!(dest_root.join("empty2").is_dir());
    assert!(dest_root.join("empty3").is_dir());
}

// ==================== Dry Run Validation Tests ====================

#[test]
fn execute_dry_run_reports_correct_statistics() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file1.txt"), b"content1").expect("write file1");
    fs::write(source_root.join("file2.txt"), b"content2").expect("write file2");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert!(!dest_root.exists(), "dry run should not create destination");
}

#[test]
fn execute_dry_run_with_times_enabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"times test").expect("write source");
    let past_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source, past_time).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().times(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(!destination.exists());
}
