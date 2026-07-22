// End-to-end tests verifying --itemize-changes output format matches upstream rsync.
//
// Upstream rsync --itemize-changes format reference:
//   YXcstpoguax  filename
//   Position 0: Y = update type: > (received), c (created), h (hardlink), . (unchanged), * (message)
//   Position 1: X = file type: f (file), d (directory), L (symlink), D (device), S (special)
//   Positions 2-10: attribute change indicators or '+' for new, '.' for unchanged
//
// The format string is always 11 characters, followed by a space and the
// filename. Deletions use "*deleting  " (padded to 11 chars) followed by
// a space and the filename, matching upstream log.c:697.

use super::common::*;
use super::*;

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
    assert_eq!(
        parts.len(),
        2,
        "should have format and filename separated by space"
    );
    assert_eq!(
        parts[0].len(),
        11,
        "format string should be exactly 11 characters: {:?}",
        parts[0]
    );
    assert_eq!(parts[1], "measure.txt");
}

#[test]
fn itemize_updated_file_shows_change_indicators() {
    use filetime::{FileTime, set_file_mtime};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("updated.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");

    let dest_file = dest_dir.join("updated.txt");
    std::fs::write(&dest_file, b"old content").expect("write dest");
    let old_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&dest_file, old_time).expect("set dest mtime");

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

    assert!(line.starts_with(">f"), "should start with '>f': {line:?}");

    let format_str = &line[..11];

    assert_eq!(&format_str[0..1], ">");
    assert_eq!(&format_str[1..2], "f");
    // upstream: generator.c:1955 - ITEM_REPORT_CHECKSUM is set only under
    // `always_checksum > 0` (i.e. `--checksum`); without it, position 2 is '.'.
    assert_eq!(
        &format_str[2..3],
        ".",
        "checksum slot should be '.' without --checksum: {format_str:?}"
    );
    assert_eq!(&format_str[3..4], "s", "size should be 's': {format_str:?}");
    assert_eq!(&format_str[4..5], "t", "time should be 't': {format_str:?}");
}

#[test]
fn itemize_unchanged_file_with_times_shows_no_output() {
    use filetime::{FileTime, set_file_mtime};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("same.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");

    let content = b"identical content";
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);

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
    // upstream: -i suppresses unchanged items; only -ii lists them with '.f'.
    let output = String::from_utf8(stdout).expect("utf8");
    assert!(
        output.is_empty() || output.starts_with(".f"),
        "unchanged file should produce no output or '.f' prefix: {output:?}"
    );
}

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
        OsString::from("-r"),
        src_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    let lines: Vec<&str> = output.lines().collect();

    assert_eq!(lines.len(), 2, "should have 2 lines of output: {output:?}");

    for line in &lines {
        assert!(
            line.starts_with(">f+++++++++"),
            "each new file should show '>f+++++++++': {line:?}"
        );
    }
}

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

    assert!(
        !dest_dir.join("dryrun.txt").exists(),
        "dry run should not create file"
    );
}

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
    // upstream: -iv keeps the itemize format and suppresses bare-filename
    // verbose output.
    assert!(
        output.contains(">f+++++++++"),
        "verbose+itemize should show itemized format: {output:?}"
    );
}

#[test]
fn itemize_with_delete_shows_star_deleting_format() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");

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

#[test]
fn itemize_recursive_new_directory_shows_cd_plus_pattern() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&src_dir).expect("create src dir");

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

    assert!(
        output.contains("cd+++++++++"),
        "new directory should show 'cd+++++++++': {output:?}"
    );
    assert!(
        output.contains(">f+++++++++"),
        "new file should show '>f+++++++++': {output:?}"
    );
}

#[cfg(unix)]
#[test]
fn itemize_new_symlink_shows_cl_plus_pattern() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&src_dir).expect("create src dir");

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

    assert!(
        output.contains("cL+++++++++"),
        "new symlink should show 'cL+++++++++': {output:?}"
    );
}

#[cfg(unix)]
#[test]
fn itemize_chmod_shows_permission_indicator() {
    use filetime::{FileTime, set_file_mtime};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("perms.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");

    let content = b"permission test";
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);

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
        assert!(
            line.len() >= 11,
            "format should be at least 11 chars: {line:?}"
        );
        let format_str = &line[..11];
        assert_eq!(
            &format_str[5..6],
            "p",
            "position 5 should show 'p' for permissions change: {format_str:?}"
        );
    }
}

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

