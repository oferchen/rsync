use super::common::*;
use super::*;

#[test]
fn transfer_request_with_from0_reads_null_separated_list() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_a = tmp.path().join("from0-a.txt");
    let source_b = tmp.path().join("from0-b.txt");
    std::fs::write(&source_a, b"from0-a").expect("write source a");
    std::fs::write(&source_b, b"from0-b").expect("write source b");

    let mut bytes = Vec::new();
    bytes.extend_from_slice(source_a.display().to_string().as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(source_b.display().to_string().as_bytes());
    bytes.push(0);
    let list_path = tmp.path().join("files-from0.list");
    std::fs::write(&list_path, bytes).expect("write list");

    let dest_dir = tmp.path().join("files-from0-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--from0"),
        OsString::from(format!("--files-from={}", list_path.display())),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_a = dest_dir.join(source_a.file_name().expect("file name a"));
    let copied_b = dest_dir.join(source_b.file_name().expect("file name b"));
    assert_eq!(std::fs::read(&copied_a).expect("read copied a"), b"from0-a");
    assert_eq!(std::fs::read(&copied_b).expect("read copied b"), b"from0-b");
}

#[test]
fn transfer_request_with_from0_preserves_comment_prefix_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let comment_named = tmp.path().join("#commented.txt");
    std::fs::write(&comment_named, b"from0-comment").expect("write comment source");

    let mut bytes = Vec::new();
    bytes.extend_from_slice(comment_named.display().to_string().as_bytes());
    bytes.push(0);
    let list_path = tmp.path().join("files-from0-comments.list");
    std::fs::write(&list_path, bytes).expect("write list");

    let dest_dir = tmp.path().join("files-from0-comments-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--from0"),
        OsString::from(format!("--files-from={}", list_path.display())),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied = dest_dir.join(comment_named.file_name().expect("file name"));
    assert_eq!(
        std::fs::read(&copied).expect("read copied"),
        b"from0-comment"
    );
}
