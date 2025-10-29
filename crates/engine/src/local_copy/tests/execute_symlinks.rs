
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

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");
    let copied = fs::read_link(dest_link).expect("read copied link");
    assert_eq!(copied, target);
    assert_eq!(summary.symlinks_copied(), 1);
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

    let options = LocalCopyOptions::default().copy_links(true);
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

    let options = LocalCopyOptions::default().copy_links(true);
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

    let options = LocalCopyOptions::default().copy_dirlinks(true);
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

    let options = LocalCopyOptions::default().copy_dirlinks(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
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

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().safe_links(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
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
