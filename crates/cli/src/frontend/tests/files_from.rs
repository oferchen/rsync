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
    std::fs::write(&list_path, "file1.txt\r\nfile2.txt\r\nfile3.txt\r\n").expect("write list");

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

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], true).expect("load entries");

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

// ==================== Multiple files-from arguments ====================

#[test]
fn files_from_reads_from_multiple_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list1 = tmp.path().join("list1.txt");
    let list2 = tmp.path().join("list2.txt");

    std::fs::write(&list1, "file1.txt\nfile2.txt\n").expect("write list1");
    std::fs::write(&list2, "file3.txt\nfile4.txt\n").expect("write list2");

    let entries = load_file_list_operands(
        &[list1.into_os_string(), list2.into_os_string()],
        false,
    )
    .expect("load entries");

    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
    assert_eq!(entries[2], "file3.txt");
    assert_eq!(entries[3], "file4.txt");
}

#[test]
fn files_from_empty_list_returns_empty_vec() {
    let entries: Vec<OsString> = Vec::new();
    let result = load_file_list_operands(&entries, false).expect("load empty");
    assert!(result.is_empty());
}

// ==================== Unicode and special characters ====================

#[test]
fn files_from_handles_unicode_filenames() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("unicode.list");

    // Various Unicode characters including CJK, emoji representations, accented chars
    std::fs::write(
        &list_path,
        "file_cafe.txt\nfile_resume.txt\nfile_chinese.txt\nfile_japanese.txt\n",
    )
    .expect("write unicode list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0], "file_cafe.txt");
    assert_eq!(entries[1], "file_resume.txt");
    assert_eq!(entries[2], "file_chinese.txt");
    assert_eq!(entries[3], "file_japanese.txt");
}

#[test]
fn files_from_handles_filenames_with_spaces() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("spaces.list");

    std::fs::write(
        &list_path,
        "file with spaces.txt\nanother file.txt\n  leading spaces.txt\ntrailing spaces  .txt\n",
    )
    .expect("write list with spaces");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0], "file with spaces.txt");
    assert_eq!(entries[1], "another file.txt");
    assert_eq!(entries[2], "  leading spaces.txt");
    assert_eq!(entries[3], "trailing spaces  .txt");
}

#[test]
fn files_from_handles_special_characters_in_filenames() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("special.list");

    // Various special characters that might appear in filenames
    std::fs::write(
        &list_path,
        "file[1].txt\nfile(2).txt\nfile{3}.txt\nfile'4'.txt\nfile\"5\".txt\n",
    )
    .expect("write special chars list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0], "file[1].txt");
    assert_eq!(entries[1], "file(2).txt");
    assert_eq!(entries[2], "file{3}.txt");
    assert_eq!(entries[3], "file'4'.txt");
    assert_eq!(entries[4], "file\"5\".txt");
}

// ==================== Path handling tests ====================

#[test]
fn files_from_preserves_absolute_paths() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("absolute.list");

    std::fs::write(&list_path, "/absolute/path/file.txt\n/another/absolute/path.txt\n")
        .expect("write absolute paths list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "/absolute/path/file.txt");
    assert_eq!(entries[1], "/another/absolute/path.txt");
}

#[test]
fn files_from_preserves_relative_paths() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("relative.list");

    std::fs::write(
        &list_path,
        "relative/path/file.txt\n./current/dir/file.txt\n../parent/file.txt\n",
    )
    .expect("write relative paths list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "relative/path/file.txt");
    assert_eq!(entries[1], "./current/dir/file.txt");
    assert_eq!(entries[2], "../parent/file.txt");
}

#[test]
fn files_from_handles_deeply_nested_paths() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("nested.list");

    std::fs::write(
        &list_path,
        "a/b/c/d/e/f/g/h/i/j/file.txt\nvery/deeply/nested/path/to/some/file.txt\n",
    )
    .expect("write nested paths list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "a/b/c/d/e/f/g/h/i/j/file.txt");
    assert_eq!(entries[1], "very/deeply/nested/path/to/some/file.txt");
}

// ==================== Zero-terminated edge cases ====================

#[test]
fn from0_handles_empty_entries() {
    use std::io::Cursor;

    // Empty entries between null separators should be skipped
    let data = b"file1.txt\0\0file2.txt\0";
    let mut reader = BufReader::new(Cursor::new(data));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
}

