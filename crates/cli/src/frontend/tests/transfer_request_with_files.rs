use super::common::*;
use super::*;

#[test]
fn transfer_request_with_files_from_copies_listed_sources() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    std::fs::create_dir(&source_dir).expect("create source dir");
    std::fs::write(source_dir.join("files-from-a.txt"), b"files-from-a")
        .expect("write source a");
    std::fs::write(source_dir.join("files-from-b.txt"), b"files-from-b")
        .expect("write source b");

    let list_path = tmp.path().join("files-from.list");
    std::fs::write(&list_path, "files-from-a.txt\nfiles-from-b.txt\n").expect("write list");

    let dest_dir = tmp.path().join("files-from-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_a = dest_dir.join("files-from-a.txt");
    let copied_b = dest_dir.join("files-from-b.txt");
    assert_eq!(
        std::fs::read(&copied_a).expect("read copied a"),
        b"files-from-a"
    );
    assert_eq!(
        std::fs::read(&copied_b).expect("read copied b"),
        b"files-from-b"
    );
}

#[test]
fn transfer_request_with_files_from_skips_comment_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    std::fs::create_dir(&source_dir).expect("create source dir");
    std::fs::write(source_dir.join("comment-a.txt"), b"comment-a").expect("write source a");
    std::fs::write(source_dir.join("comment-b.txt"), b"comment-b").expect("write source b");

    let list_path = tmp.path().join("files-from.list");
    let contents = "# leading comment\n; alt comment\ncomment-a.txt\ncomment-b.txt\n";
    std::fs::write(&list_path, contents).expect("write list");

    let dest_dir = tmp.path().join("files-from-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_a = dest_dir.join("comment-a.txt");
    let copied_b = dest_dir.join("comment-b.txt");
    assert_eq!(std::fs::read(&copied_a).expect("read a"), b"comment-a");
    assert_eq!(std::fs::read(&copied_b).expect("read b"), b"comment-b");
}
