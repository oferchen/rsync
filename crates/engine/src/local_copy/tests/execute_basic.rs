
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

#[cfg(feature = "xattr")]
#[test]
fn execute_copies_file_with_xattrs() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"attr").expect("write source");
    xattr::set(&source, "user.demo", b"value").expect("set xattr");

    let operands = vec![
        source.clone().into_os_string(),
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

#[cfg(all(unix, feature = "acl"))]
#[test]
fn execute_copies_file_with_acls() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"acl").expect("write source");
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

    assert_eq!(summary.files_copied(), 1);
    let copied = acl_to_text(&destination, acl_sys::ACL_TYPE_ACCESS).expect("dest acl");
    assert!(copied.contains("user::rw-"));
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
        source_root.clone().into_os_string(),
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
