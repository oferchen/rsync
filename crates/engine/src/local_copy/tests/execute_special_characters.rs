// Comprehensive tests for special characters in filenames during copy operations.
//
// These tests verify that the local copy engine correctly handles files with
// various special characters in their names, including:
// - Spaces (leading, trailing, multiple)
// - Quotes (single and double)
// - Backslashes
// - Glob characters (*, ?, [, ])
// - Brackets (curly, angle)
// - Shell metacharacters (|, ;, &, $)
// - Newlines and tabs
// - Control characters
// - High ASCII characters (128-255)
// - Multiple consecutive special characters
//
// Reference: rsync special character handling in flist.c, io.c

// ============================================================================
// 1. Spaces in Filenames
// ============================================================================

#[test]
fn copy_file_with_single_space_in_name() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file with space.txt";
    fs::write(source_root.join(filename), b"space content").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(dest_root.join(filename)).expect("read dest"),
        b"space content"
    );
}

#[test]
fn copy_file_with_multiple_consecutive_spaces() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file  with   multiple    spaces.txt";
    fs::write(source_root.join(filename), b"multi space").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_leading_space() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = " leading_space.txt";
    fs::write(source_root.join(filename), b"leading").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_trailing_space() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "trailing_space.txt ";
    fs::write(source_root.join(filename), b"trailing").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_named_only_spaces() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "   ";
    fs::write(source_root.join(filename), b"spaces only").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_directory_with_spaces_in_name() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let dirname = "dir with spaces";
    fs::create_dir(source_root.join(dirname)).expect("create dir");
    fs::write(
        source_root.join(dirname).join("inner file.txt"),
        b"inner",
    )
    .expect("write inner");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(dirname).join("inner file.txt").exists());
}

// ============================================================================
// 2. Quotes (Single and Double)
// ============================================================================

#[test]
fn copy_file_with_single_quotes() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file'with'quotes.txt";
    fs::write(source_root.join(filename), b"single quotes").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_double_quotes() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file\"with\"quotes.txt";
    fs::write(source_root.join(filename), b"double quotes").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_mixed_quotes() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file'and\"mixed.txt";
    fs::write(source_root.join(filename), b"mixed quotes").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_consecutive_quotes() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file''\"\"quotes.txt";
    fs::write(source_root.join(filename), b"consecutive").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// 3. Backslashes
// ============================================================================

#[test]
fn copy_file_with_backslash() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file\\with\\backslash.txt";
    fs::write(source_root.join(filename), b"backslash").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_consecutive_backslashes() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file\\\\double.txt";
    fs::write(source_root.join(filename), b"double backslash").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_trailing_backslash() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file_trailing\\";
    fs::write(source_root.join(filename), b"trailing backslash").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_escape_like_sequences() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // These look like escape sequences but are literal characters
    let filenames = ["file\\n.txt", "file\\t.txt", "file\\r.txt", "file\\0.txt"];

    for filename in &filenames {
        fs::write(source_root.join(filename), b"escape-like").expect("write source");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), filenames.len() as u64);
    for filename in &filenames {
        assert!(dest_root.join(filename).exists(), "should copy {filename}");
    }
}

// ============================================================================
// 4. Glob Characters (Asterisks and Question Marks)
// ============================================================================

#[test]
fn copy_file_with_asterisk() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file*with*asterisk.txt";
    fs::write(source_root.join(filename), b"asterisk").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_question_mark() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file?with?question.txt";
    fs::write(source_root.join(filename), b"question").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_named_asterisk_only() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "*.txt";
    fs::write(source_root.join(filename), b"star").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_glob_pattern() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file[0-9].txt";
    fs::write(source_root.join(filename), b"bracket glob").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// 5. Square Brackets
// ============================================================================

#[test]
fn copy_file_with_square_brackets() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file[with]brackets.txt";
    fs::write(source_root.join(filename), b"brackets").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_nested_brackets() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file[[nested]].txt";
    fs::write(source_root.join(filename), b"nested brackets").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// 6. Curly Braces
// ============================================================================

#[test]
fn copy_file_with_curly_braces() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file{a,b,c}.txt";
    fs::write(source_root.join(filename), b"curly braces").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_nested_curly_braces() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file{{nested}}.txt";
    fs::write(source_root.join(filename), b"nested curly").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// 7. Angle Brackets