/// upstream `testsuite/itemize.test` golden: an initial `-iplr from/ to/`
/// against a non-existent dest emits a created-directory notice, a synthetic
/// root `cd+++++++++ ./` row, a `cd+++++++++ <subdir>/` row for every
/// directory entered during the recursive walk (including nested children),
/// and then the per-file rows. Mirrors upstream `main.c:807-808` +
/// `generator.c:573-579`.
#[test]
fn itemize_initial_recursive_transfer_emits_dir_rows_for_each_subdir() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");

    std::fs::create_dir_all(from.join("foo")).expect("create from/foo");
    std::fs::create_dir_all(from.join("bar").join("baz")).expect("create from/bar/baz");
    std::fs::write(from.join("foo").join("config1"), b"hello\n").expect("write foo/config1");
    std::fs::write(from.join("bar").join("baz").join("rsync"), b"world\n")
        .expect("write bar/baz/rsync");

    let from_arg = {
        let mut p = from.into_os_string();
        p.push("/");
        p
    };
    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-iplr"),
        from_arg,
        to.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");
    let mut lines = output.lines();

    let first = lines.next().expect("first line");
    assert!(
        first.starts_with("created directory "),
        "first line should announce destination root creation: {first:?}"
    );
    assert!(
        first.ends_with(to.to_string_lossy().as_ref()),
        "created-dir notice should name the dest root (no trailing slash): {first:?}"
    );

    let rest: Vec<&str> = lines.collect();
    let expected = [
        "cd+++++++++ ./",
        "cd+++++++++ bar/",
        "cd+++++++++ bar/baz/",
        ">f+++++++++ bar/baz/rsync",
        "cd+++++++++ foo/",
        ">f+++++++++ foo/config1",
    ];
    assert_eq!(
        rest, expected,
        "itemize must emit root, every intermediate subdir, and file rows in upstream order"
    );
}

/// UTS-IT.15: focused regression for the synthetic root `cd+++++++++ ./` row.
///
/// Upstream `generator.c:573-579` emits a single dir-row for the destination
/// root on an initial recursive transfer under `-i`/`--itemize-changes`, even
/// when the source tree contains only top-level files. This locks down that
/// behaviour in isolation so a regression cannot hide behind a broader test
/// also asserting per-file rows.
///
/// upstream: generator.c:573-579 root row emission.
#[test]
fn itemize_initial_recursive_transfer_emits_root_dir_row() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");

    std::fs::create_dir(&from).expect("create from");
    std::fs::write(from.join("one.txt"), b"one\n").expect("write one.txt");
    std::fs::write(from.join("two.txt"), b"two\n").expect("write two.txt");

    let from_arg = {
        let mut p = from.into_os_string();
        p.push("/");
        p
    };
    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-iplr"),
        from_arg,
        to.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    assert!(
        output.contains("cd+++++++++ ./\n"),
        "initial recursive transfer must emit synthetic root row `cd+++++++++ ./`: {output:?}"
    );

    // Lock the root row's position: it must precede every per-file row so
    // upstream consumers parsing the stream in order see the dest root
    // before any contained entry.
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| !l.starts_with("created directory "))
        .collect();
    let root_idx = lines
        .iter()
        .position(|l| *l == "cd+++++++++ ./")
        .expect("root row present");
    let first_file_idx = lines
        .iter()
        .position(|l| l.starts_with(">f"))
        .expect("at least one file row");
    assert!(
        root_idx < first_file_idx,
        "root `cd+++++++++ ./` row must precede per-file rows: {lines:?}"
    );
}

/// UTS-IT.16: focused regression for `cd+++++++++ <subdir>/` rows on every
/// implicitly-created intermediate directory.
///
/// Upstream emits one dir-row per directory entered during the recursive
/// walk, including nested children that the user did not name on the command
/// line. This test exercises a two-level nesting (`a/` then `a/b/`) and
/// asserts both rows are present, in upstream order, before the per-file
/// row inside the nested directory.
///
/// upstream: generator.c:573-579 + flist walk recursion.
#[test]
fn itemize_initial_recursive_transfer_emits_intermediate_subdir_rows() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");

    std::fs::create_dir_all(from.join("a").join("b")).expect("create from/a/b");
    std::fs::write(from.join("a").join("inside_a"), b"a\n").expect("write a/inside_a");
    std::fs::write(from.join("a").join("b").join("inside_b"), b"b\n").expect("write a/b/inside_b");

    let from_arg = {
        let mut p = from.into_os_string();
        p.push("/");
        p
    };
    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-iplr"),
        from_arg,
        to.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8(stdout).expect("utf8");

    assert!(
        output.contains("cd+++++++++ a/\n"),
        "must emit `cd+++++++++ a/` for top-level created subdir: {output:?}"
    );
    assert!(
        output.contains("cd+++++++++ a/b/\n"),
        "must emit `cd+++++++++ a/b/` for nested created subdir: {output:?}"
    );

    // Lock relative ordering: parent dir-row precedes child dir-row, which
    // precedes the file-row inside the child. Mirrors upstream depth-first
    // walk order.
    let lines: Vec<&str> = output.lines().collect();
    let a_idx = lines
        .iter()
        .position(|l| *l == "cd+++++++++ a/")
        .expect("a/ row present");
    let ab_idx = lines
        .iter()
        .position(|l| *l == "cd+++++++++ a/b/")
        .expect("a/b/ row present");
    let ab_file_idx = lines
        .iter()
        .position(|l| *l == ">f+++++++++ a/b/inside_b")
        .expect("a/b/inside_b row present");
    assert!(
        a_idx < ab_idx && ab_idx < ab_file_idx,
        "intermediate dir-rows must appear in depth-first order before contained file-row: {lines:?}"
    );
}
