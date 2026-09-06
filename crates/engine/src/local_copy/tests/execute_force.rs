// Tests for --force behavior: replacing directories with non-directories and vice versa.
//
// From the rsync man page: "This option tells rsync to delete a non-empty directory
// when it is to be replaced by a non-directory. This is only relevant if deletions
// are not active."

#[test]
fn force_file_replaces_non_empty_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("item");
    fs::write(&source, b"file-content").expect("write source file");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("subdir")).expect("create nested dir");
    fs::write(destination.join("subdir/child.txt"), b"child").expect("write child");
    fs::write(destination.join("existing.txt"), b"existing").expect("write existing");

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

    assert!(
        destination.is_file(),
        "directory should be replaced by file"
    );
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"file-content"
    );
    assert_eq!(summary.files_copied(), 1);
}

// upstream: generator.c:2148-2153 makes room for the arriving regular file with
// `delete_item(fname, mode, del_opts | DEL_FOR_FILE)`, called with no option
// gate at all; `del_opts` carries DEL_RECURSE only under --delete/--force and
// selects the RECURSION (delete.c:207-209). An EMPTY directory obstacle is
// therefore rmdir'd and the file written, with no --force.
//
// The flist.c:3067-3081 rule this cell used to cite is the MULTI-SOURCE merge -
// see `force_disabled_multi_source_keeps_the_directory` - and a single source
// never reaches it. Measured against rsync 3.5.0.
#[test]
fn force_disabled_file_replaces_an_empty_directory_in_recursive_copy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("item"), b"replacement").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(dest_root.join("item")).expect("create conflicting directory");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(false),
    )
    .expect("an empty directory obstacle is rmdir'd, not refused");

    assert_eq!(
        fs::read(dest_root.join("item")).expect("the file must replace the directory"),
        b"replacement",
    );
}

#[test]
fn force_file_replaces_deeply_nested_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("item");
    fs::write(&source, b"flat").expect("write source");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("a/b/c/d")).expect("create deep tree");
    fs::write(destination.join("a/b/c/d/leaf.txt"), b"deep").expect("write leaf");
    fs::write(destination.join("a/top.txt"), b"top").expect("write top");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement succeeds");

    assert!(
        destination.is_file(),
        "deeply nested directory replaced by file"
    );
    assert_eq!(fs::read(&destination).expect("read"), b"flat");
}

#[test]
fn force_directory_replaces_file() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("srcdir");
    fs::create_dir_all(&source_root).expect("create source dir");
    fs::write(source_root.join("inner.txt"), b"inner").expect("write inner");

    let destination = temp.path().join("dest");
    fs::write(&destination, b"old-file").expect("write existing file");

    let operands = vec![
        source_root.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement succeeds");

    assert!(destination.is_dir(), "file should be replaced by directory");
    assert_eq!(
        fs::read(destination.join("srcdir").join("inner.txt")).expect("read inner"),
        b"inner"
    );
}

/// Companion to `force_replaces_file_with_directory_during_recursive_copy`:
/// clearing a non-directory that obstructs a directory is not `--force`'s job.
/// upstream: `generator.c:1839-1842` runs `delete_item(.., DEL_FOR_DIR)`
/// unconditionally; `--force` only adds `DEL_RECURSE` (`generator.c:1629`),
/// which governs recursing into a non-empty *directory*.
#[test]
fn force_disabled_directory_still_replaces_file() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("srcdir");
    fs::create_dir_all(&source_root).expect("create source dir");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let destination = temp.path().join("dest");
    fs::write(&destination, b"existing file").expect("write existing file");

    let operands = vec![
        source_root.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(false),
    )
    .expect("obstruction cleared without --force");

    assert!(destination.is_dir(), "file replaced by a directory");
    assert_eq!(
        fs::read(destination.join("srcdir").join("file.txt")).expect("read copied file"),
        b"content"
    );
}

#[test]
fn force_replaces_directory_entry_with_file_during_recursive_copy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    // Source has "item" as a regular file
    fs::write(source_root.join("item"), b"file-data").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    // Destination has "item" as a directory with contents
    fs::create_dir_all(dest_root.join("item/subdir")).expect("create conflicting directory");
    fs::write(dest_root.join("item/existing.txt"), b"old").expect("write old");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let item_path = dest_root.join("item");
    assert!(
        item_path.is_file(),
        "directory entry should be replaced by file"
    );
    assert_eq!(fs::read(&item_path).expect("read"), b"file-data");
}

#[test]
fn force_replaces_file_entry_with_directory_during_recursive_copy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    // Source has "item" as a directory
    fs::create_dir_all(source_root.join("item")).expect("create source dir");
    fs::write(source_root.join("item/child.txt"), b"child").expect("write child");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    // Destination has "item" as a regular file
    fs::write(dest_root.join("item"), b"old-file").expect("write conflicting file");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let item_path = dest_root.join("item");
    assert!(
        item_path.is_dir(),
        "file entry should be replaced by directory"
    );
    assert_eq!(
        fs::read(item_path.join("child.txt")).expect("read child"),
        b"child"
    );
}

