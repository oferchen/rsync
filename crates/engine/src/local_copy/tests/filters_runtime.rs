
#[test]
fn execute_respects_exclude_filter() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source.join("skip.tmp"), b"skip").expect("write skip");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default().filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("skip.tmp").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn execute_prunes_empty_directories_when_enabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest_without_prune = temp.path().join("dest_without_prune");
    let dest_with_prune = temp.path().join("dest_with_prune");

    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(source.join("empty")).expect("create empty dir");
    fs::write(source.join("keep.txt"), b"payload").expect("write keep");
    fs::write(source.join("empty").join("skip.tmp"), b"skip").expect("write skip");

    fs::create_dir_all(&dest_without_prune).expect("create dest");
    fs::create_dir_all(&dest_with_prune).expect("create dest");

    let operands_without = vec![
        source.clone().into_os_string(),
        dest_without_prune.clone().into_os_string(),
    ];
    let plan_without = LocalCopyPlan::from_operands(&operands_without).expect("plan");
    let filters_without = FilterSet::from_rules([filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options_without = LocalCopyOptions::default().filters(Some(filters_without));
    let summary_without = plan_without
        .execute_with_options(LocalCopyExecution::Apply, options_without)
        .expect("copy succeeds");

    let operands_with = vec![
        source.into_os_string(),
        dest_with_prune.clone().into_os_string(),
    ];
    let plan_with = LocalCopyPlan::from_operands(&operands_with).expect("plan");
    let filters_with = FilterSet::from_rules([filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options_with = LocalCopyOptions::default()
        .filters(Some(filters_with))
        .prune_empty_dirs(true);
    let summary_with = plan_with
        .execute_with_options(LocalCopyExecution::Apply, options_with)
        .expect("copy succeeds");

    let target_without = dest_without_prune.join("source");
    let target_with = dest_with_prune.join("source");

    assert!(target_without.join("keep.txt").exists());
    assert!(target_with.join("keep.txt").exists());
    assert!(target_without.join("empty").is_dir());
    assert!(!target_with.join("empty").exists());
    assert!(summary_with.directories_created() < summary_without.directories_created());
}

#[test]
fn execute_prunes_empty_directories_with_size_filters() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    let nested = source_root.join("nested");

    fs::create_dir_all(&nested).expect("create nested source");
    fs::create_dir_all(&destination_root).expect("create destination root");
    fs::write(nested.join("tiny.bin"), b"x").expect("write small file");

    let operands = vec![
        source_root.into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .min_file_size(Some(10))
        .prune_empty_dirs(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // When prune_empty_dirs is enabled and all files are below min_file_size,
    // the directories should be pruned because no files were actually kept.
    // This matches upstream rsync behavior: --prune-empty-dirs removes
    // directories that end up empty after size-based filtering.
    let target_root = destination_root.join("source");
    assert!(
        !target_root.join("nested").exists(),
        "nested dir should be pruned (all files below min-size)"
    );
    assert!(!target_root.join("nested").join("tiny.bin").exists());
    assert_eq!(summary.files_copied(), 0);
}

#[test]
fn execute_respects_include_filter_override() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.tmp"), b"keep").expect("write keep");
    fs::write(source.join("skip.tmp"), b"skip").expect("write skip");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // With first-match-wins, specific include must come before general exclude
    let filters = FilterSet::from_rules([
        filters::FilterRule::include("keep.tmp"),
        filters::FilterRule::exclude("*.tmp"),
    ])
    .expect("compile filters");
    let options = LocalCopyOptions::default().filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.tmp").exists());
    assert!(!target_root.join("skip.tmp").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn execute_skips_directories_with_exclude_if_present_marker() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&destination_root).expect("create dest root");

    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    let marker_dir = source_root.join("skip");
    fs::create_dir_all(&marker_dir).expect("create marker dir");
    fs::write(marker_dir.join(".rsyncignore"), b"marker").expect("write marker");
    fs::write(marker_dir.join("data.txt"), b"ignored").expect("write data");

    let operands = vec![
        source_root.into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let program = FilterProgram::new([FilterProgramEntry::ExcludeIfPresent(
        ExcludeIfPresentRule::new(".rsyncignore"),
    )])
    .expect("compile filter program");

    let options = LocalCopyOptions::default().with_filter_program(Some(program));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = destination_root.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("skip").exists());
}

#[test]
fn dir_merge_exclude_if_present_from_filter_file() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&destination_root).expect("create dest");

    fs::write(
        source_root.join(".rsync-filter"),
        b"exclude-if-present=.git\n",
    )
    .expect("write filter");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

    let project = source_root.join("project");
    fs::create_dir_all(&project).expect("create project");
    fs::write(project.join(".git"), b"marker").expect("write marker");
    fs::write(project.join("data.txt"), b"ignored").expect("write data");

    let operands = vec![
        source_root.into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        PathBuf::from(".rsync-filter"),
        DirMergeOptions::default(),
    ))])
    .expect("compile filter program");

    let options = LocalCopyOptions::default().with_filter_program(Some(program));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = destination_root.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("project").exists());
}

