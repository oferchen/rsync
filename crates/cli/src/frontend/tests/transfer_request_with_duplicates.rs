use super::common::*;
use super::*;

/// Mirrors upstream `testsuite/duplicates.test` - passing the same source
/// directory multiple times must copy each file exactly once.
///
/// Upstream deduplicates entries in `flist_sort_and_clean()` via
/// `FLAG_DUPLICATE`. This test verifies that oc-rsync produces the same
/// result: no duplicate file transfers regardless of how many times the
/// source operand is repeated.
///
/// # Upstream Reference
///
/// - `flist.c:3004-3012` - `flist_sort_and_clean()` marks duplicate entries
/// - `testsuite/duplicates.test` - copies source 10 times, asserts each
///   file appears exactly once in verbose output
#[cfg(unix)]
#[test]
fn duplicate_source_operands_copy_each_file_once() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("from");
    std::fs::create_dir(&source_dir).expect("create source");

    let name1_path = source_dir.join("name1");
    std::fs::write(&name1_path, b"This is the file").expect("write name1");

    // Create a symlink like the upstream test does
    let name2_path = source_dir.join("name2");
    std::os::unix::fs::symlink(&name1_path, &name2_path).expect("create symlink");

    let dest_dir = tmp.path().join("to");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let source_str = format!("{}/", source_dir.display());

    // Pass the source 10 times, like upstream's duplicates.test
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from(&source_str),
        OsString::from(&source_str),
        OsString::from(&source_str),
        OsString::from(&source_str),
        OsString::from(&source_str),
        OsString::from(&source_str),
        OsString::from(&source_str),
        OsString::from(&source_str),
        OsString::from(&source_str),
        OsString::from(&source_str),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));

    assert_eq!(
        std::fs::read(dest_dir.join("name1")).expect("read name1"),
        b"This is the file"
    );
    assert!(
        dest_dir.join("name2").symlink_metadata().is_ok(),
        "name2 symlink should exist"
    );

    // Check verbose output - each file should appear exactly once
    let output = String::from_utf8_lossy(&stdout);
    let name1_count = output.lines().filter(|l| l.trim() == "name1").count();
    let name2_count = output
        .lines()
        .filter(|l| l.trim().starts_with("name2 -> "))
        .count();

    assert_eq!(
        name1_count, 1,
        "name1 should appear exactly once in verbose output, got {name1_count}.\nOutput:\n{output}"
    );
    assert_eq!(
        name2_count, 1,
        "name2 symlink should appear exactly once in verbose output, got {name2_count}.\nOutput:\n{output}"
    );
}

/// Verifies that distinct source operands are all processed even after
/// deduplication removes identical ones.
#[test]
fn distinct_sources_with_duplicates_all_copied() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");

    let src_a = tmp.path().join("src_a");
    let src_b = tmp.path().join("src_b");
    std::fs::create_dir(&src_a).expect("create src_a");
    std::fs::create_dir(&src_b).expect("create src_b");

    std::fs::write(src_a.join("file_a.txt"), b"content_a").expect("write file_a");
    std::fs::write(src_b.join("file_b.txt"), b"content_b").expect("write file_b");

    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    // Pass src_a three times and src_b twice - should deduplicate to
    // one copy of each source
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        OsString::from(format!("{}/", src_a.display())),
        OsString::from(format!("{}/", src_a.display())),
        OsString::from(format!("{}/", src_a.display())),
        OsString::from(format!("{}/", src_b.display())),
        OsString::from(format!("{}/", src_b.display())),
        OsString::from(format!("{}/", dest_dir.display())),
    ]);

    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));

    // Both distinct sources should be copied
    assert!(
        dest_dir.join("file_a.txt").exists(),
        "file_a.txt should be copied from src_a"
    );
    assert!(
        dest_dir.join("file_b.txt").exists(),
        "file_b.txt should be copied from src_b"
    );
}
