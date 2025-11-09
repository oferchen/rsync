
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
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
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
    let filters_without = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
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
    let filters_with = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
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
        source_root.clone().into_os_string(),
        destination_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .min_file_size(Some(10))
        .prune_empty_dirs(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = destination_root.join("source");
    assert!(target_root.exists());
    assert!(target_root.join("nested").is_dir());
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
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([
        rsync_filters::FilterRule::exclude("*.tmp"),
        rsync_filters::FilterRule::include("keep.tmp"),
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
        source_root.clone().into_os_string(),
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
        source_root.clone().into_os_string(),
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
        source_root.clone().into_os_string(),
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
    let entries =
        load_dir_merge_rules_recursive(&filter, &options, &mut visited).expect("parse filter");

    assert_eq!(entries.rules.len(), 1);
    assert!(entries.rules.iter().any(|rule| {
        rule.pattern() == "skip" && matches!(rule.action(), rsync_filters::FilterAction::Exclude)
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
    let entries =
        load_dir_merge_rules_recursive(&filter, &options, &mut visited).expect("parse filter");

    assert_eq!(entries.rules.len(), 1);
    assert!(entries.exclude_if_present.is_empty());
}
