use super::prelude::*;


#[test]
fn builder_collects_transfer_arguments() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("source"), OsString::from("dest")])
        .build();

    assert_eq!(
        config.transfer_args(),
        &[OsString::from("source"), OsString::from("dest")]
    );
    assert!(config.has_transfer_request());
    assert!(!config.dry_run());
}


#[test]
fn builder_collects_reference_directories() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compare_destination(PathBuf::from("compare"))
        .copy_destination(PathBuf::from("copy"))
        .link_destination(PathBuf::from("link"))
        .build();

    let references = config.reference_directories();
    assert_eq!(references.len(), 3);
    assert_eq!(references[0].kind(), ReferenceDirectoryKind::Compare);
    assert_eq!(references[1].kind(), ReferenceDirectoryKind::Copy);
    assert_eq!(references[2].kind(), ReferenceDirectoryKind::Link);
    assert_eq!(references[0].path(), PathBuf::from("compare").as_path());
}