// ============================================================================

#[test]
fn copy_file_with_angle_brackets() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file<angle>brackets.txt";
    fs::write(source_root.join(filename), b"angle brackets").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_redirection_like_name() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filenames = ["file>redirect.txt", "file>>append.txt", "file<input.txt"];

    for filename in &filenames {
        fs::write(source_root.join(filename), b"redirect").expect("write source");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), filenames.len() as u64);
    for filename in &filenames {
        assert!(dest_root.join(filename).exists(), "should copy {filename}");
    }
}

// ============================================================================
// 8. Pipe Character
// ============================================================================

#[test]
fn copy_file_with_pipe_character() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file|pipe.txt";
    fs::write(source_root.join(filename), b"pipe").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_multiple_pipes() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "a|b|c|d.txt";
    fs::write(source_root.join(filename), b"multi pipe").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// 9. Semicolon and Colon
// ============================================================================

#[test]
fn copy_file_with_semicolon() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file;semicolon.txt";
    fs::write(source_root.join(filename), b"semicolon").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_colon() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file:colon.txt";
    fs::write(source_root.join(filename), b"colon").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_time_like_colons() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "backup_12:30:45.txt";
    fs::write(source_root.join(filename), b"time").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// 10. Ampersand
// ============================================================================

#[test]
fn copy_file_with_ampersand() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file&ampersand.txt";
    fs::write(source_root.join(filename), b"ampersand").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_double_ampersand() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file&&double.txt";
    fs::write(source_root.join(filename), b"double ampersand").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// 11. Dollar Sign
// ============================================================================

#[test]
fn copy_file_with_dollar_sign() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file$dollar.txt";
    fs::write(source_root.join(filename), b"dollar").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_variable_like_name() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filenames = ["$HOME.txt", "$PATH.txt", "${VAR}.txt", "$(command).txt"];

    for filename in &filenames {
        fs::write(source_root.join(filename), b"variable").expect("write source");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), filenames.len() as u64);
    for filename in &filenames {
        assert!(dest_root.join(filename).exists(), "should copy {filename}");
    }
}

// ============================================================================
// 12. Newlines in Filenames
// ============================================================================

#[test]
fn copy_file_with_newline_in_name() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file\nwith\nnewline.txt";
    fs::write(source_root.join(filename), b"newline").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_carriage_return() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file\rwith\rcarriage.txt";
    fs::write(source_root.join(filename), b"carriage return").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_crlf() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file\r\nwith\r\ncrlf.txt";
    fs::write(source_root.join(filename), b"crlf").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_directory_with_newline() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let dirname = "dir\nwith\nnewline";
    fs::create_dir(source_root.join(dirname)).expect("create dir");
    fs::write(source_root.join(dirname).join("inner.txt"), b"inner").expect("write inner");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(dirname).join("inner.txt").exists());
}

// ============================================================================
// 13. Tab Characters
// ============================================================================

#[test]
fn copy_file_with_tab_in_name() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file\twith\ttab.txt";
    fs::write(source_root.join(filename), b"tab").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_leading_tab() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "\tleading_tab.txt";
    fs::write(source_root.join(filename), b"leading tab").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_trailing_tab() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "trailing_tab.txt\t";
    fs::write(source_root.join(filename), b"trailing tab").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_consecutive_tabs() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file\t\t\ttabs.txt";
    fs::write(source_root.join(filename), b"consecutive tabs").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_mixed_tabs_and_spaces() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file \t mixed.txt";
    fs::write(source_root.join(filename), b"mixed whitespace").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// 14. Control Characters
// ============================================================================

