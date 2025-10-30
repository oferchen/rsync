use super::prelude::*;


#[cfg(unix)]
#[test]
fn run_client_updates_existing_directory_metadata() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source-tree");
    let source_nested = source_dir.join("nested");
    fs::create_dir_all(&source_nested).expect("create source tree");

    let source_mode = 0o745;
    fs::set_permissions(&source_nested, PermissionsExt::from_mode(source_mode))
        .expect("set source nested permissions");
    let source_atime = FileTime::from_unix_time(1_700_030_000, 1_000_000);
    let source_mtime = FileTime::from_unix_time(1_700_040_000, 2_000_000);
    set_file_times(&source_nested, source_atime, source_mtime)
        .expect("set source nested timestamps");

    let dest_root = tmp.path().join("dest-root");
    fs::create_dir(&dest_root).expect("create dest root");
    let dest_dir = dest_root.join("source-tree");
    let dest_nested = dest_dir.join("nested");
    fs::create_dir_all(&dest_nested).expect("pre-create destination tree");

    let dest_mode = 0o711;
    fs::set_permissions(&dest_nested, PermissionsExt::from_mode(dest_mode))
        .expect("set dest nested permissions");
    let dest_atime = FileTime::from_unix_time(1_600_000_000, 0);
    let dest_mtime = FileTime::from_unix_time(1_600_100_000, 0);
    set_file_times(&dest_nested, dest_atime, dest_mtime).expect("set dest nested timestamps");

    let config = ClientConfig::builder()
        .transfer_args([source_dir.clone(), dest_root.clone()])
        .permissions(true)
        .times(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(config.preserve_times());

    let _summary = run_client(config).expect("directory copy succeeds");

    let copied_nested = dest_root.join("source-tree").join("nested");
    let copied_metadata = fs::metadata(&copied_nested).expect("dest metadata");
    assert!(copied_metadata.is_dir());
    assert_eq!(copied_metadata.permissions().mode() & 0o777, source_mode);
    let copied_atime = FileTime::from_last_access_time(&copied_metadata);
    let copied_mtime = FileTime::from_last_modification_time(&copied_metadata);
    assert_eq!(copied_atime, source_atime);
    assert_eq!(copied_mtime, source_mtime);
}