// upstream: generator.c:2148-2153 calls `delete_item(fname, mode, del_opts |
// DEL_FOR_FILE)` UNCONDITIONALLY when a regular file arrives over a
// destination that is not a regular file. `del_opts` carries `DEL_RECURSE`
// only under `--delete`/`--force`, and delete.c:207-209 says what that flag
// selects: "If DEL_RECURSE is not set, this just reports emptiness". So an
// EMPTY directory is rmdir'd and the file written with no options at all.
//
// This cell used to assert the opposite - that the directory survived and the
// file was silently dropped at exit 0 - citing flist.c:3067-3081. That
// citation is the MULTI-SOURCE merge rule (see
// `force_disabled_multi_source_keeps_the_directory` below); it does not reach
// a single-source recursive copy, where the file is in the flist and the
// generator makes room for it. Measured against rsync 3.5.0.
#[test]
fn force_disabled_recursive_copy_replaces_an_empty_directory() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    // Source has "item" as a regular file
    fs::write(source_root.join("item"), b"file-data").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    // Destination has "item" as a directory
    fs::create_dir_all(dest_root.join("item")).expect("create conflicting directory");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(false),
    )
    .expect("an empty directory obstacle is rmdir'd, not refused");

    assert_eq!(
        fs::read(dest_root.join("item")).expect("the file must replace the directory"),
        b"file-data",
    );
}

// The multi-source exception the cell above used to claim for itself.
//
// upstream: flist.c:3067-3081 flist_sort_and_clean() drops the colliding
// regular file and keeps the directory, so the entry never reaches
// recv_generator and the destination directory is left alone at exit 0.
#[test]
fn force_disabled_multi_source_keeps_the_directory() {
    let temp = tempdir().expect("tempdir");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir_all(&first).expect("create first source");
    fs::create_dir_all(second.join("item")).expect("create second source dir");
    fs::write(first.join("item"), b"file-data").expect("write colliding file");
    fs::write(second.join("item/child"), b"child").expect("write child");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(dest_root.join("item")).expect("create conflicting directory");

    let mut first_operand = first.into_os_string();
    first_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let mut second_operand = second.into_os_string();
    second_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![
        first_operand,
        second_operand,
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(false),
    )
    .expect("the colliding file is dropped, not an error");

    assert!(
        dest_root.join("item").is_dir(),
        "a file contributed by one source must never blow away a directory \
         contributed by another"
    );
}

#[test]
fn force_dry_run_does_not_modify_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("item");
    fs::write(&source, b"replacement").expect("write source");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("inner")).expect("create dest dir structure");
    fs::write(destination.join("inner/keep.txt"), b"keep").expect("write keep");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::DryRun,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("dry-run succeeds");

    assert!(
        destination.is_dir(),
        "directory should not be modified in dry-run"
    );
    assert_eq!(
        fs::read(destination.join("inner/keep.txt")).expect("read"),
        b"keep"
    );
}

#[test]
fn force_dry_run_make_room_for_empty_directory_is_silent() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("item");
    fs::write(&source, b"replacement").expect("write source");

    let destination = temp.path().join("dest");
    fs::create_dir_all(&destination).expect("create dest dir");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .force_replacements(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    let summary = report.summary();
    // upstream: generator.c:1240 clears the conflicting directory with
    // DEL_FOR_FILE; delete.c:179 delete_item() removes the make-room target
    // silently and uncounted. An empty directory has no contents to recurse
    // over, so no deletion is itemized or counted.
    assert_eq!(
        summary.items_deleted(),
        0,
        "make-room removal of an empty directory is silent and uncounted"
    );
    assert_eq!(summary.files_copied(), 1, "should report one file copy");
}

#[cfg(unix)]
#[test]
fn force_symlink_replaces_non_empty_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let link_target = temp.path().join("target.txt");
    fs::write(&link_target, b"target").expect("write target");

    let source_link = temp.path().join("link");
    symlink(&link_target, &source_link).expect("create symlink");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("contents")).expect("create dest dir");
    fs::write(destination.join("contents/file.txt"), b"old").expect("write file");

    let operands = vec![
        source_link.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .links(true)
            .force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let metadata = fs::symlink_metadata(&destination).expect("dest metadata");
    assert!(
        metadata.file_type().is_symlink(),
        "directory should be replaced by symlink"
    );
    assert_eq!(fs::read_link(&destination).expect("read link"), link_target);
}

// upstream: generator.c:2469-2483 atomic_create() - `dir_in_the_way` forces
// `skip_atomic`, so a directory obstacle takes the `delete_item()` arm whether
// or not --backup is set, and `del_opts` selects only the RECURSION. An EMPTY
// directory is therefore rmdir'd and the symlink created in its place at exit
// 0, with no --force.
//
// This cell used to assert that the run FAILED with the oc-only
// `ReplaceDirectoryWithSymlink` argument error. Upstream has no such error and
// never aborts a run for an obstacle; a POPULATED directory is refused per
// entry at exit 23 instead (tests/local_directory_obstacle_removal.rs pins
// that half end to end). Measured against rsync 3.5.0.
#[cfg(unix)]
#[test]
fn force_disabled_symlink_replaces_an_empty_directory_in_recursive_copy() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    let link_target = temp.path().join("target.txt");
    fs::write(&link_target, b"target").expect("write target");
    symlink(&link_target, source_root.join("link")).expect("create symlink");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(dest_root.join("link")).expect("create conflicting directory");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .links(true)
            .force_replacements(false),
    )
    .expect("an empty directory obstacle is rmdir'd, not refused");

    assert_eq!(
        fs::read_link(dest_root.join("link")).expect("the symlink must replace the directory"),
        link_target,
    );
}

