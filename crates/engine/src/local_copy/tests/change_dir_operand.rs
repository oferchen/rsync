// A non-`--relative` operand that ends in a DOTDIR marker (`dir/`, `dir/.`,
// `dir/..`) names a directory upstream must `chdir` into before it stats
// anything (flist.c:2847-2858, 2914-2921). When that fails it reports
// `change_dir "<dir>" failed`, sets IOERR_GENERAL and skips the operand - the
// `--missing-args` handling never runs, because it only follows a failed
// `link_stat` of the `.` half.
//
// Measured against rsync 3.5.1 (`-a --delete-missing-args src/nope/ src/a dst/`
// with `dst/nope/f` present): exit 23,
// `rsync: [sender] change_dir "<cwd>/src/nope" failed: No such file or directory (2)`,
// `dst/a` copied and `dst/nope/f` left in place. Without the slash the same
// operand is a missing-args entry instead.
#[test]
fn change_dir_failure_names_the_dir_half_as_upstream_spells_it() {
    let temp = create_tempdir();
    let base = temp.path();
    fs::write(base.join("file"), b"x").expect("write file");
    fs::create_dir(base.join("dir")).expect("create dir");
    let dir_of = |operand: PathBuf| operand_change_dir_failure(&operand).map(|(dir, _)| dir);

    assert_eq!(
        dir_of(base.join("nope/")),
        Some(base.join("nope")),
        "`nope/` becomes `nope/.`, split at the last `/` (flist.c:2829-2834)"
    );
    assert_eq!(
        dir_of(base.join("nope/.")),
        Some(base.join("nope")),
        "`nope/.` splits the same way"
    );
    // Windows resolves `nope\..` lexically, so the chdir succeeds there even
    // though `nope` is missing; only POSIX walks the missing component.
    #[cfg(unix)]
    assert_eq!(
        dir_of(base.join("nope/..")).map(PathBuf::into_os_string),
        Some(base.join("nope/..").into_os_string()),
        "`nope/..` gains `/.` and keeps `..` in its dir half (flist.c:2835-2842)"
    );
    assert_eq!(
        dir_of(base.join("dir/")),
        None,
        "an enterable dir is no failure"
    );
    assert_eq!(
        dir_of(base.join("nope")),
        None,
        "an unmarked operand takes the link_stat route instead"
    );
}

#[test]
fn change_dir_failure_carries_the_error_chdir_would_report() {
    let temp = create_tempdir();
    let base = temp.path();
    fs::write(base.join("file"), b"x").expect("write file");
    let kind = |operand: PathBuf| operand_change_dir_failure(&operand).map(|(_, e)| e.kind());
    assert_eq!(kind(base.join("nope/")), Some(io::ErrorKind::NotFound));
    assert_eq!(
        kind(base.join("file/")),
        Some(io::ErrorKind::NotADirectory),
        "upstream reports `Not a directory (20)` for a file named with a slash"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses the search-permission check, as it does for chdir().
        let unsearchable = base.join("unsearchable");
        fs::create_dir(&unsearchable).expect("create dir");
        fs::set_permissions(&unsearchable, fs::Permissions::from_mode(0o600)).expect("chmod");
        let denied = fs::read_dir(unsearchable.join(".")).is_err();
        let result = kind(base.join("unsearchable/"));
        fs::set_permissions(&unsearchable, fs::Permissions::from_mode(0o700)).expect("restore");
        if denied {
            assert_eq!(
                result,
                Some(io::ErrorKind::PermissionDenied),
                "chdir needs search permission on the dir itself"
            );
        }
    }
}

#[test]
fn delete_missing_args_does_not_reach_a_trailing_slash_operand() {
    let temp = create_tempdir();
    let source_root = temp.path().join("src");
    fs::create_dir(&source_root).expect("create source root");
    let present = source_root.join("a");
    fs::write(&present, b"hi\n").expect("write source");
    let destination_root = temp.path().join("dst");
    let stale_dir = destination_root.join("nope");
    fs::create_dir_all(&stale_dir).expect("create stale dir");
    fs::write(stale_dir.join("f"), b"keep").expect("write stale file");

    let mut missing = source_root.join("nope").into_os_string();
    missing.push("/");
    let operands = vec![
        missing,
        present.into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let error = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delete_missing_args(true),
        )
        .expect_err("an unenterable operand dir is a transfer error (exit 23)");

    assert!(error.is_change_dir_failed(), "got {error}");
    assert_eq!(error.exit_code(), 23);
    assert!(
        error.to_string().starts_with("change_dir \"")
            && error.to_string().contains("nope\" failed: "),
        "upstream's text names the dir half: {error}"
    );
    assert_eq!(
        fs::read(stale_dir.join("f")).expect("stale file survives"),
        b"keep",
        "the missing-args deletion must never run for this operand"
    );
    assert_eq!(
        fs::read(destination_root.join("a")).expect("sibling operand copied"),
        b"hi\n",
        "the transfer continues with the remaining operands"
    );
}
