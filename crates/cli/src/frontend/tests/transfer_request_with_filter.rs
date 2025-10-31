use super::common::*;
use super::*;

#[test]
fn transfer_request_with_filter_excludes_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        OsString::from("- *.tmp"),
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
fn transfer_request_with_filter_clear_resets_rules() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        OsString::from("- *.tmp"),
        OsString::from("--filter"),
        OsString::from("!"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(copied_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_with_filter_merge_applies_rules() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let filter_file = tmp.path().join("filters.txt");
    std::fs::write(&filter_file, "- *.tmp\n").expect("write filter file");

    let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        filter_arg,
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
fn transfer_request_with_filter_merge_clear_resets_rules() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write tmp");
    std::fs::write(source_root.join("skip.log"), b"log").expect("write log");

    let filter_file = tmp.path().join("filters.txt");
    std::fs::write(&filter_file, "- *.tmp\n!\n- *.log\n").expect("write filter file");

    let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        filter_arg,
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(copied_root.join("skip.tmp").exists());
    assert!(!copied_root.join("skip.log").exists());
}

#[test]
fn transfer_request_with_filter_protect_preserves_destination_entry() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");

    let dest_subdir = dest_root.join("source");
    std::fs::create_dir_all(&dest_subdir).expect("create destination contents");
    std::fs::write(dest_subdir.join("keep.txt"), b"keep").expect("write dest keep");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete"),
        OsString::from("--filter"),
        OsString::from("protect keep.txt"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
}

#[test]
fn transfer_request_with_filter_merge_detects_recursion() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");

    let filter_file = tmp.path().join("filters.txt");
    std::fs::write(&filter_file, format!("merge {}\n", filter_file.display()))
        .expect("write recursive filter");

    let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        filter_arg,
        source_root.into_os_string(),
        dest_root.into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8_lossy(&stderr);
    assert!(rendered.contains("recursive filter merge"));
}
