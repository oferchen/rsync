// Tests for --munge-links symlink munging in the local copy path.
//
// When munge_links is enabled:
// - Source symlinks are read and their targets unmunged (if already prefixed)
// - Destination symlinks are created with the munged prefix `/rsyncd-munged/`
// - Roundtripping through munge/unmunge preserves the original target

#[cfg(unix)]
#[test]
fn munge_links_prefixes_absolute_symlink_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target_file = source_root.join("target.txt");
    fs::write(&target_file, b"content").expect("write target");

    let link = source_root.join("abs_link");
    symlink(Path::new("/etc/passwd"), &link).expect("create absolute symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true).munge_links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("abs_link");
    assert!(
        fs::symlink_metadata(&dest_link)
            .expect("meta")
            .file_type()
            .is_symlink()
    );

    let copied_target = fs::read_link(&dest_link).expect("read link");
    assert_eq!(
        copied_target,
        Path::new("/rsyncd-munged//etc/passwd"),
        "absolute symlink target should be munged"
    );
}

#[cfg(unix)]
#[test]
fn munge_links_prefixes_relative_symlink_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target_file = source_root.join("target.txt");
    fs::write(&target_file, b"content").expect("write target");

    let link = source_root.join("rel_link");
    symlink(Path::new("../secret"), &link).expect("create relative symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true).munge_links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("rel_link");
    let copied_target = fs::read_link(&dest_link).expect("read link");
    assert_eq!(
        copied_target,
        Path::new("/rsyncd-munged/../secret"),
        "relative symlink target should be munged"
    );
}

#[cfg(unix)]
#[test]
fn munge_links_disabled_preserves_symlink_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target_file = source_root.join("target.txt");
    fs::write(&target_file, b"content").expect("write target");

    let link = source_root.join("link");
    symlink(Path::new("/etc/passwd"), &link).expect("create absolute symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true).munge_links(false),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("link");
    let copied_target = fs::read_link(&dest_link).expect("read link");
    assert_eq!(
        copied_target,
        Path::new("/etc/passwd"),
        "without munge_links, symlink target should be unchanged"
    );
}

#[cfg(unix)]
#[test]
fn munge_links_unmunges_already_munged_source() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Source symlink is already munged (simulating a previous munge operation)
    let link = source_root.join("munged_link");
    symlink(
        Path::new("/rsyncd-munged//etc/passwd"),
        &link,
    )
    .expect("create pre-munged symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true).munge_links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("munged_link");
    let copied_target = fs::read_link(&dest_link).expect("read link");
    // The source target was `/rsyncd-munged//etc/passwd`, which gets unmunged
    // to `/etc/passwd`, then re-munged back to `/rsyncd-munged//etc/passwd`.
    assert_eq!(
        copied_target,
        Path::new("/rsyncd-munged//etc/passwd"),
        "already-munged source should roundtrip correctly"
    );
}

#[cfg(unix)]
#[test]
fn munge_links_safe_relative_target_still_munged() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target_file = source_root.join("sibling.txt");
    fs::write(&target_file, b"data").expect("write target");

    let link = source_root.join("safe_link");
    symlink(Path::new("sibling.txt"), &link).expect("create safe symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true).munge_links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("safe_link");
    let copied_target = fs::read_link(&dest_link).expect("read link");
    assert_eq!(
        copied_target,
        Path::new("/rsyncd-munged/sibling.txt"),
        "even safe relative targets should be munged when munge_links is enabled"
    );
}
