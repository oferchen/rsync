use super::common::*;
use super::*;

#[test]
fn backup_flag_creates_default_suffix_backups() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    std::fs::write(&source_file, b"new data").expect("write source");

    let dest_root = dest_dir.join("source");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(dest_root.join("file.txt"), b"old data").expect("seed dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--backup"),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let target_root = dest_dir.join("source");
    assert_eq!(
        std::fs::read(target_root.join("file.txt")).expect("read dest"),
        b"new data"
    );
    assert_eq!(
        std::fs::read(target_root.join("file.txt~")).expect("read backup"),
        b"old data"
    );
}

#[test]
fn backup_dir_flag_places_backups_in_relative_directory() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(source_dir.join("nested")).expect("create nested source");
    std::fs::create_dir_all(dest_dir.join("source/nested")).expect("create nested dest");

    let source_file = source_dir.join("nested/file.txt");
    std::fs::write(&source_file, b"updated").expect("write source");
    std::fs::write(dest_dir.join("source/nested/file.txt"), b"previous").expect("seed dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--backup-dir"),
        OsString::from("backups"),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let backup_path = dest_dir.join("backups/source/nested/file.txt~");
    assert_eq!(
        std::fs::read(&backup_path).expect("read backup"),
        b"previous"
    );
    assert_eq!(
        std::fs::read(dest_dir.join("source/nested/file.txt")).expect("read dest"),
        b"updated"
    );
}

#[test]
fn backup_suffix_flag_overrides_default_suffix() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    std::fs::write(&source_file, b"fresh").expect("write source");
    let dest_root = dest_dir.join("source");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(dest_root.join("file.txt"), b"stale").expect("seed dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--suffix"),
        OsString::from(".bak"),
        source_dir.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    assert_eq!(
        std::fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"fresh"
    );
    let backup_path = dest_root.join("file.txt.bak");
    assert_eq!(std::fs::read(&backup_path).expect("read backup"), b"stale");
}
