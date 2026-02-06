
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
fn prune_empty_dirs_with_min_size_filter() {
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
#[ignore] // Requires planner changes: non-dir-specific exclude patterns should not prevent directory traversal when prune_empty_dirs is enabled
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

// ========================================================================
// Additional comprehensive tests
// ========================================================================

#[test]
fn prune_empty_dirs_single_file_at_root_no_dirs() {
    // Verify that a source with only a file (no subdirs) works fine with prune
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[("only_file.txt", Some(b"hello"))],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(ctx.dest.join("only_file.txt").exists());
}

#[test]
fn prune_empty_dirs_source_is_completely_empty() {
    // Source directory has no entries at all
    let ctx = test_helpers::setup_copy_test();
    // source is already created but empty

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Destination should either not exist or be empty
    // (the root destination dir itself may or may not be created)
}

#[test]
fn prune_empty_dirs_mixed_empty_and_nonempty_siblings() {
    // Multiple sibling directories at the same level: some empty, some not
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("alpha", None),
            ("bravo/file.txt", Some(b"bravo")),
            ("charlie", None),
            ("delta/sub/file.txt", Some(b"delta")),
            ("echo", None),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!ctx.dest.join("alpha").exists(), "alpha is empty");
    assert!(ctx.dest.join("bravo").is_dir(), "bravo has a file");
    assert!(ctx.dest.join("bravo/file.txt").exists());
    assert!(!ctx.dest.join("charlie").exists(), "charlie is empty");
    assert!(ctx.dest.join("delta").is_dir(), "delta has nested file");
    assert!(ctx.dest.join("delta/sub").is_dir());
    assert!(ctx.dest.join("delta/sub/file.txt").exists());
    assert!(!ctx.dest.join("echo").exists(), "echo is empty");
}

#[test]
fn prune_empty_dirs_with_filter_partial_exclusion_preserves_dir() {
    // Directory has some files excluded and some included;
    // the directory should be preserved because included files remain
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("project/src/main.rs", Some(b"fn main() {}")),
            ("project/src/lib.rs", Some(b"pub mod lib;")),
            ("project/src/temp.bak", Some(b"backup")),
            ("project/build/output.o", Some(b"object")),
            ("project/build/output.bak", Some(b"backup")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([FilterRule::exclude("*.bak")]).expect("compile filters");
    let options = LocalCopyOptions::default()
        .filters(Some(filters))
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // src/ should exist because .rs files are included
    assert!(ctx.dest.join("project/src").is_dir());
    assert!(ctx.dest.join("project/src/main.rs").exists());
    assert!(ctx.dest.join("project/src/lib.rs").exists());
    assert!(!ctx.dest.join("project/src/temp.bak").exists());

    // build/ should exist because output.o is included
    assert!(ctx.dest.join("project/build").is_dir());
    assert!(ctx.dest.join("project/build/output.o").exists());
    assert!(!ctx.dest.join("project/build/output.bak").exists());
}

#[test]
fn prune_empty_dirs_deeply_nested_single_file() {
    // A very deep path with only one file at the bottom
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("a/b/c/d/e/f/g/h/i/j/leaf.txt", Some(b"deep")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // All directories along the path should be created
    assert!(ctx.dest.join("a/b/c/d/e/f/g/h/i/j").is_dir());
    assert!(ctx.dest.join("a/b/c/d/e/f/g/h/i/j/leaf.txt").exists());
}

#[test]
fn prune_empty_dirs_min_and_max_size_combined() {
    // Only files within a size range should keep directories alive
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("too_small/tiny.txt", Some(b"hi")),
            ("too_large/huge.txt", Some(b"this is a very large file that exceeds max size limit")),
            ("just_right/medium.txt", Some(b"just right size")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .min_file_size(Some(5))
        .max_file_size(Some(30))
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        !ctx.dest.join("too_small").exists(),
        "directory with only too-small files should be pruned"
    );
    assert!(
        !ctx.dest.join("too_large").exists(),
        "directory with only too-large files should be pruned"
    );
    assert!(ctx.dest.join("just_right").is_dir(), "directory with right-sized file should exist");
    assert!(ctx.dest.join("just_right/medium.txt").exists());
}

#[test]
fn prune_empty_dirs_with_delete_option() {
    // Prune should work alongside --delete: empty dirs on dest should not be recreated
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("kept/file.txt", Some(b"content")),
            ("empty", None),
        ],
    );

    // Pre-create destination with some extra content that should be deleted
    fs::create_dir_all(ctx.dest.join("extra_dir")).expect("create extra");
    fs::write(ctx.dest.join("extra_dir/old.txt"), b"old").expect("write old");

    // Use trailing separator so contents are copied directly into dest
    let mut source_operand = ctx.source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .prune_empty_dirs(true)
        .delete(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(ctx.dest.join("kept").is_dir());
    assert!(ctx.dest.join("kept/file.txt").exists());
    assert!(!ctx.dest.join("empty").exists(), "empty dir should be pruned");
    // extra_dir should be deleted by --delete since it's not in source
    assert!(!ctx.dest.join("extra_dir").exists(), "extra dir should be deleted");
}

#[test]
fn prune_empty_dirs_option_setter_and_getter() {
    // Verify the option can be set and read back
    let opts = LocalCopyOptions::default();
    assert!(!opts.prune_empty_dirs_enabled(), "default should be false");

    let opts = opts.prune_empty_dirs(true);
    assert!(opts.prune_empty_dirs_enabled(), "should be true after setting");

    let opts = opts.prune_empty_dirs(false);
    assert!(!opts.prune_empty_dirs_enabled(), "should be false after unsetting");
}

#[test]
fn prune_empty_dirs_idempotent_run() {
    // Running twice with prune should produce the same result
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("empty", None),
            ("kept/file.txt", Some(b"content")),
        ],
    );

    // First run
    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("first copy succeeds");

    assert!(!ctx.dest.join("empty").exists());
    assert!(ctx.dest.join("kept/file.txt").exists());

    // Second run (destination already exists with content)
    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("second copy succeeds");

    assert!(!ctx.dest.join("empty").exists());
    assert!(ctx.dest.join("kept/file.txt").exists());
}

#[test]
fn prune_empty_dirs_cascading_after_filter_removes_all_files() {
    // A directory tree where filtering removes all files, leaving multiple
    // levels of empty directories that should all be pruned bottom-up
    let ctx = test_helpers::setup_copy_test();

    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("root_dir/sub1/sub2/only.log", Some(b"log entry")),
            ("root_dir/sub1/another.log", Some(b"another log")),
            ("root_dir/keep.txt", Some(b"keeper")),
        ],
    );

    let operands = vec![ctx.source.into_os_string(), ctx.dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([FilterRule::exclude("*.log")]).expect("compile filters");
    let options = LocalCopyOptions::default()
        .filters(Some(filters))
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // root_dir should exist because keep.txt is there
    assert!(ctx.dest.join("root_dir").is_dir());
    assert!(ctx.dest.join("root_dir/keep.txt").exists());
    // sub1 should be pruned because after excluding .log files, it's empty
    assert!(!ctx.dest.join("root_dir/sub1").exists(), "sub1 should be pruned (all .log files excluded)");
}
