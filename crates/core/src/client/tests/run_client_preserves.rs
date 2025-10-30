use super::prelude::*;


#[cfg(unix)]
#[test]
fn run_client_preserves_symbolic_links_in_directories() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");

    let target_file = tmp.path().join("target.txt");
    fs::write(&target_file, b"data").expect("write target");
    let link_path = nested.join("link");
    symlink(&target_file, &link_path).expect("create link");

    let dest_root = tmp.path().join("destination");
    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .force_event_collection(true)
        .build();

    let summary = run_client(config).expect("directory copy succeeds");

    let copied_link = dest_root.join("nested").join("link");
    let copied_target = fs::read_link(copied_link).expect("read copied link");
    assert_eq!(copied_target, target_file);
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


#[cfg(unix)]
#[test]
fn run_client_preserves_file_metadata() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-metadata.txt");
    let destination = tmp.path().join("dest-metadata.txt");
    fs::write(&source, b"metadata").expect("write source");

    let mode = 0o640;
    fs::set_permissions(&source, PermissionsExt::from_mode(mode)).expect("set source permissions");
    let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set source timestamps");

    let source_metadata = fs::metadata(&source).expect("source metadata");
    assert_eq!(source_metadata.permissions().mode() & 0o777, mode);
    let src_atime = FileTime::from_last_access_time(&source_metadata);
    let src_mtime = FileTime::from_last_modification_time(&source_metadata);
    assert_eq!(src_atime, atime);
    assert_eq!(src_mtime, mtime);

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .permissions(true)
        .times(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.files_copied(), 1);
}


#[cfg(unix)]
#[test]
fn run_client_preserves_directory_metadata() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source-dir");
    fs::create_dir(&source_dir).expect("create source dir");

    let mode = 0o751;
    fs::set_permissions(&source_dir, PermissionsExt::from_mode(mode))
        .expect("set directory permissions");
    let atime = FileTime::from_unix_time(1_700_010_000, 0);
    let mtime = FileTime::from_unix_time(1_700_020_000, 789_000_000);
    set_file_times(&source_dir, atime, mtime).expect("set directory timestamps");

    let destination_dir = tmp.path().join("dest-dir");
    let config = ClientConfig::builder()
        .transfer_args([source_dir.clone(), destination_dir.clone()])
        .permissions(true)
        .times(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(config.preserve_times());

    let summary = run_client(config).expect("directory copy succeeds");

    let dest_metadata = fs::metadata(&destination_dir).expect("dest metadata");
    assert!(dest_metadata.is_dir());
    assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert!(summary.directories_created() >= 1);
}

