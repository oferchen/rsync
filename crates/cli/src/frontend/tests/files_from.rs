use super::common::*;
use super::*;

#[test]
fn files_from_reports_read_failures() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let missing = tmp.path().join("missing.list");
    let dest_dir = tmp.path().join("files-from-error-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", missing.display())),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("utf8");
    assert!(rendered.contains("failed to read file list"));
}

#[cfg(unix)]
#[test]
fn files_from_preserves_non_utf8_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("binary.list");
    std::fs::write(&list_path, [b'f', b'o', 0x80, b'\n']).expect("write binary list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].as_os_str().as_bytes(), b"fo\x80");
}

#[test]
fn from0_reader_accepts_missing_trailing_separator() {
    let data = b"alpha\0beta\0gamma";
    let mut reader = BufReader::new(&data[..]);
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    assert_eq!(
        entries,
        vec![
            OsString::from("alpha"),
            OsString::from("beta"),
            OsString::from("gamma"),
        ]
    );
}