#[test]
fn from0_handles_only_null_bytes() {
    use std::io::Cursor;

    let data = b"\0\0\0";
    let mut reader = BufReader::new(Cursor::new(data));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    assert!(entries.is_empty());
}

#[test]
fn from0_handles_paths_with_newlines() {
    use std::io::Cursor;

    // With --from0, newlines in filenames are preserved
    let data = b"file\nwith\nnewlines.txt\0normal.txt\0";
    let mut reader = BufReader::new(Cursor::new(data));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file\nwith\nnewlines.txt");
    assert_eq!(entries[1], "normal.txt");
}

#[cfg(unix)]
#[test]
fn from0_preserves_non_utf8_in_zero_terminated() {
    use std::io::Cursor;

    let mut data = Vec::new();
    data.extend_from_slice(&[b'f', b'i', b'l', b'e', 0x80, 0x81]);
    data.push(0);
    data.extend_from_slice(b"normal.txt");
    data.push(0);

    let mut reader = BufReader::new(Cursor::new(data));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].as_os_str().as_bytes(),
        &[b'f', b'i', b'l', b'e', 0x80, 0x81]
    );
    assert_eq!(entries[1], "normal.txt");
}

// ==================== Comment edge cases ====================

#[test]
fn files_from_hash_only_on_first_char_is_comment() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("hash_position.list");

    // Only lines starting with # are comments; # elsewhere is preserved
    std::fs::write(
        &list_path,
        "file#with#hash.txt\n# this is a comment\nfile.txt#\n",
    )
    .expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file#with#hash.txt");
    assert_eq!(entries[1], "file.txt#");
}

#[test]
fn files_from_semicolon_only_on_first_char_is_comment() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("semicolon_position.list");

    // Only lines starting with ; are comments
    std::fs::write(
        &list_path,
        "file;with;semicolons.txt\n; this is a comment\nfile.txt;\n",
    )
    .expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file;with;semicolons.txt");
    assert_eq!(entries[1], "file.txt;");
}

#[test]
fn files_from_comments_only_file_returns_empty() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("comments_only.list");

    std::fs::write(
        &list_path,
        "# comment 1\n; comment 2\n# comment 3\n; comment 4\n",
    )
    .expect("write comments only");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert!(entries.is_empty());
}

// ==================== Reader function edge cases ====================

#[test]
fn reader_handles_very_long_lines() {
    use std::io::Cursor;

    // Create a very long filename (4000 chars)
    let long_name: String = "x".repeat(4000);
    let input = format!("{}\nshort.txt\n", long_name);

    let mut reader = BufReader::new(Cursor::new(input.as_bytes()));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, false, &mut entries).expect("read long lines");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].to_string_lossy().len(), 4000);
    assert_eq!(entries[1], "short.txt");
}

#[test]
fn reader_handles_single_entry_no_newline() {
    use std::io::Cursor;

    let input = b"single_file.txt";
    let mut reader = BufReader::new(Cursor::new(input));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, false, &mut entries).expect("read single entry");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], "single_file.txt");
}

#[test]
fn reader_handles_trailing_carriage_returns() {
    use std::io::Cursor;

    // Multiple trailing CRs should all be stripped
    let input = b"file.txt\r\r\r\n";
    let mut reader = BufReader::new(Cursor::new(input));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, false, &mut entries).expect("read with CRs");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], "file.txt");
}

// ==================== Integration tests with actual file operations ====================

#[test]
fn files_from_integration_copies_listed_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");

    // Create source files
    std::fs::write(source_dir.join("file1.txt"), b"content1").expect("write file1");
    std::fs::write(source_dir.join("file2.txt"), b"content2").expect("write file2");
    std::fs::write(source_dir.join("file3.txt"), b"content3").expect("write file3");

    // Create file list (only file1 and file3)
    let list_path = tmp.path().join("files.list");
    std::fs::write(&list_path, "file1.txt\nfile3.txt\n").expect("write list");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    // Verify only listed files were copied
    assert!(dest_dir.join("file1.txt").exists());
    assert!(!dest_dir.join("file2.txt").exists());
    assert!(dest_dir.join("file3.txt").exists());
}

