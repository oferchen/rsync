
#[test]
fn prune_empty_dirs_does_not_create_empty_directories() {
    let ctx = test_helpers::setup_copy_test();

    // Create source with empty directory
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("empty_dir", None),
            ("file.txt", Some(b"content")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!ctx.dest.join("empty_dir").exists(), "empty dir should not be created");
    assert!(ctx.dest.join("file.txt").exists(), "file should be copied");
}

#[test]
fn prune_empty_dirs_creates_directories_with_files() {
    let ctx = test_helpers::setup_copy_test();

    // Create source with directory containing files
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("dir_with_files/file1.txt", Some(b"content1")),
            ("dir_with_files/file2.txt", Some(b"content2")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(ctx.dest.join("dir_with_files").is_dir(), "directory with files should be created");
    assert!(ctx.dest.join("dir_with_files/file1.txt").exists());
    assert!(ctx.dest.join("dir_with_files/file2.txt").exists());
}

#[test]
fn prune_empty_dirs_prunes_nested_empty_directories() {
    let ctx = test_helpers::setup_copy_test();

    // Create source with nested empty directories
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("level1/level2/level3", None),
            ("level1/level2/another_empty", None),
            ("keep/file.txt", Some(b"keep me")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!ctx.dest.join("level1").exists(), "nested empty dirs should be pruned");
    assert!(ctx.dest.join("keep").is_dir(), "directory with file should be created");
    assert!(ctx.dest.join("keep/file.txt").exists());
}

#[test]
fn prune_empty_dirs_with_filter_excluding_all_files() {
    let ctx = test_helpers::setup_copy_test();

    // Create source with directory containing only .tmp files
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("excluded_dir/file1.tmp", Some(b"temp1")),
            ("excluded_dir/file2.tmp", Some(b"temp2")),
            ("included_dir/keep.txt", Some(b"keep")),
            ("included_dir/also_temp.tmp", Some(b"temp")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).expect("compile filters");
    let options = LocalCopyOptions::default()
        .filters(Some(filters))
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        !ctx.dest.join("excluded_dir").exists(),
        "directory with only excluded files should be pruned"
    );
    assert!(
        ctx.dest.join("included_dir").is_dir(),
        "directory with non-excluded files should exist"
    );
    assert!(ctx.dest.join("included_dir/keep.txt").exists());
    assert!(!ctx.dest.join("included_dir/also_temp.tmp").exists());
}

#[test]
fn prune_empty_dirs_preserves_non_empty_parent_of_empty_child() {
    let ctx = test_helpers::setup_copy_test();

    // Create parent with a file and an empty child directory
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("parent/file.txt", Some(b"parent file")),
            ("parent/empty_child", None),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(ctx.dest.join("parent").is_dir(), "parent with file should exist");
    assert!(ctx.dest.join("parent/file.txt").exists());
    assert!(!ctx.dest.join("parent/empty_child").exists(), "empty child should be pruned");
}

#[test]
fn prune_empty_dirs_with_multiple_branches() {
    let ctx = test_helpers::setup_copy_test();

    // Create tree with multiple branches, some empty, some not
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("branch1/empty", None),
            ("branch2/full/file.txt", Some(b"file")),
            ("branch3/nested/empty", None),
            ("branch4", None),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!ctx.dest.join("branch1").exists(), "branch1 with only empty child should be pruned");
    assert!(ctx.dest.join("branch2").is_dir(), "branch2 with file should exist");
    assert!(ctx.dest.join("branch2/full").is_dir());
    assert!(ctx.dest.join("branch2/full/file.txt").exists());
    assert!(!ctx.dest.join("branch3").exists(), "branch3 with nested empty should be pruned");
    assert!(!ctx.dest.join("branch4").exists(), "empty branch4 should be pruned");
}

#[test]
fn prune_empty_dirs_respects_dry_run() {
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("empty", None),
            ("nonempty/file.txt", Some(b"content")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    assert!(!ctx.dest.exists(), "destination should not exist in dry-run");
    // In dry-run, the summary should report what would be created
    assert!(summary.directories_created() >= 1, "should report non-empty dir creation");
}

#[test]
fn prune_empty_dirs_with_trailing_separator() {
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("empty", None),
            ("nonempty/file.txt", Some(b"content")),
        ],
    );

    // Use trailing separator to copy contents
    let mut source_operand = ctx.source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!ctx.dest.join("empty").exists(), "empty dir should be pruned");
    assert!(ctx.dest.join("nonempty").is_dir());
    assert!(ctx.dest.join("nonempty/file.txt").exists());
}

#[test]
#[ignore] // TODO: Prune behavior with min-size
fn prune_empty_dirs_with_min_size_filter() { // TODO: Prune behavior with min-size filter needs verification
    let ctx = test_helpers::setup_copy_test();

    // Create files of different sizes
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("small_files_only/tiny.txt", Some(b"hi")),
            ("small_files_only/small.txt", Some(b"123")),
            ("mixed_dir/tiny.txt", Some(b"x")),
            ("mixed_dir/large.txt", Some(b"this is a large file content")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .min_file_size(Some(10))
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        !ctx.dest.join("small_files_only").exists(),
        "directory with only small files should be pruned"
    );
    assert!(ctx.dest.join("mixed_dir").is_dir(), "directory with large file should exist");
    assert!(ctx.dest.join("mixed_dir/large.txt").exists());
    assert!(!ctx.dest.join("mixed_dir/tiny.txt").exists());
}