// Creates no socket, so the Apple exclusion the neighbouring socket tests
// carry does not apply: mkfifo_for_tests has an Apple arm and force-replacing
// a populated directory is not platform-specific. Without this, replacing a
// NON-EMPTY directory with a FIFO is covered on no Apple platform - the
// macOS-running execute_fifo_replaces_directory_when_force_enabled uses an
// empty one, so the recursive-removal half goes untested there.
#[cfg(unix)]
#[test]
fn force_fifo_replaces_non_empty_directory() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("subdir")).expect("create dest dir");
    fs::write(destination.join("subdir/file.txt"), b"old").expect("write file");

    let operands = vec![
        source_fifo.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .specials(true)
            .force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let metadata = fs::symlink_metadata(&destination).expect("dest metadata");
    assert!(
        metadata.file_type().is_fifo(),
        "directory should be replaced by FIFO"
    );
}

#[test]
fn force_handles_multiple_type_conflicts_in_one_copy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    // Source: "alpha" is a file, "beta" is a directory
    fs::write(source_root.join("alpha"), b"alpha-file").expect("write alpha");
    fs::create_dir_all(source_root.join("beta")).expect("create beta dir");
    fs::write(source_root.join("beta/inside.txt"), b"inside").expect("write inside");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Destination: "alpha" is a directory, "beta" is a file (opposite types)
    fs::create_dir_all(dest_root.join("alpha/child")).expect("create alpha dir");
    fs::write(dest_root.join("alpha/child/deep.txt"), b"deep").expect("write deep");
    fs::write(dest_root.join("beta"), b"beta-file").expect("write beta");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement with multiple conflicts succeeds");

    assert!(
        dest_root.join("alpha").is_file(),
        "alpha: directory should become file"
    );
    assert_eq!(
        fs::read(dest_root.join("alpha")).expect("read alpha"),
        b"alpha-file"
    );

    assert!(
        dest_root.join("beta").is_dir(),
        "beta: file should become directory"
    );
    assert_eq!(
        fs::read(dest_root.join("beta/inside.txt")).expect("read inside"),
        b"inside"
    );
}

#[test]
fn force_replaces_file_in_parent_path_with_directory() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("parent/child")).expect("create source tree");
    fs::write(source_root.join("parent/child/file.txt"), b"nested").expect("write nested");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    // Create "parent" as a file at the destination, blocking directory creation
    fs::write(dest_root.join("parent"), b"blocker").expect("write blocker file");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement of parent path succeeds");

    assert!(
        dest_root.join("parent").is_dir(),
        "parent file should become directory"
    );
    assert_eq!(
        fs::read(dest_root.join("parent/child/file.txt")).expect("read nested"),
        b"nested"
    );
}

#[test]
fn force_replacement_counts_deletion() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("item");
    fs::write(&source, b"new-content").expect("write source");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("old-contents")).expect("create dest dir");
    fs::write(destination.join("old-contents/file.txt"), b"old").expect("write old");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .force_replacements(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("forced replacement succeeds");

    let summary = report.summary();
    // upstream: the make-room removal of the conflicting directory node is
    // silent (DEL_FOR_FILE), but delete.c:83 delete_dir_contents() recurses
    // with DEL_MAKE_ROOM stripped, so its non-empty contents are counted like
    // ordinary deletions.
    assert!(
        summary.items_deleted() >= 1,
        "should count the conflicting directory's contents as deletions"
    );
    assert_eq!(summary.files_copied(), 1, "should count the file copy");
}

#[test]
fn force_with_no_type_conflict_copies_normally() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    // No conflicting entries at destination

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().force_replacements(true),
        )
        .expect("copy succeeds");

    assert!(dest_root.join("source").join("file.txt").is_file());
    assert_eq!(
        fs::read(dest_root.join("source").join("file.txt")).expect("read"),
        b"content"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 0, "no deletions when no conflict");
}

#[test]
fn force_overwrite_file_with_file_is_normal_copy() {
    // When source and destination are both files, --force should not trigger
    // any directory removal. Use --ignore-times to ensure the copy happens
    // even if timestamps match.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("file.txt");
    fs::write(&source, b"new-content").expect("write source");

    let destination = temp.path().join("dest.txt");
    fs::write(&destination, b"old-content").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .force_replacements(true)
                .ignore_times(true),
        )
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read"), b"new-content");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        summary.items_deleted(),
        0,
        "no force deletion needed for same-type overwrite"
    );
}