#[test]
fn files_from_integration_handles_nested_directories() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");
    let nested = source_dir.join("subdir");
    std::fs::create_dir(&nested).expect("create nested");

    std::fs::write(source_dir.join("top.txt"), b"top content").expect("write top");
    std::fs::write(nested.join("nested.txt"), b"nested content").expect("write nested");

    let list_path = tmp.path().join("nested.list");
    std::fs::write(&list_path, "top.txt\nsubdir/nested.txt\n").expect("write list");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(stdout.is_empty());

    // Both files should be copied (nested.txt goes to dest/nested.txt per rsync behavior)
    assert!(dest_dir.join("top.txt").exists());
    assert!(dest_dir.join("nested.txt").exists());
}

#[test]
fn files_from_integration_with_empty_list_succeeds() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");
    std::fs::write(source_dir.join("file.txt"), b"content").expect("write file");

    let list_path = tmp.path().join("empty.list");
    std::fs::write(&list_path, "").expect("write empty list");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    // Should succeed with empty list (nothing to copy)
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(!dest_dir.join("file.txt").exists());
}

#[test]
fn files_from_integration_with_comments_only_list_succeeds() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");
    std::fs::write(source_dir.join("file.txt"), b"content").expect("write file");

    let list_path = tmp.path().join("comments.list");
    std::fs::write(&list_path, "# Just comments\n; Nothing else\n").expect("write comments list");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    // Should succeed (no files to copy due to all comments)
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(!dest_dir.join("file.txt").exists());
}

// ==================== CLI argument parsing tests ====================

#[test]
fn parse_args_recognizes_files_from_with_equals() {
    use crate::frontend::arguments::parse_args;

    let parsed = parse_args(["rsync", "--files-from=/path/to/list.txt", "src/", "dst/"])
        .expect("parse");

    assert_eq!(parsed.files_from.len(), 1);
    assert_eq!(parsed.files_from[0], "/path/to/list.txt");
}

#[test]
fn parse_args_recognizes_files_from_with_space() {
    use crate::frontend::arguments::parse_args;

    let parsed = parse_args(["rsync", "--files-from", "/path/to/list.txt", "src/", "dst/"])
        .expect("parse");

    assert_eq!(parsed.files_from.len(), 1);
    assert_eq!(parsed.files_from[0], "/path/to/list.txt");
}

#[test]
fn parse_args_recognizes_multiple_files_from() {
    use crate::frontend::arguments::parse_args;

    let parsed = parse_args([
        "rsync",
        "--files-from=/list1.txt",
        "--files-from=/list2.txt",
        "src/",
        "dst/",
    ])
    .expect("parse");

    assert_eq!(parsed.files_from.len(), 2);
    assert_eq!(parsed.files_from[0], "/list1.txt");
    assert_eq!(parsed.files_from[1], "/list2.txt");
}

#[test]
fn parse_args_recognizes_files_from_with_dash_for_stdin() {
    use crate::frontend::arguments::parse_args;

    let parsed = parse_args(["rsync", "--files-from=-", "src/", "dst/"]).expect("parse");

    assert_eq!(parsed.files_from.len(), 1);
    assert_eq!(parsed.files_from[0], "-");
}

#[test]
fn parse_args_recognizes_from0_flag() {
    use crate::frontend::arguments::parse_args;

    let parsed = parse_args([
        "rsync",
        "--from0",
        "--files-from=/list.txt",
        "src/",
        "dst/",
    ])
    .expect("parse");

    assert!(parsed.from0);
}

#[test]
fn parse_args_recognizes_no_from0_disables_from0() {
    use crate::frontend::arguments::parse_args;

    let parsed = parse_args([
        "rsync",
        "--from0",
        "--no-from0",
        "--files-from=/list.txt",
        "src/",
        "dst/",
    ])
    .expect("parse");

    assert!(!parsed.from0);
}

#[test]
fn parse_args_files_from_empty_value() {
    use crate::frontend::arguments::parse_args;

    // Empty string as files-from argument
    let parsed = parse_args(["rsync", "--files-from=", "src/", "dst/"]).expect("parse");

    assert_eq!(parsed.files_from.len(), 1);
    assert_eq!(parsed.files_from[0], "");
}

// ==================== Error handling tests ====================

#[cfg(unix)]
#[test]
fn files_from_reports_permission_error() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("unreadable.list");
    std::fs::write(&list_path, "file.txt\n").expect("write list");

    // Remove read permission
    let mut perms = std::fs::metadata(&list_path)
        .expect("metadata")
        .permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&list_path, perms).expect("set permissions");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        dest_dir.into_os_string(),
    ]);

    // Restore permissions for cleanup
    let mut perms = std::fs::metadata(&list_path)
        .expect("metadata")
        .permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&list_path, perms).expect("restore permissions");

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("utf8");
    assert!(
        rendered.contains("failed to read file list")
            || rendered.contains("Permission denied")
            || rendered.contains("permission denied")
    );
}

