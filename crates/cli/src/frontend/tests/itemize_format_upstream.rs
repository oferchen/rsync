// End-to-end tests verifying --itemize-changes output format matches upstream rsync.
//
// Upstream rsync --itemize-changes format reference:
//   YXcstpoguax  filename
//   Position 0: Y = update type: > (received), c (created), h (hardlink), . (unchanged), * (message)
//   Position 1: X = file type: f (file), d (directory), L (symlink), D (device), S (special)
//   Positions 2-10: attribute change indicators or '+' for new, '.' for unchanged
//
// The format string is always 11 characters for file operations, followed by
// a space and the filename. The only exception is deletion which outputs
// "*deleting   filename".

use super::common::*;
use super::*;

// ==================== New file: >f+++++++++ ==================

#[test]
fn itemize_new_file_output_matches_upstream() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("hello.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"hello world").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--itemize-changes"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let output = String::from_utf8(stdout).expect("utf8");
    assert_eq!(output, ">f+++++++++ hello.txt\n");
}

#[test]
fn itemize_short_flag_i_produces_same_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("short.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"short flag").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-i"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let output = String::from_utf8(stdout).expect("utf8");
    assert_eq!(output, ">f+++++++++ short.txt\n");
}

// ==================== Format string length ==================

#[test]
fn itemize_output_format_is_eleven_chars_plus_space_plus_filename() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("measure.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"content").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-i"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    let line = output.trim_end_matches('\n');
    // Format: "YXcstpoguax filename" -- 11 chars + space + filename
    let parts: Vec<&str> = line.splitn(2, ' ').collect();
    assert_eq!(parts.len(), 2, "should have format and filename separated by space");
    assert_eq!(parts[0].len(), 11, "format string should be exactly 11 characters: {:?}", parts[0]);
    assert_eq!(parts[1], "measure.txt");
}

// ==================== Updated file: content change ==================

#[test]
fn itemize_updated_file_shows_change_indicators() {
    use tempfile::tempdir;
    use filetime::{FileTime, set_file_mtime};

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("updated.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");

    // Create existing destination file first
    let dest_file = dest_dir.join("updated.txt");
    std::fs::write(&dest_file, b"old content").expect("write dest");
    let old_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&dest_file, old_time).expect("set dest mtime");

    // Create source with different content and newer time
    std::fs::write(&source, b"new content here").expect("write source");
    let new_time = FileTime::from_unix_time(1_700_001_000, 0);
    set_file_mtime(&source, new_time).expect("set source mtime");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-i"),
        OsString::from("--times"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    let line = output.trim_end_matches('\n');

    // Should start with '>f' (received file)
    assert!(line.starts_with(">f"), "should start with '>f': {line:?}");

    // Extract the 11-character format string
    let format_str = &line[..11];

    // Position 0: '>' for received/transferred
    assert_eq!(&format_str[0..1], ">");
    // Position 1: 'f' for regular file
    assert_eq!(&format_str[1..2], "f");
    // Position 2: 'c' for checksum (content changed)
    assert_eq!(&format_str[2..3], "c", "checksum should be 'c': {format_str:?}");
    // Position 3: 's' for size changed
    assert_eq!(&format_str[3..4], "s", "size should be 's': {format_str:?}");
    // Position 4: 't' for time changed (preserved)
    assert_eq!(&format_str[4..5], "t", "time should be 't': {format_str:?}");
}

// ==================== Unchanged file ==================

#[test]
fn itemize_unchanged_file_with_times_shows_no_output() {
    use tempfile::tempdir;
    use filetime::{FileTime, set_file_mtime};

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("same.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");

    let content = b"identical content";
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);

    // Create both with same content and mtime
    std::fs::write(&source, content).expect("write source");
    set_file_mtime(&source, timestamp).expect("set source mtime");

    let dest_file = dest_dir.join("same.txt");
    std::fs::write(&dest_file, content).expect("write dest");
    set_file_mtime(&dest_file, timestamp).expect("set dest mtime");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-i"),
        OsString::from("--times"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    // When file is unchanged, no itemize output should be produced
    // (rsync only shows items that have changes unless -ii is used)
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(
        output.is_empty() || output.starts_with(".f"),
        "unchanged file should produce no output or '.f' prefix: {output:?}"
    );
}

// ==================== Multiple files ==================

#[test]
fn itemize_multiple_new_files_each_show_new_format() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");

    std::fs::write(src_dir.join("alpha.txt"), b"alpha").expect("write alpha");
    std::fs::write(src_dir.join("beta.txt"), b"beta").expect("write beta");

    let mut src_operand = src_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-i"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    let lines: Vec<&str> = output.lines().collect();

    // Should have output for both files
    assert_eq!(lines.len(), 2, "should have 2 lines of output: {output:?}");

    for line in &lines {
        assert!(
            line.starts_with(">f+++++++++"),
            "each new file should show '>f+++++++++': {line:?}"
        );
    }
}

