use super::common::*;
use super::*;

#[test]
fn transfer_request_with_files_from_copies_listed_sources() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_a = tmp.path().join("files-from-a.txt");
    let source_b = tmp.path().join("files-from-b.txt");
    std::fs::write(&source_a, b"files-from-a").expect("write source a");
    std::fs::write(&source_b, b"files-from-b").expect("write source b");

    let list_path = tmp.path().join("files-from.list");
    let list_contents = format!("{}\n{}\n", source_a.display(), source_b.display());
    std::fs::write(&list_path, list_contents).expect("write list");

    let dest_dir = tmp.path().join("files-from-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_a = dest_dir.join(source_a.file_name().expect("file name a"));
    let copied_b = dest_dir.join(source_b.file_name().expect("file name b"));
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
    let source_a = tmp.path().join("comment-a.txt");
    let source_b = tmp.path().join("comment-b.txt");
    std::fs::write(&source_a, b"comment-a").expect("write source a");
    std::fs::write(&source_b, b"comment-b").expect("write source b");

    let list_path = tmp.path().join("files-from.list");
    let contents = format!(
        "# leading comment\n; alt comment\n{}\n{}\n",
        source_a.display(),
        source_b.display()
    );
    std::fs::write(&list_path, contents).expect("write list");

    let dest_dir = tmp.path().join("files-from-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_a = dest_dir.join(source_a.file_name().expect("file name a"));
    let copied_b = dest_dir.join(source_b.file_name().expect("file name b"));
    assert_eq!(std::fs::read(&copied_a).expect("read a"), b"comment-a");
    assert_eq!(std::fs::read(&copied_b).expect("read b"), b"comment-b");
}