#[test]
fn files_from_with_nonexistent_source_file_reports_error() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");

    // List references a file that does not exist
    let list_path = tmp.path().join("missing_file.list");
    std::fs::write(&list_path, "nonexistent.txt\n").expect("write list");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    // Should fail or warn about missing file
    // The behavior depends on --ignore-missing-args
    let rendered = String::from_utf8(stderr).expect("utf8");
    assert!(
        code != 0
            || rendered.contains("No such file")
            || rendered.contains("vanished")
            || rendered.contains("failed")
    );
}

// ==================== Additional comprehensive tests ====================

// ==================== Real Unicode filename tests ====================

#[test]
fn files_from_handles_actual_unicode_characters() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("unicode_real.list");

    // Real Unicode characters: Chinese, Japanese, emoji-like symbols, accented
    std::fs::write(
        &list_path,
        "文件.txt\nファイル.txt\ncafe\u{0301}.txt\nnaive\u{0308}.txt\nfile_\u{2764}.txt\n",
    )
    .expect("write unicode list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0], "文件.txt");
    assert_eq!(entries[1], "ファイル.txt");
    assert_eq!(entries[2], "cafe\u{0301}.txt");
    assert_eq!(entries[3], "naive\u{0308}.txt");
    assert_eq!(entries[4], "file_\u{2764}.txt");
}

#[test]
fn files_from_handles_rtl_and_bidi_characters() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("bidi.list");

    // Right-to-left text (Arabic, Hebrew)
    std::fs::write(&list_path, "\u{05E9}\u{05DC}\u{05D5}\u{05DD}.txt\n\u{0645}\u{0644}\u{0641}.txt\n")
        .expect("write bidi list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "\u{05E9}\u{05DC}\u{05D5}\u{05DD}.txt");
    assert_eq!(entries[1], "\u{0645}\u{0644}\u{0641}.txt");
}

#[test]
fn files_from_handles_combining_characters() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("combining.list");

    // Combining diacritical marks
    std::fs::write(
        &list_path,
        "e\u{0301}.txt\na\u{0308}o\u{0308}u\u{0308}.txt\nn\u{0303}.txt\n",
    )
    .expect("write combining list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "e\u{0301}.txt");
    assert_eq!(entries[1], "a\u{0308}o\u{0308}u\u{0308}.txt");
    assert_eq!(entries[2], "n\u{0303}.txt");
}

#[test]
fn files_from_handles_zero_width_characters() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("zerowidth.list");

    // Zero-width joiner and non-joiner
    std::fs::write(
        &list_path,
        "file\u{200D}name.txt\ntest\u{200C}file.txt\n",
    )
    .expect("write zero-width list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file\u{200D}name.txt");
    assert_eq!(entries[1], "test\u{200C}file.txt");
}

// ==================== Special character edge cases ====================

#[test]
fn files_from_handles_glob_metacharacters() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("glob.list");

    // Glob metacharacters that might be interpreted by shells
    std::fs::write(
        &list_path,
        "file*.txt\nfile?.txt\nfile[abc].txt\nfile{a,b}.txt\n",
    )
    .expect("write glob list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0], "file*.txt");
    assert_eq!(entries[1], "file?.txt");
    assert_eq!(entries[2], "file[abc].txt");
    assert_eq!(entries[3], "file{a,b}.txt");
}

#[test]
fn files_from_handles_shell_special_characters() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("shell.list");

    // Shell special characters
    std::fs::write(
        &list_path,
        "file$var.txt\nfile`cmd`.txt\nfile|pipe.txt\nfile&bg.txt\nfile>redir.txt\nfile<input.txt\n",
    )
    .expect("write shell list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 6);
    assert_eq!(entries[0], "file$var.txt");
    assert_eq!(entries[1], "file`cmd`.txt");
    assert_eq!(entries[2], "file|pipe.txt");
    assert_eq!(entries[3], "file&bg.txt");
    assert_eq!(entries[4], "file>redir.txt");
    assert_eq!(entries[5], "file<input.txt");
}