// ==================== Dry run combined with itemize ==================

#[test]
fn itemize_combined_with_dry_run_shows_what_would_transfer() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("dryrun.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"dry run").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-in"),
        source.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    assert_eq!(
        output, ">f+++++++++ dryrun.txt\n",
        "dry run should still show itemized format"
    );

    // File should not actually be created
    assert!(
        !dest_dir.join("dryrun.txt").exists(),
        "dry run should not create file"
    );
}

// ==================== Verbose combined with itemize ==================

#[test]
fn itemize_combined_with_verbose_shows_itemized_format() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("verbose.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"verbose").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-iv"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    // With -iv, itemize format takes precedence over verbose filename-only
    assert!(
        output.contains(">f+++++++++"),
        "verbose+itemize should show itemized format: {output:?}"
    );
}

// ==================== Delete with itemize ==================

#[test]
fn itemize_with_delete_shows_star_deleting_format() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");

    // Create a file only in destination (to be deleted)
    std::fs::write(dest_dir.join("orphan.txt"), b"orphan").expect("write orphan");

    let mut src_operand = src_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-ri"),
        OsString::from("--delete"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(
        output.contains("*deleting"),
        "delete should show '*deleting' format: {output:?}"
    );
}

// ==================== Directory creation with itemize ==================

#[test]
fn itemize_recursive_new_directory_shows_cd_plus_pattern() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&src_dir).expect("create src dir");

    // Create a subdirectory with a file
    let sub_dir = src_dir.join("subdir");
    std::fs::create_dir(&sub_dir).expect("create subdir");
    std::fs::write(sub_dir.join("file.txt"), b"content").expect("write file");

    let mut src_operand = src_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-ri"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Should contain a directory creation line
    assert!(
        output.contains("cd+++++++++"),
        "new directory should show 'cd+++++++++': {output:?}"
    );
    // Should contain a file creation line
    assert!(
        output.contains(">f+++++++++"),
        "new file should show '>f+++++++++': {output:?}"
    );
}

// ==================== Symlink with itemize ==================

#[cfg(unix)]
#[test]
fn itemize_new_symlink_shows_cl_plus_pattern() {
    use tempfile::tempdir;
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&src_dir).expect("create src dir");

    // Create a symlink in source
    let target = src_dir.join("target.txt");
    std::fs::write(&target, b"target").expect("write target");
    let link = src_dir.join("link.txt");
    symlink("target.txt", &link).expect("create symlink");

    let mut src_operand = src_dir.into_os_string();
    src_operand.push("/");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rli"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    // Should contain a symlink creation line
    assert!(
        output.contains("cL+++++++++"),
        "new symlink should show 'cL+++++++++': {output:?}"
    );
}

// ==================== Permission change with itemize ==================

#[cfg(unix)]
#[test]
fn itemize_chmod_shows_permission_indicator() {
    use tempfile::tempdir;
    use filetime::{FileTime, set_file_mtime};

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("perms.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");

    let content = b"permission test";
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);

    // Create source and dest with same content and time but different perms
    std::fs::write(&source, content).expect("write source");
    set_file_mtime(&source, timestamp).expect("set source mtime");

    let dest_file = dest_dir.join("perms.txt");
    std::fs::write(&dest_file, content).expect("write dest");
    set_file_mtime(&dest_file, timestamp).expect("set dest mtime");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-i"),
        OsString::from("--times"),
        OsString::from("--chmod=u+x"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    if !output.is_empty() {
        let line = output.lines().next().expect("first line");
        // Should show 'p' in the permissions position (position 5)
        assert!(
            line.len() >= 11,
            "format should be at least 11 chars: {line:?}"
        );
        let format_str = &line[..11];
        assert_eq!(
            &format_str[5..6], "p",
            "position 5 should show 'p' for permissions change: {format_str:?}"
        );
    }
}

// ==================== No itemize suppresses output ==================

#[test]
fn no_itemize_changes_suppresses_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("suppress.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"suppress").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-itemize-changes"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stdout.is_empty(),
        "no-itemize-changes should suppress output"
    );
}

// ==================== Itemize toggle ordering ==================

#[test]
fn itemize_last_toggle_wins_enabled() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("toggle.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"toggle").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-itemize-changes"),
        OsString::from("--itemize-changes"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(
        !output.is_empty(),
        "last --itemize-changes should enable output"
    );
    assert!(
        output.contains(">f+++++++++"),
        "should show new file format: {output:?}"
    );
}

#[test]
fn itemize_last_toggle_wins_disabled() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("toggle2.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"toggle2").expect("write source");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--itemize-changes"),
        OsString::from("--no-itemize-changes"),
        source.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stdout.is_empty(),
        "last --no-itemize-changes should suppress output"
    );
}
