use super::prelude::*;


#[test]
fn run_client_dry_run_skips_copy() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"dry-run").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .dry_run(true)
        .build();

    let summary = run_client(config).expect("dry-run succeeds");

    assert!(!destination.exists());
    assert_eq!(summary.files_copied(), 1);
}