#[test]
fn files_from_handles_backslash_in_filenames() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("backslash.list");

    // Backslashes (note: on Unix these are literal filename characters)
    std::fs::write(
        &list_path,
        "file\\name.txt\npath\\\\double.txt\ntrailing\\.txt\n",
    )
    .expect("write backslash list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "file\\name.txt");
    assert_eq!(entries[1], "path\\\\double.txt");
    assert_eq!(entries[2], "trailing\\.txt");
}

#[test]
fn files_from_handles_equals_and_colon_in_filenames() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("punctuation.list");

    // Characters that might be confused with argument separators
    std::fs::write(
        &list_path,
        "file=value.txt\nkey:value.txt\npath/with:colon.txt\n--not-a-flag.txt\n",
    )
    .expect("write punctuation list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0], "file=value.txt");
    assert_eq!(entries[1], "key:value.txt");
    assert_eq!(entries[2], "path/with:colon.txt");
    assert_eq!(entries[3], "--not-a-flag.txt");
}

// ==================== Whitespace edge cases ====================

#[test]
fn files_from_handles_tab_characters() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("tabs.list");

    // Tab characters in filenames
    std::fs::write(&list_path, "file\twith\ttabs.txt\n\ttab_prefix.txt\ntab_suffix\t.txt\n")
        .expect("write tabs list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "file\twith\ttabs.txt");
    assert_eq!(entries[1], "\ttab_prefix.txt");
    assert_eq!(entries[2], "tab_suffix\t.txt");
}

#[test]
fn files_from_handles_form_feed_and_vertical_tab() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("control.list");

    // Form feed and vertical tab
    std::fs::write(&list_path, "file\x0Cwith\x0Bcontrol.txt\n").expect("write control list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], "file\x0Cwith\x0Bcontrol.txt");
}

#[test]
fn files_from_preserves_multiple_consecutive_spaces() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("multispaces.list");

    std::fs::write(&list_path, "file   three   spaces.txt\nfile    four    spaces.txt\n")
        .expect("write multispaces list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file   three   spaces.txt");
    assert_eq!(entries[1], "file    four    spaces.txt");
}

// ==================== from0 comprehensive tests ====================

#[test]
fn from0_handles_embedded_newlines_and_carriage_returns() {
    use std::io::Cursor;

    // Filenames containing newlines and carriage returns
    let mut data = Vec::new();
    data.extend_from_slice(b"file\nwith\nnewlines.txt");
    data.push(0);
    data.extend_from_slice(b"file\rwith\rCR.txt");
    data.push(0);
    data.extend_from_slice(b"file\r\nwith\r\nCRLF.txt");
    data.push(0);

    let mut reader = BufReader::new(Cursor::new(data));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "file\nwith\nnewlines.txt");
    assert_eq!(entries[1], "file\rwith\rCR.txt");
    assert_eq!(entries[2], "file\r\nwith\r\nCRLF.txt");
}

#[test]
fn from0_handles_unicode_with_embedded_terminators() {
    use std::io::Cursor;

    let mut data = Vec::new();
    // Unicode filename followed by null
    data.extend_from_slice("文件.txt".as_bytes());
    data.push(0);
    // Another unicode filename
    data.extend_from_slice("カタカナ.txt".as_bytes());
    data.push(0);

    let mut reader = BufReader::new(Cursor::new(data));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "文件.txt");
    assert_eq!(entries[1], "カタカナ.txt");
}

#[test]
fn from0_handles_very_long_filenames() {
    use std::io::Cursor;

    // Create a very long filename (10000 chars)
    let long_name: String = "x".repeat(10000);
    let mut data = Vec::new();
    data.extend_from_slice(long_name.as_bytes());
    data.push(0);
    data.extend_from_slice(b"short.txt");
    data.push(0);

    let mut reader = BufReader::new(Cursor::new(data));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].to_string_lossy().len(), 10000);
    assert_eq!(entries[1], "short.txt");
}

#[test]
fn from0_handles_single_entry_no_trailing_null() {
    use std::io::Cursor;

    let data = b"single_file.txt";
    let mut reader = BufReader::new(Cursor::new(&data[..]));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], "single_file.txt");
}

#[test]
fn from0_handles_multiple_consecutive_nulls() {
    use std::io::Cursor;

    // Multiple null bytes should create empty entries that get skipped
    let data = b"file1.txt\0\0\0file2.txt\0\0";
    let mut reader = BufReader::new(Cursor::new(&data[..]));
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    // Empty entries between nulls should be skipped
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
}

