use super::common::*;
use super::*;

#[test]
fn dry_run_flag_skips_destination_mutation() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    fs::write(&source, b"contents").expect("write source");
    let destination = tmp.path().join("dest.txt");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--dry-run"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!destination.exists());
}

#[test]
fn short_dry_run_flag_skips_destination_mutation() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    fs::write(&source, b"contents").expect("write source");
    let destination = tmp.path().join("dest.txt");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-n"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!destination.exists());
}

#[test]
fn dry_run_with_verbose_lists_files_on_stdout() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file1.txt"), b"aaa").expect("write file1");
    fs::write(source_dir.join("file2.txt"), b"bbb").expect("write file2");

    let dest_dir = tmp.path().join("dest");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-nv"),
        source_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr should be empty: {:?}",
        String::from_utf8_lossy(&stderr)
    );

    // With -v, rsync lists the files that would be transferred on stdout.
    let output = String::from_utf8_lossy(&stdout);
    assert!(
        output.contains("file1.txt"),
        "verbose dry-run output should list file1.txt, got: {output:?}"
    );
    assert!(
        output.contains("file2.txt"),
        "verbose dry-run output should list file2.txt, got: {output:?}"
    );

    // No files should actually be created.
    assert!(!dest_dir.exists(), "destination should not be created");
}

#[test]
fn dry_run_with_recursive_does_not_create_directories() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    let subdir = source_dir.join("subdir");
    fs::create_dir_all(&subdir).expect("create subdir");
    fs::write(subdir.join("nested.txt"), b"nested content").expect("write nested");

    let dest_dir = tmp.path().join("dest");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rn"),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(
        !dest_dir.exists(),
        "recursive dry-run should not create destination"
    );
}

#[test]
fn dry_run_preserves_existing_destination_content() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"original content").expect("write dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--dry-run"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"original content",
        "dry run must preserve existing destination content"
    );
}

#[test]
fn dry_run_with_delete_does_not_remove_files() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    let dest_dir = tmp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    // Source has one file, dest has two.
    fs::write(source_dir.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest_dir.join("keep.txt"), b"old keep").expect("write dest keep");
    fs::write(dest_dir.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rn"),
        OsString::from("--delete"),
        source_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );
    // extra.txt must still exist -- dry-run does not actually delete.
    assert!(
        dest_dir.join("extra.txt").exists(),
        "dry-run --delete must not actually remove files"
    );
    assert_eq!(
        fs::read(dest_dir.join("extra.txt")).expect("read extra"),
        b"extra"
    );
    // Destination keep.txt must be unmodified.
    assert_eq!(
        fs::read(dest_dir.join("keep.txt")).expect("read keep"),
        b"old keep"
    );
}

#[test]
fn dry_run_verbose_with_delete_lists_deletions() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    let dest_dir = tmp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");

    fs::write(source_dir.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest_dir.join("keep.txt"), b"old").expect("write dest keep");
    fs::write(dest_dir.join("orphan.txt"), b"orphan").expect("write orphan");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rnv"),
        OsString::from("--delete"),
        source_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    let output = String::from_utf8_lossy(&stdout);
    // Upstream rsync -nv --delete shows "deleting <file>" lines.
    assert!(
        output.contains("orphan.txt"),
        "verbose dry-run --delete should mention the file to be deleted, got: {output:?}"
    );
    // Files must remain.
    assert!(dest_dir.join("orphan.txt").exists());
}

#[test]
fn dry_run_with_archive_flag() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    let subdir = source_dir.join("sub");
    fs::create_dir_all(&subdir).expect("create subdir");
    fs::write(source_dir.join("root.txt"), b"root").expect("write root");
    fs::write(subdir.join("nested.txt"), b"nested").expect("write nested");

    let dest_dir = tmp.path().join("dest");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-an"),
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(
        !dest_dir.exists(),
        "archive dry-run should not create destination"
    );
}

#[test]
fn dry_run_with_exclude_filter() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::write(source_dir.join("include.txt"), b"include").expect("write include");
    fs::write(source_dir.join("exclude.log"), b"exclude").expect("write exclude");

    let dest_dir = tmp.path().join("dest");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-nv"),
        OsString::from("--exclude=*.log"),
        source_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );

    let output = String::from_utf8_lossy(&stdout);
    assert!(
        output.contains("include.txt"),
        "included file should appear in output: {output:?}"
    );
    assert!(
        !output.contains("exclude.log"),
        "excluded file should not appear in output: {output:?}"
    );
    assert!(!dest_dir.exists());
}

#[test]
fn dry_run_source_file_preserved() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    let original = b"must remain unchanged";
    fs::write(&source, original).expect("write source");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-n"),
        source.clone().into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert_eq!(
        fs::read(&source).expect("read source"),
        original,
        "source must be unmodified after dry run"
    );
}

#[test]
fn dry_run_combined_with_stats_produces_summary() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"stats test data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-n"),
        OsString::from("--stats"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );

    // With --stats, rsync produces a summary block on stdout.
    let output = String::from_utf8_lossy(&stdout);
    // The output should contain statistics keywords that upstream rsync
    // prints, like "Number of files" or "Total file size".
    assert!(
        !output.is_empty(),
        "dry-run --stats should produce statistics output"
    );
    assert!(!destination.exists());
}

#[test]
fn dry_run_multiple_files_exit_zero() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    fs::write(source_dir.join("a.txt"), b"a").expect("write a");
    fs::write(source_dir.join("b.txt"), b"b").expect("write b");
    fs::write(source_dir.join("c.txt"), b"c").expect("write c");

    let dest_dir = tmp.path().join("dest");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-n"),
        source_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(!dest_dir.exists());
}
