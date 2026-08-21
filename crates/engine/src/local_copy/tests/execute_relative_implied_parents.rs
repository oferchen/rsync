// Tests for --relative implied-parent creation across an in-the-way symlink.
//
// upstream: generator.c:1839-1842 recv_generator() receives each implied parent
// as its own file-list entry and decides that level on its own, so a symlink
// occupying one of them is found rather than traversed. Creating the whole
// parent path in one step instead follows the symlink, and the directories land
// wherever it points.

/// Builds `src/a/b/f` plus a destination root whose `a` component is a symlink
/// pointing outside the destination tree, and returns the `-R`-style operands.
#[cfg(unix)]
fn escaping_implied_parent_fixture(temp: &Path) -> (Vec<OsString>, PathBuf, PathBuf) {
    let source_root = temp.join("src");
    fs::create_dir_all(source_root.join("a").join("b")).expect("create source tree");
    fs::write(source_root.join("a").join("b").join("f"), b"payload").expect("write source");

    let destination_root = temp.join("dst");
    fs::create_dir_all(&destination_root).expect("create destination root");
    let outside = temp.join("outside");
    fs::create_dir_all(&outside).expect("create outside tree");
    std::os::unix::fs::symlink(&outside, destination_root.join("a")).expect("plant symlink");

    let operand = source_root.join(".").join("a").join("b").join("f");
    let operands = vec![
        operand.into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    (operands, destination_root, outside)
}

#[cfg(unix)]
#[test]
fn relative_implied_parents_do_not_follow_a_symlinked_component() {
    let temp = create_tempdir();
    let (operands, _destination_root, outside) = escaping_implied_parent_fixture(temp.path());
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().relative_paths(true),
        )
        .expect_err("a non-directory occupying an implied parent is refused");

    assert!(
        matches!(
            error.kind(),
            LocalCopyErrorKind::InvalidArgument(
                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory
            )
        ),
        "unexpected error kind: {:?}",
        error.kind()
    );
    assert_eq!(
        fs::read_dir(&outside)
            .expect("read outside tree")
            .count(),
        0,
        "implied parents must not be created through the symlink"
    );
}

/// Non-vacuity companion: on the same fixture, `--force` clears the symlink and
/// the transfer completes wholly inside the destination tree. Without this the
/// pin above would also hold if the fixture never reached parent creation at
/// all.
#[cfg(unix)]
#[test]
fn forced_relative_implied_parents_replace_the_symlink_in_place() {
    let temp = create_tempdir();
    let (operands, destination_root, outside) = escaping_implied_parent_fixture(temp.path());
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .relative_paths(true)
            .force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let copied = destination_root.join("a").join("b").join("f");
    assert_eq!(fs::read(&copied).expect("read copied"), b"payload");
    assert!(
        destination_root
            .join("a")
            .symlink_metadata()
            .expect("stat replaced component")
            .file_type()
            .is_dir(),
        "the in-the-way symlink must be replaced by a real directory"
    );
    assert_eq!(
        fs::read_dir(&outside)
            .expect("read outside tree")
            .count(),
        0,
        "nothing may be written outside the destination tree"
    );
}