// ==================== Path edge cases ====================

#[test]
fn files_from_handles_dot_and_dotdot_paths() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("dots.list");

    std::fs::write(
        &list_path,
        ".\n..\n./file.txt\n../file.txt\n./path/./to/../file.txt\n",
    )
    .expect("write dots list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0], ".");
    assert_eq!(entries[1], "..");
    assert_eq!(entries[2], "./file.txt");
    assert_eq!(entries[3], "../file.txt");
    assert_eq!(entries[4], "./path/./to/../file.txt");
}

#[test]
fn files_from_handles_hidden_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("hidden.list");

    // Unix hidden files (starting with dot)
    std::fs::write(
        &list_path,
        ".hidden\n.config/file.txt\n..weird\n...triple\n",
    )
    .expect("write hidden list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0], ".hidden");
    assert_eq!(entries[1], ".config/file.txt");
    assert_eq!(entries[2], "..weird");
    assert_eq!(entries[3], "...triple");
}

#[test]
fn files_from_handles_trailing_slash() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("trailing_slash.list");

    std::fs::write(&list_path, "directory/\npath/to/dir/\n./relative/\n")
        .expect("write trailing slash list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "directory/");
    assert_eq!(entries[1], "path/to/dir/");
    assert_eq!(entries[2], "./relative/");
}

#[test]
fn files_from_handles_double_slashes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("double_slash.list");

    std::fs::write(
        &list_path,
        "path//double//slashes.txt\n//root//style.txt\ntrailing//\n",
    )
    .expect("write double slash list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "path//double//slashes.txt");
    assert_eq!(entries[1], "//root//style.txt");
    assert_eq!(entries[2], "trailing//");
}

// ==================== Comment edge cases ====================

#[test]
fn files_from_handles_whitespace_before_comment() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("whitespace_comment.list");

    // Lines with leading whitespace before # or ; are NOT comments
    std::fs::write(&list_path, " #not_a_comment.txt\n\t;also_not_comment.txt\nfile.txt\n")
        .expect("write whitespace comment list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], " #not_a_comment.txt");
    assert_eq!(entries[1], "\t;also_not_comment.txt");
    assert_eq!(entries[2], "file.txt");
}

#[test]
fn files_from_handles_inline_hash_after_text() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("inline_hash.list");

    // Hash/semicolon after text is preserved
    std::fs::write(
        &list_path,
        "file # not a comment.txt\npath;still_valid.txt\n",
    )
    .expect("write inline hash list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file # not a comment.txt");
    assert_eq!(entries[1], "path;still_valid.txt");
}

#[test]
fn files_from_handles_empty_comment_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("empty_comment.list");

    // Comment character alone on a line
    std::fs::write(&list_path, "#\n;\nfile.txt\n").expect("write empty comment list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], "file.txt");
}

// ==================== Large file tests ====================

#[test]
fn files_from_handles_large_number_of_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("large.list");

    // Create a file with 10000 entries
    let content: String = (0..10000).map(|i| format!("file{}.txt\n", i)).collect();
    std::fs::write(&list_path, content).expect("write large list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 10000);
    assert_eq!(entries[0], "file0.txt");
    assert_eq!(entries[9999], "file9999.txt");
}

#[test]
fn files_from_handles_large_file_with_comments() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("large_with_comments.list");

    // Alternate between files and comments
    let content: String = (0..5000)
        .map(|i| format!("# Comment {}\nfile{}.txt\n", i, i))
        .collect();
    std::fs::write(&list_path, content).expect("write large list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 5000);
}

// ==================== Mixed content tests ====================