#[test]
fn copy_file_with_bell_character() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = OsStr::from_bytes(b"file\x07bell.txt");
    fs::write(source_root.join(filename), b"bell").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_backspace_character() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = OsStr::from_bytes(b"file\x08backspace.txt");
    fs::write(source_root.join(filename), b"backspace").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_escape_character() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = OsStr::from_bytes(b"file\x1bescape.txt");
    fs::write(source_root.join(filename), b"escape").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_form_feed() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = OsStr::from_bytes(b"file\x0cformfeed.txt");
    fs::write(source_root.join(filename), b"form feed").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_vertical_tab() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = OsStr::from_bytes(b"file\x0bvtab.txt");
    fs::write(source_root.join(filename), b"vertical tab").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_multiple_control_characters() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = OsStr::from_bytes(b"file\x01\x02\x03\x04\x05.txt");
    fs::write(source_root.join(filename), b"multiple control").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_del_character() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = OsStr::from_bytes(b"file\x7fdel.txt");
    fs::write(source_root.join(filename), b"del").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// 15. High ASCII Characters (128-255)
// ============================================================================

#[test]
fn copy_file_with_high_ascii_128() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = OsStr::from_bytes(b"file\x80high.txt");
    fs::write(source_root.join(filename), b"high 128").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_high_ascii_255() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = OsStr::from_bytes(b"file\xffhigh.txt");
    fs::write(source_root.join(filename), b"high 255").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_files_with_various_high_ascii() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Test various high-byte patterns
    let byte_values: &[u8] = &[0x80, 0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0, 0xff];
    for byte in byte_values {
        let filename_bytes = [b'f', b'i', b'l', b'e', *byte, b'.', b't', b'x', b't'];
        let filename = OsStr::from_bytes(&filename_bytes);
        fs::write(source_root.join(filename), format!("high {byte}").as_bytes())
            .expect("write source");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), byte_values.len() as u64);
}

#[test]
fn copy_file_with_non_utf8_sequence() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Invalid UTF-8: 0x80-0xBF are continuation bytes that shouldn't appear standalone
    let filename = OsStr::from_bytes(b"file\x80\x81\x82invalid.txt");
    fs::write(source_root.join(filename), b"non-utf8").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// 16. Multiple Consecutive Special Characters
// ============================================================================

#[test]
fn copy_file_with_all_quotes_consecutive() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "''''\"\"\"\"''''\"\".txt";
    fs::write(source_root.join(filename), b"all quotes").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_all_brackets_consecutive() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "[[{{(())}}]].txt";
    fs::write(source_root.join(filename), b"all brackets").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_shell_injection_like_name() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Names that could cause issues if improperly escaped
    let filenames = [
        "; rm -rf ~.txt",
        "| cat etc_passwd.txt",
        "$(whoami).txt",
        "`whoami`.txt",
        "&& echo pwned.txt",
        "|| true.txt",
    ];

    for filename in &filenames {
        fs::write(source_root.join(filename), b"shell injection").expect("write source");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), filenames.len() as u64);
    for filename in &filenames {
        assert!(dest_root.join(filename).exists(), "should copy {filename}");
    }
}

#[test]
fn copy_file_with_all_glob_characters() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "*?[a-z]*.txt";
    fs::write(source_root.join(filename), b"all globs").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// Combined Tests - Complex Scenarios
// ============================================================================

#[test]
fn copy_multiple_files_with_various_special_characters() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filenames = [
        "file with space.txt",
        "file'quote.txt",
        "file\\backslash.txt",
        "file*glob.txt",
        "file[bracket].txt",
        "file{brace}.txt",
        "file|pipe.txt",
        "file;semicolon.txt",
        "file&ampersand.txt",
        "file$dollar.txt",
        "file\ttab.txt",
    ];

    for filename in &filenames {
        fs::write(source_root.join(filename), b"mixed").expect("write source");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), filenames.len() as u64);
    for filename in &filenames {
        assert!(dest_root.join(filename).exists(), "should copy {filename}");
    }
}

#[test]
fn copy_deeply_nested_directories_with_special_characters() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create nested structure with special chars at each level
    let path = source_root
        .join("dir with space")
        .join("dir'quote")
        .join("dir*glob")
        .join("dir|pipe");
    fs::create_dir_all(&path).expect("create nested dirs");
    fs::write(path.join("deep file.txt"), b"deep").expect("write deep file");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_file = dest_root
        .join("dir with space")
        .join("dir'quote")
        .join("dir*glob")
        .join("dir|pipe")
        .join("deep file.txt");
    assert!(dest_file.exists());
}