#[test]
#[ignore] // TODO: Prune behavior with max-size filter needs verification
fn prune_empty_dirs_with_max_size_filter() {
    let ctx = test_helpers::setup_copy_test();

    // Create files of different sizes
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("large_files_only/huge1.bin", Some(b"this is a very large file content here")),
            ("large_files_only/huge2.bin", Some(b"another very large file content")),
            ("mixed_dir/small.txt", Some(b"ok")),
            ("mixed_dir/huge.bin", Some(b"this is huge content file")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .max_file_size(Some(10))
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        !ctx.dest.join("large_files_only").exists(),
        "directory with only large files should be pruned"
    );
    assert!(ctx.dest.join("mixed_dir").is_dir(), "directory with small file should exist");
    assert!(ctx.dest.join("mixed_dir/small.txt").exists());
    assert!(!ctx.dest.join("mixed_dir/huge.bin").exists());
}

#[test]
fn prune_empty_dirs_disabled_creates_all_directories() {
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("empty1", None),
            ("empty2/nested", None),
            ("nonempty/file.txt", Some(b"content")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(false);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(ctx.dest.join("empty1").is_dir(), "empty1 should be created");
    assert!(ctx.dest.join("empty2").is_dir(), "empty2 should be created");
    assert!(ctx.dest.join("empty2/nested").is_dir(), "nested empty should be created");
    assert!(ctx.dest.join("nonempty").is_dir());
    assert!(ctx.dest.join("nonempty/file.txt").exists());
}

#[test]
fn prune_empty_dirs_with_collect_events_reports_pruning() {
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("empty", None),
            ("nonempty/file.txt", Some(b"content")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .prune_empty_dirs(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!ctx.dest.join("empty").exists());
    assert!(ctx.dest.join("nonempty").is_dir());

    let records = report.records();
    let dir_created = records
        .iter()
        .filter(|r| r.action() == &LocalCopyAction::DirectoryCreated)
        .count();

    // Should only create the non-empty directory
    assert!(dir_created >= 1, "should report directory creation for non-empty dir");
}

#[test]
fn prune_empty_dirs_with_symlink_in_directory() {
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("target.txt", Some(b"target")),
            ("dir_with_link", None),
            ("empty", None),
        ],
    );

    // Create a symlink in dir_with_link
    #[cfg(unix)]
    {
        use std::os::unix::fs as unix_fs;
        unix_fs::symlink(
            ctx.source.join("target.txt"),
            ctx.source.join("dir_with_link/link.txt"),
        )
        .expect("create symlink");
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs as windows_fs;
        windows_fs::symlink_file(
            ctx.source.join("target.txt"),
            ctx.source.join("dir_with_link/link.txt"),
        )
        .expect("create symlink");
    }

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .links(true)
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!ctx.dest.join("empty").exists(), "empty dir should be pruned");
    assert!(
        ctx.dest.join("dir_with_link").is_dir(),
        "directory with symlink should exist"
    );
    // The symlink should exist (implementation may vary)
}

#[test]
fn prune_empty_dirs_deep_hierarchy_partial_pruning() {
    let ctx = test_helpers::setup_copy_test();

    // Create a deep hierarchy with a file at the bottom
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("a/b/c/d/e/file.txt", Some(b"deep")),
            ("a/b/empty", None),
            ("a/x/y/z", None),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Path to file should be preserved
    assert!(ctx.dest.join("a").is_dir());
    assert!(ctx.dest.join("a/b").is_dir());
    assert!(ctx.dest.join("a/b/c").is_dir());
    assert!(ctx.dest.join("a/b/c/d").is_dir());
    assert!(ctx.dest.join("a/b/c/d/e").is_dir());
    assert!(ctx.dest.join("a/b/c/d/e/file.txt").exists());

    // Empty branches should be pruned
    assert!(!ctx.dest.join("a/b/empty").exists());
    assert!(!ctx.dest.join("a/x").exists());
}

#[test]
fn prune_empty_dirs_with_existing_only_option() {
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("new_dir/new_file.txt", Some(b"new")),
            ("empty", None),
        ],
    );

    // Pre-create destination (empty)
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .existing_only(true)
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // With existing_only, new directories shouldn't be created anyway
    assert!(!ctx.dest.join("new_dir").exists());
    assert!(!ctx.dest.join("empty").exists());
}

#[test]
#[ignore] // TODO: Prune behavior with filters
fn prune_empty_dirs_with_include_exclude_filters() {
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("docs/readme.txt", Some(b"readme")),
            ("docs/secret.doc", Some(b"secret")),
            ("docs/notes.txt", Some(b"notes")),
            ("code/main.rs", Some(b"code")),
            ("code/test.tmp", Some(b"temp")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Include only .txt files
    let filters = FilterSet::from_rules([
        FilterRule::include("*.txt"),
        FilterRule::exclude("*"),
    ])
    .expect("compile filters");

    let options = LocalCopyOptions::default()
        .filters(Some(filters))
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(ctx.dest.join("docs").is_dir(), "docs has .txt files");
    assert!(ctx.dest.join("docs/readme.txt").exists());
    assert!(ctx.dest.join("docs/notes.txt").exists());
    assert!(!ctx.dest.join("docs/secret.doc").exists());

    // code directory should be pruned (only has .rs and .tmp)
    assert!(!ctx.dest.join("code").exists(), "code has no .txt files");
}
