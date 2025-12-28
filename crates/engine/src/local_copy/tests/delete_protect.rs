
#[test]
fn delete_respects_protect_filters() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"keep").expect("write keep");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([filters::FilterRule::protect("keep.txt")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert_eq!(summary.items_deleted(), 0);
}

#[test]
fn delete_risk_rule_overrides_protection() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"keep").expect("write keep");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([
        filters::FilterRule::protect("keep.txt"),
        filters::FilterRule::risk("keep.txt"),
    ])
    .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(!target_root.join("keep.txt").exists());
    assert_eq!(summary.items_deleted(), 1);
}