#[test]
fn files_from_handles_comprehensive_mixed_content() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("comprehensive.list");

    std::fs::write(
        &list_path,
        r#"# Header comment
; Another comment style

simple.txt
path/to/file.txt
  leading_spaces.txt
trailing_spaces.txt
file with spaces.txt
file	with	tabs.txt
/absolute/path.txt
./relative/path.txt
../parent/path.txt
file#with#hash.txt
file;with;semicolons.txt
file[1].txt
file*.txt
file?.txt

# Middle comment

unicode_文件.txt
more_files.txt
"#,
    )
    .expect("write comprehensive list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    // Count actual file entries (non-comment, non-blank)
    assert_eq!(entries.len(), 16);
    assert_eq!(entries[0], "simple.txt");
    assert_eq!(entries[1], "path/to/file.txt");
    assert_eq!(entries[2], "  leading_spaces.txt");
    assert_eq!(entries[3], "trailing_spaces.txt");
    assert_eq!(entries[4], "file with spaces.txt");
    assert_eq!(entries[5], "file\twith\ttabs.txt");
    assert_eq!(entries[6], "/absolute/path.txt");
    assert_eq!(entries[7], "./relative/path.txt");
    assert_eq!(entries[8], "../parent/path.txt");
    assert_eq!(entries[9], "file#with#hash.txt");
    assert_eq!(entries[10], "file;with;semicolons.txt");
    assert_eq!(entries[11], "file[1].txt");
    assert_eq!(entries[12], "file*.txt");
    assert_eq!(entries[13], "file?.txt");
    assert_eq!(entries[14], "unicode_文件.txt");
    assert_eq!(entries[15], "more_files.txt");
}

// ==================== Multiple files-from sources ====================

#[test]
fn files_from_combines_multiple_list_files_preserving_order() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list1 = tmp.path().join("list1.txt");
    let list2 = tmp.path().join("list2.txt");
    let list3 = tmp.path().join("list3.txt");

    std::fs::write(&list1, "a.txt\nb.txt\n").expect("write list1");
    std::fs::write(&list2, "c.txt\nd.txt\n").expect("write list2");
    std::fs::write(&list3, "e.txt\nf.txt\n").expect("write list3");

    let entries = load_file_list_operands(
        &[
            list1.into_os_string(),
            list2.into_os_string(),
            list3.into_os_string(),
        ],
        false,
    )
    .expect("load entries");

    assert_eq!(entries.len(), 6);
    assert_eq!(entries[0], "a.txt");
    assert_eq!(entries[1], "b.txt");
    assert_eq!(entries[2], "c.txt");
    assert_eq!(entries[3], "d.txt");
    assert_eq!(entries[4], "e.txt");
    assert_eq!(entries[5], "f.txt");
}

#[test]
fn files_from_handles_duplicate_entries_across_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list1 = tmp.path().join("dup1.txt");
    let list2 = tmp.path().join("dup2.txt");

    std::fs::write(&list1, "file.txt\nunique1.txt\n").expect("write list1");
    std::fs::write(&list2, "file.txt\nunique2.txt\n").expect("write list2");

    let entries = load_file_list_operands(
        &[list1.into_os_string(), list2.into_os_string()],
        false,
    )
    .expect("load entries");

    // Duplicates are preserved (not deduplicated at this level)
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0], "file.txt");
    assert_eq!(entries[1], "unique1.txt");
    assert_eq!(entries[2], "file.txt");
    assert_eq!(entries[3], "unique2.txt");
}

#[test]
fn files_from_handles_one_empty_file_among_many() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list1 = tmp.path().join("nonempty1.txt");
    let list2 = tmp.path().join("empty.txt");
    let list3 = tmp.path().join("nonempty2.txt");

    std::fs::write(&list1, "a.txt\nb.txt\n").expect("write list1");
    std::fs::write(&list2, "").expect("write empty list");
    std::fs::write(&list3, "c.txt\nd.txt\n").expect("write list3");

    let entries = load_file_list_operands(
        &[
            list1.into_os_string(),
            list2.into_os_string(),
            list3.into_os_string(),
        ],
        false,
    )
    .expect("load entries");

    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0], "a.txt");
    assert_eq!(entries[1], "b.txt");
    assert_eq!(entries[2], "c.txt");
    assert_eq!(entries[3], "d.txt");
}

// ==================== Integration tests with from0 and actual copy ====================

#[test]
fn files_from_integration_with_from0_copies_listed_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");

    // Create source files
    std::fs::write(source_dir.join("file1.txt"), b"content1").expect("write file1");
    std::fs::write(source_dir.join("file2.txt"), b"content2").expect("write file2");
    std::fs::write(source_dir.join("file3.txt"), b"content3").expect("write file3");

    // Create null-terminated file list
    let list_path = tmp.path().join("files.list");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"file1.txt");
    bytes.push(0);
    bytes.extend_from_slice(b"file3.txt");
    bytes.push(0);
    std::fs::write(&list_path, bytes).expect("write list");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--from0"),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    // Verify only listed files were copied
    assert!(dest_dir.join("file1.txt").exists());
    assert!(!dest_dir.join("file2.txt").exists());
    assert!(dest_dir.join("file3.txt").exists());
}

