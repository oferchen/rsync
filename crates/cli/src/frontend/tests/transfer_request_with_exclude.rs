use super::common::*;
use super::*;

#[test]
fn transfer_request_with_exclude_from_skips_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let exclude_file = tmp.path().join("filters.txt");
    std::fs::write(&exclude_file, "# comment\n\n*.tmp\n").expect("write filters");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--exclude-from"),
        exclude_file.as_os_str().to_os_string(),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_with_exclude_from_stdin_skips_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    super::set_filter_stdin_input(b"*.tmp\n".to_vec());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--exclude-from"),
        OsString::from("-"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}
