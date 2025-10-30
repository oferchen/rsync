use super::prelude::*;


#[test]
fn run_client_respects_filter_rules() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .extend_filter_rules([FilterRuleSpec::exclude("*.tmp".to_string())])
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert!(dest_root.join("source").join("keep.txt").exists());
    assert!(!dest_root.join("source").join("skip.tmp").exists());
    assert!(summary.files_copied() >= 1);
}