#[test]
fn filter_program_clear_discards_previous_rules() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&destination_root).expect("create dest");

    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source_root.join("skip.tmp"), b"tmp").expect("write tmp");
    fs::write(source_root.join("skip.bak"), b"bak").expect("write bak");

    let operands = vec![
        source_root.into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::exclude("*.tmp")),
        FilterProgramEntry::Clear,
        FilterProgramEntry::Rule(FilterRule::exclude("*.bak")),
    ])
    .expect("compile filter program");

    let options = LocalCopyOptions::default().with_filter_program(Some(program));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = destination_root.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(
        target_root.join("skip.tmp").exists(),
        "list-clearing rule should discard earlier excludes"
    );
    assert!(!target_root.join("skip.bak").exists());
}

#[test]
fn dir_merge_clear_keyword_discards_previous_rules() {
    let temp = tempdir().expect("tempdir");
    let filter = temp.path().join("filter.rules");
    fs::write(&filter, b"exclude-if-present=.git\nclear\n- skip\n").expect("write filter");

    let mut visited = Vec::new();
    let options = DirMergeOptions::default();
    let entries = load_dir_merge_rules_recursive(&filter, &options, false, &mut visited)
        .expect("parse filter");

    assert_eq!(entries.rules.len(), 1);
    assert!(entries.rules.iter().any(|rule| {
        rule.pattern() == "skip" && matches!(rule.action(), filters::FilterAction::Exclude)
    }));
    assert!(entries.exclude_if_present.is_empty());
}

#[test]
fn dir_merge_clear_keyword_discards_rules_in_whitespace_mode() {
    let temp = tempdir().expect("tempdir");
    let filter = temp.path().join("filter.rules");
    fs::write(&filter, b"exclude-if-present=.git clear -skip").expect("write filter");

    let mut visited = Vec::new();
    let options = DirMergeOptions::default()
        .use_whitespace()
        .allow_list_clearing(true);
    let entries = load_dir_merge_rules_recursive(&filter, &options, false, &mut visited)
        .expect("parse filter");

    assert_eq!(entries.rules.len(), 1);
    assert!(entries.exclude_if_present.is_empty());
}

/// Nested `dir-merge` inside a per-directory merge file should register the
/// referenced filename for lookup in each visited subdirectory, NOT load it
/// eagerly against the enclosing file's directory.
///
/// upstream: exclude.c:1419-1428 - mirrors the testsuite `exclude-lsh.test`
/// fixture where `bar/.filt` declares `dir-merge .filt2`, and the actual
/// `.filt2` exclusion rules live in subdirectories like `bar/baz/.filt2`.
#[test]
fn nested_dir_merge_registers_per_directory_rule() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&destination_root).expect("create dest");

    // bar/.filt declares a nested per-directory merge. Without the fix this
    // tries to load bar/.filt2 (which does not exist) instead of registering
    // .filt2 for subdirectory lookup.
    let bar = source_root.join("bar");
    fs::create_dir_all(&bar).expect("create bar");
    fs::write(bar.join(".filt"), b"dir-merge .filt2\n").expect("write bar/.filt");

    // bar/baz holds the per-subdir filter that the dir-merge should resolve.
    let baz = bar.join("baz");
    fs::create_dir_all(&baz).expect("create baz");
    fs::write(baz.join(".filt2"), b"- *.deep\n").expect("write baz/.filt2");
    fs::write(baz.join("file5.deep"), b"filtout").expect("write file5.deep");
    fs::write(baz.join("keep.txt"), b"keep").expect("write keep");

    let operands = vec![
        source_root.into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        PathBuf::from(".filt"),
        DirMergeOptions::default(),
    ))])
    .expect("compile filter program");

    let options = LocalCopyOptions::default().with_filter_program(Some(program));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = destination_root.join("source");
    let copied_baz = target_root.join("bar").join("baz");
    assert!(
        copied_baz.join("keep.txt").exists(),
        "unfiltered file should be copied"
    );
    assert!(
        !copied_baz.join("file5.deep").exists(),
        "nested dir-merge from bar/.filt should register .filt2 lookup in subdirectories and exclude bar/baz/file5.deep"
    );
}