#[test]
fn copy_file_with_special_characters_preserves_content() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file'with\"special*chars[].txt";
    let content = b"This is the exact content that should be preserved!";
    fs::write(source_root.join(filename), content).expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(dest_root.join(filename)).expect("read dest"),
        content
    );
}

#[test]
fn copy_file_with_special_characters_dry_run() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file'with\"special*chars[].txt";
    fs::write(source_root.join(filename), b"dry run test").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(
        !dest_root.join(filename).exists(),
        "dry run should not create file"
    );
}

#[test]
fn copy_file_with_special_characters_with_times_preservation() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file'with\"special*chars[].txt";
    fs::write(source_root.join(filename), b"times test").expect("write source");

    let past_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(source_root.join(filename), past_time).expect("set mtime");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join(filename)).expect("dest metadata"),
    );
    assert_eq!(dest_mtime, past_time);
}

// ============================================================================
// Additional Punctuation and Symbols
// ============================================================================

#[test]
fn copy_file_with_at_sign() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "email@domain.txt";
    fs::write(source_root.join(filename), b"at sign").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_hash() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "#hashtag.txt";
    fs::write(source_root.join(filename), b"hash").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_percent() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "100%_complete.txt";
    fs::write(source_root.join(filename), b"percent").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_caret() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "x^2.txt";
    fs::write(source_root.join(filename), b"caret").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_tilde() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filenames = ["~backup.txt", "file~.txt", "file.txt~"];

    for filename in &filenames {
        fs::write(source_root.join(filename), b"tilde").expect("write source");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), filenames.len() as u64);
}

#[test]
fn copy_file_with_backtick() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "file`backtick`.txt";
    fs::write(source_root.join(filename), b"backtick").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_exclamation() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "!important!.txt";
    fs::write(source_root.join(filename), b"exclamation").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_equals_sign() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "key=value.txt";
    fs::write(source_root.join(filename), b"equals").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_plus_sign() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "1+1=2.txt";
    fs::write(source_root.join(filename), b"plus").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_comma() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = "a,b,c.txt";
    fs::write(source_root.join(filename), b"comma").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

// ============================================================================
// Edge Cases - Leading/Trailing Dots and Dashes
// ============================================================================

#[test]
fn copy_file_with_leading_dot() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filename = ".hidden";
    fs::write(source_root.join(filename), b"hidden").expect("write source");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join(filename).exists());
}

#[test]
fn copy_file_with_multiple_leading_dots() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filenames = ["..hidden", "...hidden", "....hidden"];

    for filename in &filenames {
        fs::write(source_root.join(filename), b"multi dots").expect("write source");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), filenames.len() as u64);
}

#[test]
fn copy_file_with_leading_dash() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let filenames = ["-file.txt", "--file.txt", "-r", "--verbose"];

    for filename in &filenames {
        fs::write(source_root.join(filename), b"leading dash").expect("write source");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), filenames.len() as u64);
}

#[test]
fn copy_file_named_only_dots() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Note: "." and ".." are reserved, but "..." is valid
    let filenames = ["...", "...."];

    for filename in &filenames {
        fs::write(source_root.join(filename), b"dots only").expect("write source");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), filenames.len() as u64);
}

// ============================================================================
// Update Operations with Special Characters
// ============================================================================

#[test]
fn update_file_with_special_characters() {
    let temp = tempdir().expect("tempdir");
    let filename = "file'with\"special*chars.txt";
    let source = temp.path().join(format!("src_{filename}"));
    let destination = temp.path().join(filename);

    // Create older destination file
    fs::write(&destination, b"old content").expect("write dest");
    let old_time = FileTime::from_unix_time(1_500_000_000, 0);
    set_file_times(&destination, old_time, old_time).expect("set old time");

    // Create newer source file with different content
    fs::write(&source, b"new updated content").expect("write source");
    let new_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_times(&source, new_time, new_time).expect("set new time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"new updated content"
    );
}

#[test]
fn delete_file_with_special_characters() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Create file with special chars only in destination (to be deleted)
    let extra_file = "extra'file\"to*delete.txt";
    fs::write(dest_root.join(extra_file), b"to delete").expect("write extra");

    // Create a file in source (to be copied)
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delete(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
    assert!(!dest_root.join(extra_file).exists());
    assert!(dest_root.join("keep.txt").exists());
}
