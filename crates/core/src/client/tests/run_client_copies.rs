use super::prelude::*;


#[test]
fn run_client_copies_single_file() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"example").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .permissions(true)
        .times(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(config.preserve_times());

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), b"example");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), b"example".len() as u64);
    assert!(!summary.compression_used());
    assert!(summary.compressed_bytes().is_none());
}


#[test]
fn run_client_copies_directory_tree() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let nested = source_root.join("nested");
    let source_file = nested.join("file.txt");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(&source_file, b"tree").expect("write source file");

    let dest_root = tmp.path().join("destination");

    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .build();

    let summary = run_client(config).expect("directory copy succeeds");

    let copied_file = dest_root.join("nested").join("file.txt");
    assert_eq!(fs::read(copied_file).expect("read copied"), b"tree");
    assert!(summary.files_copied() >= 1);
    assert!(summary.directories_created() >= 1);
}


#[cfg(unix)]
#[test]
fn run_client_copies_symbolic_link() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let target_file = tmp.path().join("target.txt");
    fs::write(&target_file, b"symlink target").expect("write target");

    let source_link = tmp.path().join("source-link");
    symlink(&target_file, &source_link).expect("create source symlink");

    let destination_link = tmp.path().join("dest-link");
    let config = ClientConfig::builder()
        .transfer_args([source_link.clone(), destination_link.clone()])
        .force_event_collection(true)
        .build();

    let summary = run_client(config).expect("link copy succeeds");

    let copied = fs::read_link(destination_link).expect("read copied link");
    assert_eq!(copied, target_file);
    assert_eq!(summary.symlinks_copied(), 1);

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::SymlinkCopied))
        .expect("symlink event present");
    let recorded_target = event
        .metadata()
        .and_then(ClientEntryMetadata::symlink_target)
        .expect("symlink target recorded");
    assert_eq!(recorded_target, target_file.as_path());
}

