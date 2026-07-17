use super::common::*;
use super::*;

#[test]
fn files_from_reads_list_from_specified_file() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("file.list");

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
    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = test_support::create_tempdir();
    let missing = tmp.path().join("missing.list");
    let src_dir = tmp.path().join("files-from-error-src");
    std::fs::create_dir(&src_dir).expect("create src");
    let dest_dir = tmp.path().join("files-from-error-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    // upstream: options.c:2465-2471 requires a source and destination operand
    // with --files-from (argc == 2); the list is only read afterwards
    // (main.c:1806), so both operands are supplied to reach the read failure.
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", missing.display())),
        src_dir.into_os_string(),
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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("binary.list");
    std::fs::write(&list_path, [b'f', b'o', 0x80, b'\n']).expect("write binary list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].as_os_str().as_bytes(), b"fo\x80");
}

#[test]
fn files_from_parses_one_filename_per_line() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("multiline.list");

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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("crlf.list");

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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("mixed.list");

    std::fs::write(&list_path, "file1.txt\nfile2.txt\r\nfile3.txt\n").expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "file1.txt");
    assert_eq!(entries[1], "file2.txt");
    assert_eq!(entries[2], "file3.txt");
}

#[test]
fn files_from_skips_hash_comments() {
    let tmp = test_support::create_tempdir();
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
    let tmp = test_support::create_tempdir();
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
    let tmp = test_support::create_tempdir();
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
    let tmp = test_support::create_tempdir();
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

#[test]
fn files_from_reads_from_stdin_with_dash() {
    use std::io::Cursor;
    use std::io::Read;

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
fn from0_strips_comment_lines() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("from0_comments.list");

    // upstream: --from0 still strips leading #/; comment lines. RL_DUMP_COMMENTS
    // is gated on reading_remotely, not eol_nulls (flist.c:2249); read_line drops
    // #/; regardless of RL_EOL_NULLS (io.c:1276).
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"#comment");
    bytes.push(0);
    bytes.extend_from_slice(b";comment");
    bytes.push(0);
    bytes.extend_from_slice(b"realfile");
    bytes.push(0);
    std::fs::write(&list_path, bytes).expect("write list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], true).expect("load entries");

    assert_eq!(entries, vec!["realfile"]);
}

#[test]
fn files_from_handles_empty_file() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("empty.list");

    std::fs::write(&list_path, "").expect("write empty list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 0);
}

#[test]
fn files_from_handles_whitespace_only_file() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("whitespace.list");

    std::fs::write(&list_path, "\n\n\n").expect("write whitespace list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 0);
}

#[test]
fn files_from_reads_from_multiple_files() {
    let tmp = test_support::create_tempdir();
    let list1 = tmp.path().join("list1.txt");
    let list2 = tmp.path().join("list2.txt");

    std::fs::write(&list1, "file1.txt\nfile2.txt\n").expect("write list1");
    std::fs::write(&list2, "file3.txt\nfile4.txt\n").expect("write list2");

    let entries = load_file_list_operands(&[list1.into_os_string(), list2.into_os_string()], false)
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

#[test]
fn files_from_handles_unicode_filenames() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("unicode.list");

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
    let tmp = test_support::create_tempdir();
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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("special.list");

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

#[test]
fn files_from_preserves_absolute_paths() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("absolute.list");

    std::fs::write(
        &list_path,
        "/absolute/path/file.txt\n/another/absolute/path.txt\n",
    )
    .expect("write absolute paths list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "/absolute/path/file.txt");
    assert_eq!(entries[1], "/another/absolute/path.txt");
}

#[test]
fn files_from_preserves_relative_paths() {
    let tmp = test_support::create_tempdir();
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
    let tmp = test_support::create_tempdir();
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

#[test]
fn files_from_hash_only_on_first_char_is_comment() {
    let tmp = test_support::create_tempdir();
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
    let tmp = test_support::create_tempdir();
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
    let tmp = test_support::create_tempdir();
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

#[test]
fn reader_handles_very_long_lines() {
    use std::io::Cursor;

    // Create a very long filename (4000 chars)
    let long_name: String = "x".repeat(4000);
    let input = format!("{long_name}\nshort.txt\n");

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

#[test]
fn files_from_integration_copies_listed_files() {
    let tmp = test_support::create_tempdir();
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");

    // Create source files
    std::fs::write(source_dir.join("file1.txt"), b"content1").expect("write file1");
    std::fs::write(source_dir.join("file2.txt"), b"content2").expect("write file2");
    std::fs::write(source_dir.join("file3.txt"), b"content3").expect("write file3");

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

    assert!(dest_dir.join("file1.txt").exists());
    assert!(!dest_dir.join("file2.txt").exists());
    assert!(dest_dir.join("file3.txt").exists());
}

#[test]
fn files_from_integration_handles_nested_directories() {
    let tmp = test_support::create_tempdir();
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

    // upstream: options.c:2187-2188 - --files-from implies --relative, so
    // subdir/nested.txt stays at dest/subdir/nested.txt.
    assert!(dest_dir.join("top.txt").exists());
    assert!(dest_dir.join("subdir").join("nested.txt").exists());
}

#[test]
fn files_from_integration_with_empty_list_succeeds() {
    let tmp = test_support::create_tempdir();
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

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(!dest_dir.join("file.txt").exists());
}

#[test]
fn files_from_integration_with_comments_only_list_succeeds() {
    let tmp = test_support::create_tempdir();
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

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(!dest_dir.join("file.txt").exists());
}

#[test]
fn parse_args_recognizes_files_from_with_equals() {
    use crate::frontend::arguments::parse_args;

    let parsed =
        parse_args(["rsync", "--files-from=/path/to/list.txt", "src/", "dst/"]).expect("parse");

    assert_eq!(parsed.files_from.len(), 1);
    assert_eq!(parsed.files_from[0], "/path/to/list.txt");
}

#[test]
fn parse_args_recognizes_files_from_with_space() {
    use crate::frontend::arguments::parse_args;

    let parsed =
        parse_args(["rsync", "--files-from", "/path/to/list.txt", "src/", "dst/"]).expect("parse");

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

    let parsed =
        parse_args(["rsync", "--from0", "--files-from=/list.txt", "src/", "dst/"]).expect("parse");

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

#[cfg(unix)]
#[test]
fn files_from_reports_permission_error() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("unreadable.list");
    std::fs::write(&list_path, "file.txt\n").expect("write list");

    // Remove read permission
    let mut perms = std::fs::metadata(&list_path)
        .expect("metadata")
        .permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&list_path, perms).expect("set permissions");

    let src_dir = tmp.path().join("src");
    std::fs::create_dir(&src_dir).expect("create src");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    // upstream: options.c:2465-2471 - --files-from requires src + dest operands
    // (argc == 2) before the list is read, so both are supplied here.
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        src_dir.into_os_string(),
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
    let tmp = test_support::create_tempdir();
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

#[test]
fn files_from_handles_actual_unicode_characters() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("unicode_real.list");

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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("bidi.list");

    std::fs::write(
        &list_path,
        "\u{05E9}\u{05DC}\u{05D5}\u{05DD}.txt\n\u{0645}\u{0644}\u{0641}.txt\n",
    )
    .expect("write bidi list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "\u{05E9}\u{05DC}\u{05D5}\u{05DD}.txt");
    assert_eq!(entries[1], "\u{0645}\u{0644}\u{0641}.txt");
}

#[test]
fn files_from_handles_combining_characters() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("combining.list");

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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("zerowidth.list");

    std::fs::write(&list_path, "file\u{200D}name.txt\ntest\u{200C}file.txt\n")
        .expect("write zero-width list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file\u{200D}name.txt");
    assert_eq!(entries[1], "test\u{200C}file.txt");
}

#[test]
fn files_from_handles_glob_metacharacters() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("glob.list");

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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("shell.list");

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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("backslash.list");

    // On Unix, backslash is a literal filename character, not an escape.
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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("punctuation.list");

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

#[test]
fn files_from_handles_tab_characters() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("tabs.list");

    std::fs::write(
        &list_path,
        "file\twith\ttabs.txt\n\ttab_prefix.txt\ntab_suffix\t.txt\n",
    )
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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("control.list");

    std::fs::write(&list_path, "file\x0Cwith\x0Bcontrol.txt\n").expect("write control list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], "file\x0Cwith\x0Bcontrol.txt");
}

#[test]
fn files_from_preserves_multiple_consecutive_spaces() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("multispaces.list");

    std::fs::write(
        &list_path,
        "file   three   spaces.txt\nfile    four    spaces.txt\n",
    )
    .expect("write multispaces list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "file   three   spaces.txt");
    assert_eq!(entries[1], "file    four    spaces.txt");
}

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

#[test]
fn files_from_handles_dot_and_dotdot_paths() {
    let tmp = test_support::create_tempdir();
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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("hidden.list");

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
    let tmp = test_support::create_tempdir();
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
    let tmp = test_support::create_tempdir();
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

#[test]
fn files_from_handles_whitespace_before_comment() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("whitespace_comment.list");

    // Lines with leading whitespace before # or ; are NOT comments
    std::fs::write(
        &list_path,
        " #not_a_comment.txt\n\t;also_not_comment.txt\nfile.txt\n",
    )
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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("inline_hash.list");

    // Hash/semicolon mid-line is preserved; only line-leading hash/semicolon
    // delimits a comment.
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
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("empty_comment.list");

    std::fs::write(&list_path, "#\n;\nfile.txt\n").expect("write empty comment list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], "file.txt");
}

#[test]
fn files_from_handles_large_number_of_entries() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("large.list");

    let content: String = (0..10000).map(|i| format!("file{i}.txt\n")).collect();
    std::fs::write(&list_path, content).expect("write large list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 10000);
    assert_eq!(entries[0], "file0.txt");
    assert_eq!(entries[9999], "file9999.txt");
}

#[test]
fn files_from_handles_large_file_with_comments() {
    let tmp = test_support::create_tempdir();
    let list_path = tmp.path().join("large_with_comments.list");

    let content: String = (0..5000)
        .map(|i| format!("# Comment {i}\nfile{i}.txt\n"))
        .collect();
    std::fs::write(&list_path, content).expect("write large list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 5000);
}

#[test]
fn files_from_handles_comprehensive_mixed_content() {
    let tmp = test_support::create_tempdir();
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

#[test]
fn files_from_combines_multiple_list_files_preserving_order() {
    let tmp = test_support::create_tempdir();
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
    let tmp = test_support::create_tempdir();
    let list1 = tmp.path().join("dup1.txt");
    let list2 = tmp.path().join("dup2.txt");

    std::fs::write(&list1, "file.txt\nunique1.txt\n").expect("write list1");
    std::fs::write(&list2, "file.txt\nunique2.txt\n").expect("write list2");

    let entries = load_file_list_operands(&[list1.into_os_string(), list2.into_os_string()], false)
        .expect("load entries");

    // Duplicates are preserved; dedup happens downstream.
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0], "file.txt");
    assert_eq!(entries[1], "unique1.txt");
    assert_eq!(entries[2], "file.txt");
    assert_eq!(entries[3], "unique2.txt");
}

#[test]
fn files_from_handles_one_empty_file_among_many() {
    let tmp = test_support::create_tempdir();
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

#[test]
fn files_from_integration_with_from0_copies_listed_files() {
    let tmp = test_support::create_tempdir();
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");

    // Create source files
    std::fs::write(source_dir.join("file1.txt"), b"content1").expect("write file1");
    std::fs::write(source_dir.join("file2.txt"), b"content2").expect("write file2");
    std::fs::write(source_dir.join("file3.txt"), b"content3").expect("write file3");

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

    assert!(dest_dir.join("file1.txt").exists());
    assert!(!dest_dir.join("file2.txt").exists());
    assert!(dest_dir.join("file3.txt").exists());
}

#[test]
fn files_from_integration_with_recursive_copies_nested_files() {
    let tmp = test_support::create_tempdir();
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

    // --files-from implies --relative (upstream options.c:2187-2188), so
    // listed paths preserve their directory structure at the destination.
    assert!(dest_dir.join("top.txt").exists());
    assert!(dest_dir.join("a/mid.txt").exists());
    assert!(dest_dir.join("a/b/c/deepest.txt").exists());
    assert!(!dest_dir.join("a/b/deep.txt").exists());
}

#[test]
fn files_from_integration_with_unicode_filenames() {
    let tmp = test_support::create_tempdir();
    let source_dir = tmp.path().join("source");
    std::fs::create_dir(&source_dir).expect("create source");

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
    let tmp = test_support::create_tempdir();
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

#[test]
fn resolve_file_list_entries_handles_mixed_absolute_and_relative() {
    use crate::frontend::execution::resolve_file_list_entries;

    let mut entries = vec![
        OsString::from("relative.txt"),
        OsString::from("/absolute/path.txt"),
        OsString::from("another/relative.txt"),
    ];
    let operands = vec![OsString::from("/base/dir"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, false);

    let expected_0 = Path::new("/base/dir").join("relative.txt");
    assert_eq!(entries[0], expected_0.as_os_str());
    assert_eq!(entries[1], "/absolute/path.txt");
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

    resolve_file_list_entries(&mut entries, &operands, false, false);

    // `./` and `../` prefixes are still relative, so the base prepends.
    let expected_0 = Path::new("/base").join("./file.txt");
    assert_eq!(entries[0], expected_0.as_os_str());
    let expected_1 = Path::new("/base").join("../parent/file.txt");
    assert_eq!(entries[1], expected_1.as_os_str());
}

#[test]
fn resolve_files_from_entry_with_embedded_dot_marker_skips_extra_marker() {
    use crate::frontend::execution::resolve_file_list_entries;

    // upstream: flist.c:2316-2318 - entries like "from/./dir/subdir" already
    // contain a "./" transfer root marker. The resolver must NOT add another
    // "./" because the engine splits at the first marker, and a double marker
    // would incorrectly keep "from/" in the destination path.
    let mut entries = vec![
        OsString::from("from/./dir/subdir"),
        OsString::from("from/./dir/subdir/foobar.baz"),
        OsString::from("simple.txt"),
    ];
    let operands = vec![OsString::from("/scratch"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, true);

    // Entry with embedded marker: base joined directly, no extra "./"
    let expected_0 = Path::new("/scratch").join("from/./dir/subdir");
    assert_eq!(entries[0], expected_0.as_os_str());

    let expected_1 = Path::new("/scratch").join("from/./dir/subdir/foobar.baz");
    assert_eq!(entries[1], expected_1.as_os_str());

    // Entry without marker: base/./entry as before.
    // Build expected with the same push sequence as the implementation so
    // platform-specific separators match (Windows uses '\' between components).
    let mut expected_2 = Path::new("/scratch").to_path_buf();
    expected_2.push(".");
    expected_2.push("simple.txt");
    assert_eq!(entries[2], expected_2.as_os_str());
}

#[test]
fn resolve_files_from_entry_with_leading_dot_slash_skips_extra_marker() {
    use crate::frontend::execution::resolve_file_list_entries;

    // Entry starting with "./" already has a marker at position 0.
    let mut entries = vec![OsString::from("./dir/file.txt")];
    let operands = vec![OsString::from("/scratch"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, true);

    // Should join directly: /scratch/./dir/file.txt
    let expected = Path::new("/scratch").join("./dir/file.txt");
    assert_eq!(entries[0], expected.as_os_str());
}

/// Regression for the upstream `testsuite/files-from.test` first (local-mode)
/// invocation.
///
/// Filelist:
/// ```text
/// from/./
/// from/./dir/subdir
/// from/./dir/subdir/subsubdir
/// from/./dir/subdir/subsubdir2/
/// from/./dir/subdir/foobar.baz
/// ```
///
/// Source operand is `$scratchdir` (the parent of `from/`); dest is `$todir/`.
/// Upstream `flist.c:2316-2440` honours `(xfer_dirs && name_type != NORMAL_NAME)`
/// and calls `send_directory()` for every trailing-slash or dotdir entry, so
/// `from/./` enumerates one level of `from/`'s children even though `--files-from`
/// implicitly clears `recurse` (`options.c:2189`). The same applies to
/// `from/./dir/subdir/subsubdir2/`, which must pull `bin-lt-list` even though
/// `subsubdir/` (no trailing slash) does not pull `etc-ltr-list`.
///
/// Prior to PR #5852 the local-copy executor short-circuited at
/// `copy_directory_recursive`'s `!recursive_enabled()` bail-out, so the
/// destination only contained `dir/subdir/{foobar.baz,subsubdir,subsubdir2}`
/// and was missing all top-level siblings plus `subsubdir2/bin-lt-list`.
#[cfg(unix)]
#[test]
fn files_from_integration_matches_upstream_dotdir_walk() {
    use std::os::unix::fs::symlink;

    let tmp = test_support::create_tempdir();
    let scratch = tmp.path();
    let from = scratch.join("from");

    // hands_setup() shape from upstream testsuite/rsync.fns:
    std::fs::create_dir_all(from.join("dir/subdir/subsubdir")).expect("subsubdir");
    std::fs::create_dir_all(from.join("dir/subdir/subsubdir2")).expect("subsubdir2");
    std::fs::create_dir(from.join("emptydir")).expect("emptydir");
    std::fs::write(from.join("empty"), b"").expect("empty");
    std::fs::write(from.join("nolf"), b"This file has no trailing lf").expect("nolf");
    std::fs::write(from.join("text"), b"text payload").expect("text");
    std::fs::write(from.join("filelist"), b"placeholder").expect("filelist file");
    std::fs::write(from.join("dir/text"), b"dir text payload").expect("dir/text");
    std::fs::write(from.join("dir/subdir/foobar.baz"), b"some data\n").expect("foobar.baz");
    std::fs::write(
        from.join("dir/subdir/subsubdir/etc-ltr-list"),
        b"etc listing\n",
    )
    .expect("etc-ltr-list");
    std::fs::write(
        from.join("dir/subdir/subsubdir2/bin-lt-list"),
        b"bin listing\n",
    )
    .expect("bin-lt-list");
    symlink("nolf", from.join("nolf-symlink")).expect("nolf-symlink");

    let list_path = scratch.join("filelist");
    std::fs::write(
        &list_path,
        "from/./\n\
         from/./dir/subdir\n\
         from/./dir/subdir/subsubdir\n\
         from/./dir/subdir/subsubdir2/\n\
         from/./dir/subdir/foobar.baz\n",
    )
    .expect("write filelist");

    let to = scratch.join("to");
    std::fs::create_dir(&to).expect("create to");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from(format!("--files-from={}", list_path.display())),
        OsString::from(scratch.as_os_str()),
        to.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(stdout.is_empty());

    // Top-level siblings pulled in by `from/./` (one-level walk).
    assert!(to.join("empty").is_file(), "to/empty missing");
    assert!(to.join("emptydir").is_dir(), "to/emptydir missing");
    assert!(to.join("filelist").is_file(), "to/filelist missing");
    assert!(to.join("nolf").is_file(), "to/nolf missing");
    assert!(
        to.join("nolf-symlink").symlink_metadata().is_ok(),
        "to/nolf-symlink missing"
    );
    assert!(to.join("text").is_file(), "to/text missing");
    assert!(to.join("dir").is_dir(), "to/dir missing");

    // `from/./dir/subdir` listed without trailing slash: directory itself, no
    // children unless they are individually listed.
    assert!(to.join("dir/subdir").is_dir(), "to/dir/subdir missing");
    assert!(
        to.join("dir/subdir/foobar.baz").is_file(),
        "to/dir/subdir/foobar.baz missing"
    );
    assert!(
        to.join("dir/subdir/subsubdir").is_dir(),
        "to/dir/subdir/subsubdir missing"
    );

    // `from/./dir/subdir/subsubdir2/` listed with trailing slash: directory
    // plus one level of contents (mirrors upstream `name_type=DOTDIR_NAME`).
    assert!(
        to.join("dir/subdir/subsubdir2").is_dir(),
        "to/dir/subdir/subsubdir2 missing"
    );
    assert!(
        to.join("dir/subdir/subsubdir2/bin-lt-list").is_file(),
        "to/dir/subdir/subsubdir2/bin-lt-list missing (regression: walk-one-level dropped child)"
    );

    // `from/./dir/subdir/subsubdir` listed without trailing slash: directory
    // itself only; its children must NOT be pulled.
    assert!(
        !to.join("dir/subdir/subsubdir/etc-ltr-list").exists(),
        "non-trailing-slash entry should not pull descendants"
    );

    // `from/./` does NOT pull `dir/text` because that is two levels down from
    // the marker, and a one-level walk only enumerates immediate children.
    assert!(
        !to.join("dir/text").exists(),
        "one-level walk must not descend into dir/"
    );
}

/// Companion regression test for a flat filelist where every listed entry is a
/// top-level file. Before PR #5852 the executor walked only the entries that
/// happened to land under an already-recursed directory; flat top-level files
/// were silently dropped despite being in the list.
#[test]
fn files_from_integration_keeps_every_top_level_entry() {
    let tmp = test_support::create_tempdir();
    let source_dir = tmp.path().join("from");
    std::fs::create_dir(&source_dir).expect("create source");

    let names = [
        "alpha.txt",
        "bravo.txt",
        "charlie.txt",
        "delta.txt",
        "echo.txt",
        "foxtrot.txt",
        "golf.txt",
        "hotel.txt",
        "india.txt",
        "juliet.txt",
    ];
    for name in &names {
        std::fs::write(source_dir.join(name), format!("body of {name}").as_bytes())
            .expect("write source file");
    }

    let list_path = tmp.path().join("flat.list");
    let mut list = String::new();
    for name in &names {
        list.push_str(name);
        list.push('\n');
    }
    std::fs::write(&list_path, list).expect("write list");

    let dest_dir = tmp.path().join("to");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from(format!("--files-from={}", list_path.display())),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(stdout.is_empty());

    for name in &names {
        let dest_path = dest_dir.join(name);
        assert!(
            dest_path.is_file(),
            "{} missing from destination (regression: top-level entry dropped)",
            dest_path.display()
        );
        let actual = std::fs::read(&dest_path).expect("read dest entry");
        assert_eq!(actual, format!("body of {name}").as_bytes());
    }
}
