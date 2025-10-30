use super::prelude::*;


#[test]
fn run_client_merges_directory_contents_when_trailing_separator_present() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    let file_path = nested.join("file.txt");
    fs::write(&file_path, b"contents").expect("write file");

    let dest_root = tmp.path().join("dest");
    let mut source_arg = source_root.clone().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .build();

    let summary = run_client(config).expect("directory contents copy succeeds");

    assert!(dest_root.is_dir());
    assert!(dest_root.join("nested").is_dir());
    assert_eq!(
        fs::read(dest_root.join("nested").join("file.txt")).expect("read copied"),
        b"contents"
    );
    assert!(!dest_root.join("source").exists());
    assert!(summary.files_copied() >= 1);
}

