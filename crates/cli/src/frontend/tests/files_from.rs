use super::common::*;
use super::*;

// ==================== File reading tests ====================

#[test]
fn files_from_reads_list_from_specified_file() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("file.list");

    // Create a file list with three entries
    std::fs::write(&list_path, "file1.txt\nfile2.txt\nfile3.txt\n").expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
    assert_eq!(entries[2], "file3.txt");
}

#[test]
fn files_from_reports_read_failures() {
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
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

// ==================== One filename per line tests ====================

#[test]
fn files_from_parses_one_filename_per_line() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("multiline.list");

    // Multiple filenames with various line endings
    std::fs::write(&list_path, "alpha.txt\nbeta.txt\ngamma.txt").expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "alpha.txt");
    assert_eq!(entries[1], "beta.txt");
    assert_eq!(entries[2], "gamma.txt");
}

#[test]
fn files_from_handles_crlf_line_endings() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("crlf.list");

    // Windows-style CRLF line endings
    std::fs::write(&list_path, "file1.txt\r\nfile2.txt\r\nfile3.txt\r\n")
        .expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
    assert_eq!(entries[2], "file3.txt");
}

#[test]
fn files_from_handles_mixed_line_endings() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("mixed.list");

    // Mix of LF and CRLF
    std::fs::write(&list_path, "file1.txt\nfile2.txt\r\nfile3.txt\n").expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
    assert_eq!(entries[2], "file3.txt");
}

// ==================== Comment and blank line handling tests ====================

#[test]
fn files_from_skips_hash_comments() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("comments.list");

    std::fs::write(
        &list_path,
        "# This is a comment\nfile1.txt\n# Another comment\nfile2.txt\n",
    )
    .expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
}

#[test]
fn files_from_skips_semicolon_comments() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("semicolon.list");

    std::fs::write(
        &list_path,
        "; Comment with semicolon\nfile1.txt\n; Another semicolon comment\nfile2.txt\n",
    )
    .expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
}

#[test]
fn files_from_skips_blank_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("blank.list");

    std::fs::write(&list_path, "file1.txt\n\n\nfile2.txt\n\n").expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
}

#[test]
fn files_from_handles_comments_and_blank_lines_together() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("mixed_comments.list");

    std::fs::write(
        &list_path,
        "# Header comment\n\nfile1.txt\n\n; Middle comment\n\nfile2.txt\n# Footer\n",
    )
    .expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
}

// ==================== stdin tests ====================

#[test]
fn files_from_reads_from_stdin_with_dash() {
    use std::io::Cursor;
    use std::io::Read;

    // Test using the low-level reader function
    let input = b"file1.txt\nfile2.txt\nfile3.txt\n";
    let mut reader = BufReader::new(Cursor::new(input));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, false, &mut entries).expect("read from stdin");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
    assert_eq!(entries[2], "file3.txt");
}

#[test]
fn files_from_stdin_handles_comments() {
    use std::io::Cursor;

    let input = b"# Comment\nfile1.txt\n; Another comment\nfile2.txt\n";
    let mut reader = BufReader::new(Cursor::new(input));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, false, &mut entries).expect("read from stdin");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
}

#[test]
fn files_from_stdin_handles_blank_lines() {
    use std::io::Cursor;

    let input = b"file1.txt\n\n\nfile2.txt\n";
    let mut reader = BufReader::new(Cursor::new(input));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, false, &mut entries).expect("read from stdin");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
}

// ==================== zero-terminated tests ====================

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

#[test]
fn from0_disables_comment_handling() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("from0_comments.list");

    // With --from0, # and ; should not be treated as comments
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"#notacomment");
    bytes.push(0);
    bytes.extend_from_slice(b";alsonotacomment");
    bytes.push(0);
    std::fs::write(&list_path, bytes).expect("write list");

    let entries = load_file_list_operands(&[list_path.into_os_string()], true)
        .expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "#notacomment");
    assert_eq!(entries[1], ";alsonotacomment");
}

// ==================== edge cases ====================

#[test]
fn files_from_handles_empty_file() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("empty.list");

    std::fs::write(&list_path, "").expect("write empty list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 0);
}

#[test]
fn files_from_handles_whitespace_only_file() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("whitespace.list");

    std::fs::write(&list_path, "\n\n\n").expect("write whitespace list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 0);
}
