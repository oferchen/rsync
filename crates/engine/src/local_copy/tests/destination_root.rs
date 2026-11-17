// Destination-root resolution, mirroring upstream `get_local_name()`
// (main.c:775-868). The receiver decides what the destination operand names
// from the first file list's entry count, not from how many operands were
// typed: more than one entry needs a directory, a single entry is written under
// the destination's own name from inside its parent.
//
// Each case below was measured against rsync 3.5.2 on Linux.

/// `one missing dst` lists only `one`, so `dst` names the copied file itself.
/// rsync 3.5.2: `link_stat ... failed`, exit 23, `dst` a regular file.
#[test]
fn a_missing_operand_does_not_count_toward_the_destination_directory() {
    let temp = create_tempdir();
    let one = temp.path().join("one");
    fs::write(&one, b"one").expect("write source");
    let destination = temp.path().join("dst");
    let operands = vec![
        one.into_os_string(),
        temp.path().join("missing").into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan.execute().expect_err("the missing operand is reported");

    assert!(error.is_link_stat_failed(), "{error:?}");
    assert_eq!(error.exit_code(), 23);
    assert_eq!(fs::read(&destination).expect("dst is a file"), b"one");
}

/// `one missing nodir/x` writes the single entry inside `nodir`, which does not
/// exist. rsync 3.5.2: the sender's `link_stat` line, then `change_dir#3
/// "<cwd>/nodir" failed: No such file or directory (2)`, exit 3. Creating
/// `nodir/x` as a directory instead would report `mkdir` and exit 11.
#[test]
fn a_single_entry_destination_needs_an_existing_parent() {
    let temp = create_tempdir();
    let one = temp.path().join("one");
    fs::write(&one, b"one").expect("write source");
    let parent = temp.path().join("nodir");
    let operands = vec![
        one.into_os_string(),
        temp.path().join("missing").into_os_string(),
        parent.join("x").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan.execute().expect_err("the parent cannot be entered");

    match error.kind() {
        LocalCopyErrorKind::DestinationParentUnusable { path, source } => {
            assert_eq!(path, &parent);
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
    assert_eq!(error.exit_code(), 3);
    assert!(!parent.exists());
}

/// `-R src/./a/f dst` lists the implied directory `a` with the file, so `dst`
/// is created as a directory. rsync 3.5.2: exit 0, `dst/a/f`.
#[test]
fn a_relative_operand_with_a_directory_component_creates_the_destination() {
    let temp = create_tempdir();
    let source = temp.path().join("src");
    fs::create_dir_all(source.join("a")).expect("create source tree");
    fs::write(source.join("a").join("f"), b"f").expect("write source");
    let destination = temp.path().join("dst");
    let operands = vec![
        source.join(".").join("a").join("f").into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().relative_paths(true),
    )
    .expect("relative copy succeeds");

    assert_eq!(
        fs::read(destination.join("a").join("f")).expect("read copied file"),
        b"f"
    );
}

/// `-R src/./a/f nodir/x`: two entries need `nodir/x` made with one `mkdir`,
/// which fails because `nodir` is missing. rsync 3.5.2: `mkdir
/// "<cwd>/nodir/x" failed: No such file or directory (2)`, exit 11.
#[test]
fn a_relative_operand_into_a_missing_parent_fails_the_destination_mkdir() {
    let temp = create_tempdir();
    let source = temp.path().join("src");
    fs::create_dir_all(source.join("a")).expect("create source tree");
    fs::write(source.join("a").join("f"), b"f").expect("write source");
    let destination = temp.path().join("nodir").join("x");
    let operands = vec![
        source.join(".").join("a").join("f").into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().relative_paths(true),
        )
        .expect_err("mkdir under a missing parent fails");

    match error.kind() {
        LocalCopyErrorKind::DestinationMkdirFailed { path, source } => {
            assert_eq!(path, &destination);
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
    assert_eq!(error.exit_code(), 11);
}

/// `-R src/./a dst` without `-r` skips the directory, so nothing is listed and
/// `dst` is never touched. rsync 3.5.2: `skipping directory a`, exit 0.
#[test]
fn a_skipped_relative_directory_leaves_the_destination_alone() {
    let temp = create_tempdir();
    let source = temp.path().join("src");
    fs::create_dir_all(source.join("a")).expect("create source tree");
    let destination = temp.path().join("dst");
    let operands = vec![
        source.join(".").join("a").into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .relative_paths(true)
            .recursive(false),
    )
    .expect("a skipped directory is not an error");

    assert!(!destination.exists());
}

/// Two files into an existing file. rsync 3.5.2: `ERROR: destination must be a
/// directory when copying more than 1 file`, exit 3, the file untouched.
#[test]
fn more_than_one_entry_refuses_an_existing_file() {
    let temp = create_tempdir();
    let one = temp.path().join("one");
    let two = temp.path().join("two");
    fs::write(&one, b"one").expect("write source");
    fs::write(&two, b"two").expect("write source");
    let destination = temp.path().join("file");
    fs::write(&destination, b"existing").expect("write destination");
    let operands = vec![
        one.into_os_string(),
        two.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute()
        .expect_err("an existing file cannot hold two");

    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::DestinationConflict(DestinationConflict::MoreThanOneFile)
    ));
    assert_eq!(error.exit_code(), 3);
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"existing"
    );
}

/// `-r empty file`: the lone entry is a directory. rsync 3.5.2: `ERROR: cannot
/// overwrite non-directory with a directory`, exit 3, the file untouched.
#[test]
fn a_lone_directory_refuses_an_existing_file() {
    let temp = create_tempdir();
    let empty = temp.path().join("empty");
    fs::create_dir(&empty).expect("create source dir");
    let destination = temp.path().join("file");
    fs::write(&destination, b"existing").expect("write destination");
    let operands = vec![empty.into_os_string(), destination.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute()
        .expect_err("a directory cannot replace the file");

    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::DestinationConflict(DestinationConflict::DirectoryOverNonDirectory)
    ));
    assert_eq!(error.exit_code(), 3);
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"existing"
    );
}
