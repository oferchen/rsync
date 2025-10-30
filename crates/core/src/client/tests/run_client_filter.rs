use super::prelude::*;


#[test]
fn run_client_filter_clear_resets_previous_rules() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .extend_filter_rules([
            FilterRuleSpec::exclude("*.tmp".to_string()),
            FilterRuleSpec::clear(),
            FilterRuleSpec::exclude("keep.txt".to_string()),
        ])
        .build();

    let summary = run_client(config).expect("copy succeeds");

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("skip.tmp").exists());
    assert!(!copied_root.join("keep.txt").exists());
    assert!(summary.files_copied() >= 1);
}