#[test]
fn files_from_integration_with_recursive_copies_nested_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");
    std::fs::create_dir_all(source_dir.join("a/b/c")).expect("create nested");

    std::fs::write(source_dir.join("top.txt"), b"top").expect("write top");
    std::fs::write(source_dir.join("a/mid.txt"), b"mid").expect("write mid");
    std::fs::write(source_dir.join("a/b/deep.txt"), b"deep").expect("write deep");
    std::fs::write(source_dir.join("a/b/c/deepest.txt"), b"deepest").expect("write deepest");

    let list_path = tmp.path().join("recursive.list");
    std::fs::write(&list_path, "top.txt\na/mid.txt\na/b/c/deepest.txt\n").expect("write list");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(stdout.is_empty());

    // Files are copied with their basename only (flattened)
    assert!(dest_dir.join("top.txt").exists());
    assert!(dest_dir.join("mid.txt").exists());
    assert!(dest_dir.join("deepest.txt").exists());
    // This file was not in the list
    assert!(!dest_dir.join("deep.txt").exists());
}

#[test]
fn files_from_integration_with_unicode_filenames() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");

    // Create files with Unicode names
    std::fs::write(source_dir.join("文件.txt"), b"chinese").expect("write chinese");
    std::fs::write(source_dir.join("ファイル.txt"), b"japanese").expect("write japanese");
    std::fs::write(source_dir.join("cafe\u{0301}.txt"), b"accent").expect("write accent");

    let list_path = tmp.path().join("unicode.list");
    std::fs::write(&list_path, "文件.txt\nファイル.txt\ncafe\u{0301}.txt\n").expect("write list");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(stdout.is_empty());

    assert!(dest_dir.join("文件.txt").exists());
    assert!(dest_dir.join("ファイル.txt").exists());
    assert!(dest_dir.join("cafe\u{0301}.txt").exists());

    assert_eq!(
        std::fs::read(dest_dir.join("文件.txt")).expect("read"),
        b"chinese"
    );
}

#[test]
fn files_from_integration_with_spaces_in_filenames() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");

    std::fs::write(source_dir.join("file with spaces.txt"), b"spaces").expect("write spaces");
    std::fs::write(source_dir.join("  leading.txt"), b"leading").expect("write leading");
    std::fs::write(source_dir.join("trailing  .txt"), b"trailing").expect("write trailing");

    let list_path = tmp.path().join("spaces.list");
    std::fs::write(
        &list_path,
        "file with spaces.txt\n  leading.txt\ntrailing  .txt\n",
    )
    .expect("write list");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(stdout.is_empty());

    assert!(dest_dir.join("file with spaces.txt").exists());
    assert!(dest_dir.join("  leading.txt").exists());
    assert!(dest_dir.join("trailing  .txt").exists());
}

// ==================== resolve_file_list_entries comprehensive tests ====================

#[test]
fn resolve_file_list_entries_handles_mixed_absolute_and_relative() {
    use crate::frontend::execution::resolve_file_list_entries;

    let mut entries = vec![
        OsString::from("relative.txt"),
        OsString::from("/absolute/path.txt"),
        OsString::from("another/relative.txt"),
    ];
    let operands = vec![OsString::from("/base/dir"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false);

    // Relative paths are resolved against base
    let expected_0 = Path::new("/base/dir").join("relative.txt");
    assert_eq!(entries[0], expected_0.as_os_str());
    // Absolute paths unchanged
    assert_eq!(entries[1], "/absolute/path.txt");
    // Another relative
    let expected_2 = Path::new("/base/dir").join("another/relative.txt");
    assert_eq!(entries[2], expected_2.as_os_str());
}

#[test]
fn resolve_file_list_entries_with_dot_relative_paths() {
    use crate::frontend::execution::resolve_file_list_entries;

    let mut entries = vec![
        OsString::from("./file.txt"),
        OsString::from("../parent/file.txt"),
    ];
    let operands = vec![OsString::from("/base"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false);

    // These are still relative, so they get the base prepended
    let expected_0 = Path::new("/base").join("./file.txt");
    assert_eq!(entries[0], expected_0.as_os_str());
    let expected_1 = Path::new("/base").join("../parent/file.txt");
    assert_eq!(entries[1], expected_1.as_os_str());
}
