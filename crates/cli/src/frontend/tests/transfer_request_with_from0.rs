use super::common::*;
use super::*;

#[test]
fn transfer_request_with_from0_reads_null_separated_list() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    std::fs::create_dir(&source_dir).expect("create source dir");
    std::fs::write(source_dir.join("from0-a.txt"), b"from0-a").expect("write source a");
    std::fs::write(source_dir.join("from0-b.txt"), b"from0-b").expect("write source b");

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"from0-a.txt");
    bytes.push(0);
    bytes.extend_from_slice(b"from0-b.txt");
    bytes.push(0);
    let list_path = tmp.path().join("files-from0.list");
    std::fs::write(&list_path, bytes).expect("write list");

    let dest_dir = tmp.path().join("files-from0-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--from0"),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_a = dest_dir.join("from0-a.txt");
    let copied_b = dest_dir.join("from0-b.txt");
    assert_eq!(std::fs::read(&copied_a).expect("read copied a"), b"from0-a");
    assert_eq!(std::fs::read(&copied_b).expect("read copied b"), b"from0-b");
}

#[test]
fn transfer_request_with_from0_preserves_comment_prefix_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    std::fs::create_dir(&source_dir).expect("create source dir");
    std::fs::write(source_dir.join("#commented.txt"), b"from0-comment")
        .expect("write comment source");

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"#commented.txt");
    bytes.push(0);
    let list_path = tmp.path().join("files-from0-comments.list");
    std::fs::write(&list_path, bytes).expect("write list");

    let dest_dir = tmp.path().join("files-from0-comments-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--from0"),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied = dest_dir.join("#commented.txt");
    assert_eq!(
        std::fs::read(&copied).expect("read copied"),
        b"from0-comment"
    );
}