/// Builds the bug #273 repro tree under `root` and returns the trailing-slash
/// source operand plus the destination directory. The root `.rsync-filter`
/// (protecting `*.bak`, descending all dirs) is transferred to the destination;
/// the destination is pre-seeded with an extraneous `normal.bak` at the root and
/// `sub/x.bak` one level down, neither of which appears in the source flist.
fn seed_perdir_delete_repro(root: &Path) -> (OsString, PathBuf) {
    let source = root.join("source");
    let dest = root.join("dest");
    fs::create_dir_all(source.join("sub")).expect("create source/sub");
    fs::create_dir_all(dest.join("sub")).expect("create dest/sub");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source.join("sub/keep2.txt"), b"keep2").expect("write keep2");
    // A per-dir merge rule that protects *.bak and descends every directory.
    fs::write(source.join(".rsync-filter"), b"- *.bak\n+ */\n").expect("write .rsync-filter");
    fs::write(dest.join("normal.bak"), b"bak").expect("seed dest/normal.bak");
    fs::write(dest.join("sub/x.bak"), b"bak").expect("seed dest/sub/x.bak");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    (source_operand, dest)
}

fn perdir_delete_program() -> FilterProgram {
    FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        PathBuf::from(".rsync-filter"),
        DirMergeOptions::default(),
    ))])
    .expect("compile filter program")
}

/// bug #273: with `--delete-after`, a per-dir-merge `.rsync-filter` rule
/// protects matching destination files from deletion, and the ROOT directory's
/// rule INHERITS into subdirectories - so `sub/x.bak` survives even though only
/// the root carries a `.rsync-filter`. The destination merge files are present
/// by the time the delete pass runs, so the ancestor rule is loaded and the
/// isolated delete chain carries it down the tree. Deleting `sub/x.bak` here
/// would be silent data loss.
///
/// upstream: `rsync -avF --delete-after` keeps both `normal.bak` and
/// `sub/x.bak` (delete.c:63 push_local_filters + exclude.c:801 inherited head).
#[test]
fn perdir_merge_delete_after_inherits_into_subdirs() {
    let temp = tempdir().expect("tempdir");
    let (source_operand, dest) = seed_perdir_delete_repro(temp.path());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .delete(true)
        .delete_after(true)
        .with_filter_program(Some(perdir_delete_program()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        dest.join("normal.bak").exists(),
        "--delete-after: root .rsync-filter `- *.bak` must protect normal.bak",
    );
    assert!(
        dest.join("sub/x.bak").exists(),
        "--delete-after: ancestor .rsync-filter rule must inherit and protect sub/x.bak (bug #273 data loss)",
    );
    assert!(dest.join("keep.txt").exists());
    assert!(dest.join("sub/keep2.txt").exists());
}

/// bug #273: with `--delete` (delete-DURING), the destination `.rsync-filter`
/// has not been written yet when the per-directory delete sweep runs (it arrives
/// with the transfer), so the isolated delete chain is empty and BOTH `.bak`
/// files are removed - matching upstream, which is why the manual recommends
/// `--delete-after` for per-dir-merge protection. Protecting them here (the old
/// behaviour, which leaked source-side rules into the during sweep) would
/// diverge from upstream.
///
/// upstream: `rsync -avF --delete` deletes both `normal.bak` and `sub/x.bak`
/// (rsync.1.md:4419 - the receiver has not merged the dir's rules yet).
#[test]
fn perdir_merge_delete_during_matches_upstream_no_protection() {
    let temp = tempdir().expect("tempdir");
    let (source_operand, dest) = seed_perdir_delete_repro(temp.path());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .delete(true)
        .with_filter_program(Some(perdir_delete_program()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        !dest.join("normal.bak").exists(),
        "--delete (during): normal.bak must be deleted (dest .rsync-filter not present yet)",
    );
    assert!(
        !dest.join("sub/x.bak").exists(),
        "--delete (during): sub/x.bak must be deleted (no source-side rule leak into the during sweep)",
    );
    assert!(dest.join("keep.txt").exists());
    assert!(dest.join("sub/keep2.txt").exists());
}
