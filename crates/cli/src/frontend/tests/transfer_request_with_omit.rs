use super::common::*;
use super::*;

#[test]
fn transfer_request_with_omit_dir_times_skips_directory_timestamp() {
    use filetime::{FileTime, set_file_times};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    let source_dir = source_root.join("nested");
    let source_file = source_dir.join("file.txt");

    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::write(&source_file, b"payload").expect("write file");

    let dir_mtime = FileTime::from_unix_time(1_700_200_000, 0);
    set_file_times(&source_dir, dir_mtime, dir_mtime).expect("set dir times");
    set_file_times(&source_file, dir_mtime, dir_mtime).expect("set file times");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--omit-dir-times"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dest_dir = dest_root.join("nested");
    let dest_file = dest_dir.join("file.txt");

    let dir_metadata = std::fs::metadata(&dest_dir).expect("dest dir metadata");
    let file_metadata = std::fs::metadata(&dest_file).expect("dest file metadata");
    let dest_dir_mtime = FileTime::from_last_modification_time(&dir_metadata);
    let dest_file_mtime = FileTime::from_last_modification_time(&file_metadata);

    assert_ne!(dest_dir_mtime, dir_mtime);
    assert_eq!(dest_file_mtime, dir_mtime);
}

#[cfg(unix)]
#[test]
fn transfer_request_with_omit_link_times_skips_symlink_timestamp() {
    use filetime::{FileTime, set_file_times, set_symlink_file_times};
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source dir");

    let source_target = source_root.join("target.txt");
    let source_link = source_root.join("link.txt");
    fs::write(&source_target, b"payload").expect("write source target");
    symlink("target.txt", &source_link).expect("create symlink");

    let timestamp = FileTime::from_unix_time(1_700_300_000, 0);
    set_file_times(&source_target, timestamp, timestamp).expect("set file times");
    set_symlink_file_times(&source_link, timestamp, timestamp).expect("set symlink times");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--omit-link-times"),
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dest_target = dest_root.join("target.txt");
    let dest_link = dest_root.join("link.txt");

    let dest_target_metadata = fs::metadata(&dest_target).expect("dest target metadata");
    let dest_link_metadata = fs::symlink_metadata(&dest_link).expect("dest link metadata");
    let dest_target_mtime = FileTime::from_last_modification_time(&dest_target_metadata);
    let dest_link_mtime = FileTime::from_last_modification_time(&dest_link_metadata);

    assert_eq!(dest_target_mtime, timestamp);
    assert_ne!(dest_link_mtime, timestamp);
}
