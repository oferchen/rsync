
#[test]
fn delete_respects_exclude_filters() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");
    fs::write(target_root.join("extra.txt"), b"extra").expect("write extra");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("extra.txt").exists());
    let skip_path = target_root.join("skip.tmp");
    assert!(skip_path.exists());
    assert_eq!(fs::read(skip_path).expect("read skip"), b"dest skip");
    assert!(summary.files_copied() >= 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn delete_excluded_removes_excluded_entries() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_excluded(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("skip.tmp").exists());
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn delete_excluded_removes_matching_source_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source.join("skip.tmp"), b"skip source").expect("write skip source");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_excluded(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("skip.tmp").exists());
    assert_eq!(summary.items_deleted(), 1);
}
