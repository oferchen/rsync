// Tests for the per-type "created" stat counters that feed `--stats`
// "Number of created files: N (reg: .., dir: .., link: .., dev: .., special: ..)".
//
// upstream: receiver.c:733-746 / sender.c:295-308 - every ITEM_IS_NEW entry
// bumps `stats.created_*` for its type, whether or not it transferred file
// data. An in-place update of a pre-existing file/symlink is transferred but
// is NOT ITEM_IS_NEW, so it must never inflate the created counts. These tests
// pin that new-vs-updated distinction, which the "copied" tallies (files_copied,
// symlinks_copied) deliberately do not make.

fn build_new_source_tree(root: &std::path::Path) {
    fs::create_dir_all(root).expect("create source root");
    fs::create_dir_all(root.join("subdir")).expect("create subdir");
    // A brand-new empty file transfers no data but is still ITEM_IS_NEW, so it
    // must count as a created regular file.
    fs::write(root.join("empty"), b"").expect("write empty file");
    fs::write(root.join("data"), b"payload").expect("write data file");
    std::os::unix::fs::symlink("data", root.join("link")).expect("create symlink");
}

#[cfg(unix)]
#[test]
fn created_counts_include_new_nontransferring_entries() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    build_new_source_tree(&source);

    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid options");
    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("recursive copy succeeds");

    // Both regular files are newly created, including the zero-byte one that
    // moved no data. upstream counts it via `stats.created_files++` on the
    // ITEM_IS_NEW path even though `!(iflags & ITEM_TRANSFER)` skips the body.
    assert_eq!(
        summary.created_regular_files(),
        2,
        "new empty + data files must both count as created reg"
    );
    assert_eq!(
        summary.created_symlinks(),
        1,
        "the new symlink must count as a created link"
    );
    // The created directory tally covers the freshly made subdir; the executor
    // also counts the synthesized destination root, so at least both dirs show.
    assert!(
        summary.directories_created() >= 2,
        "new subdir + destination root must count as created dirs, got {}",
        summary.directories_created()
    );
}

#[cfg(unix)]
#[test]
fn updated_entries_are_not_counted_as_created() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    build_new_source_tree(&source);

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];

    // First run materialises everything.
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::builder().archive().build().expect("options"),
    )
    .expect("initial copy succeeds");

    // Mutate a pre-existing file (different size) and re-point the symlink so
    // both are re-transferred on the second run. Neither destination is new.
    fs::write(source.join("data"), b"payload-grown-larger").expect("rewrite data file");
    fs::remove_file(source.join("link")).expect("remove symlink");
    std::os::unix::fs::symlink("empty", source.join("link")).expect("recreate symlink");

    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::builder().archive().build().expect("options"),
        )
        .expect("second copy succeeds");

    // The changed file IS transferred (files_copied bumps) but was NOT created;
    // upstream leaves ITEM_IS_NEW clear because the destination already existed.
    assert!(
        summary.files_copied() >= 1,
        "the modified file must be re-transferred"
    );
    assert_eq!(
        summary.created_regular_files(),
        0,
        "an in-place update must not count as a created reg file"
    );
    assert_eq!(
        summary.created_symlinks(),
        0,
        "a re-pointed pre-existing symlink must not count as created"
    );
}

#[cfg(unix)]
#[test]
fn dry_run_counts_created_entries_like_a_real_run() {
    // upstream: a dry-run still walks itemize() and bumps `stats.created_*`, so
    // `--dry-run --stats` reports the same created counts as a real transfer.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    build_new_source_tree(&source);

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::builder().archive().build().expect("options"),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.created_regular_files(), 2);
    assert_eq!(summary.created_symlinks(), 1);
    assert!(!dest.exists(), "dry run must not touch the destination");
}
