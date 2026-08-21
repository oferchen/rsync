// Tests for --relative implied-parent creation across an in-the-way symlink.
//
// upstream: generator.c:1839-1842 recv_generator() receives each implied parent
// as its own file-list entry and decides that level on its own: a non-directory
// found in its place is removed and a real directory created there. Resolving
// the whole parent path in one step instead follows such a component, and the
// directories land wherever it points.

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

/// The implied parents must be created inside the destination tree, replacing
/// the symlink rather than following it.
///
/// The two halves are load-bearing together: "nothing outside" alone would hold
/// if the transfer never reached parent creation, so the landed file pins that
/// both levels really were created through the obstructed component.
#[cfg(unix)]
#[test]
fn relative_implied_parents_replace_a_symlinked_component_in_place() {
    let temp = create_tempdir();
    let (operands, destination_root, outside) = escaping_implied_parent_fixture(temp.path());
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().relative_paths(true),
    )
    .expect("implied parents are created inside the destination tree");

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
        fs::read_dir(&outside).expect("read outside tree").count(),
        0,
        "implied parents must not be created through the symlink"
    );
}

/// Dry run must report the same outcome without touching either tree - in
/// particular without creating the implied parents through the symlink.
#[cfg(unix)]
#[test]
fn relative_implied_parents_dry_run_touches_neither_tree() {
    let temp = create_tempdir();
    let (operands, destination_root, outside) = escaping_implied_parent_fixture(temp.path());
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::DryRun,
        LocalCopyOptions::default().relative_paths(true),
    )
    .expect("dry run succeeds");

    assert!(
        destination_root
            .join("a")
            .symlink_metadata()
            .expect("stat component")
            .file_type()
            .is_symlink(),
        "dry run must leave the symlink in place"
    );
    assert_eq!(
        fs::read_dir(&outside).expect("read outside tree").count(),
        0,
        "dry run must not create anything through the symlink"
    );
}
