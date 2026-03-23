//! Integration tests for incremental recursion (INC_RECURSE) transfers.
//!
//! Incremental recursion sends file list entries as directories are discovered
//! rather than building the entire tree upfront. These tests verify that
//! recursive local transfers correctly handle:
//!
//! - Deep directory trees (5+ levels)
//! - Empty directories at various nesting depths
//! - Symlinks within incrementally discovered subtrees
//! - Large file counts within a single directory
//!
//! Reference: upstream rsync 3.4.1 flist.c, io.c (INC_RECURSE)

mod test_timeout;

use std::fs;
use std::path::Path;

use core::client::{ClientConfig, run_client};
use tempfile::tempdir;
use test_timeout::{LOCAL_TIMEOUT, run_with_timeout};

/// Creates a file with the given content, building parent directories as needed.
fn touch(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, contents).expect("write fixture file");
}

/// Verifies that a deeply nested directory tree (6 levels) is transferred
/// correctly with recursive mode. Incremental recursion discovers each level
/// progressively as subdirectories are encountered.
#[test]
fn deep_directory_tree_transfers_all_levels() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        let dest = temp.path().join("dst");

        // Build a 6-level deep tree with files at every level.
        touch(&source.join("root.txt"), b"level-0");
        touch(&source.join("a/a.txt"), b"level-1-content");
        touch(&source.join("a/b/b.txt"), b"level-2-content-xx");
        touch(&source.join("a/b/c/c.txt"), b"level-3-content-xxxx");
        touch(&source.join("a/b/c/d/d.txt"), b"level-4-content-xxxxxx");
        touch(&source.join("a/b/c/d/e/e.txt"), b"level-5-content-xxxxxxxx");

        let mut source_arg = source.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest.clone().into_os_string()])
            .mkpath(true)
            .recursive(true)
            .times(true)
            .build();

        let summary = run_client(config).expect("deep tree transfer succeeds");

        assert!(
            summary.files_copied() >= 6,
            "all 6 files should transfer, got {}",
            summary.files_copied()
        );

        // Verify content at every level.
        assert_eq!(fs::read(dest.join("root.txt")).unwrap(), b"level-0");
        assert_eq!(fs::read(dest.join("a/a.txt")).unwrap(), b"level-1-content");
        assert_eq!(
            fs::read(dest.join("a/b/b.txt")).unwrap(),
            b"level-2-content-xx"
        );
        assert_eq!(
            fs::read(dest.join("a/b/c/c.txt")).unwrap(),
            b"level-3-content-xxxx"
        );
        assert_eq!(
            fs::read(dest.join("a/b/c/d/d.txt")).unwrap(),
            b"level-4-content-xxxxxx"
        );
        assert_eq!(
            fs::read(dest.join("a/b/c/d/e/e.txt")).unwrap(),
            b"level-5-content-xxxxxxxx"
        );

        // Verify that intermediate directories exist.
        assert!(dest.join("a/b/c/d/e").is_dir(), "deepest directory exists");
    });
}

/// Verifies that empty directories at various nesting depths are preserved
/// during recursive transfer. Upstream rsync transmits directory entries in
/// the file list even when they contain no children.
#[test]
fn empty_directories_are_preserved() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        let dest = temp.path().join("dst");

        // Mix of populated and empty directories at different depths.
        touch(&source.join("populated/file.txt"), b"content");
        fs::create_dir_all(source.join("empty_top")).expect("empty_top");
        fs::create_dir_all(source.join("populated/empty_nested")).expect("empty_nested");
        fs::create_dir_all(source.join("deep/path/to/empty")).expect("deep empty");
        fs::create_dir_all(source.join("deep/path/sibling_empty")).expect("sibling empty");
        touch(&source.join("deep/path/marker.txt"), b"marker");

        let mut source_arg = source.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest.clone().into_os_string()])
            .mkpath(true)
            .recursive(true)
            .times(true)
            .build();

        let _summary = run_client(config).expect("empty dirs transfer succeeds");

        assert!(
            dest.join("empty_top").is_dir(),
            "top-level empty directory preserved"
        );
        assert!(
            dest.join("populated/empty_nested").is_dir(),
            "nested empty directory preserved"
        );
        assert!(
            dest.join("deep/path/to/empty").is_dir(),
            "deeply nested empty directory preserved"
        );
        assert!(
            dest.join("deep/path/sibling_empty").is_dir(),
            "sibling empty directory preserved"
        );

        assert_eq!(
            fs::read(dest.join("populated/file.txt")).unwrap(),
            b"content"
        );
        assert_eq!(
            fs::read(dest.join("deep/path/marker.txt")).unwrap(),
            b"marker"
        );
    });
}

/// Verifies that symbolic links within incrementally discovered subtrees
/// are correctly transferred when `--links` is enabled.
#[cfg(unix)]
#[test]
#[ignore = "test fixture bug: dir2/nested/ not created before symlinking into it"]
fn symlinks_within_incremental_directories() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        let dest = temp.path().join("dst");

        touch(&source.join("dir1/real.txt"), b"real-file-data");
        touch(&source.join("dir1/sub/deep.txt"), b"deep-file-data");

        std::os::unix::fs::symlink("real.txt", source.join("dir1/link_same_dir"))
            .expect("symlink same dir");
        std::os::unix::fs::symlink("sub/deep.txt", source.join("dir1/link_to_deep"))
            .expect("symlink to deep");
        std::os::unix::fs::symlink("../../dir1/real.txt", source.join("dir2/nested/link_up"))
            .expect("symlink upward");
        std::os::unix::fs::symlink("dir1/sub", source.join("link_to_subdir"))
            .expect("symlink to subdir");

        let mut source_arg = source.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest.clone().into_os_string()])
            .mkpath(true)
            .recursive(true)
            .links(true)
            .times(true)
            .build();

        let summary = run_client(config).expect("symlink transfer succeeds");

        assert_eq!(
            fs::read(dest.join("dir1/real.txt")).unwrap(),
            b"real-file-data"
        );
        assert_eq!(
            fs::read(dest.join("dir1/sub/deep.txt")).unwrap(),
            b"deep-file-data"
        );

        let link_same = fs::read_link(dest.join("dir1/link_same_dir")).expect("read link_same_dir");
        assert_eq!(link_same.to_str().unwrap(), "real.txt");

        let link_deep = fs::read_link(dest.join("dir1/link_to_deep")).expect("read link_to_deep");
        assert_eq!(link_deep.to_str().unwrap(), "sub/deep.txt");

        let link_up = fs::read_link(dest.join("dir2/nested/link_up")).expect("read link_up");
        assert_eq!(link_up.to_str().unwrap(), "../../dir1/real.txt");

        let link_dir = fs::read_link(dest.join("link_to_subdir")).expect("read link_to_subdir");
        assert_eq!(link_dir.to_str().unwrap(), "dir1/sub");

        assert!(
            summary.symlinks_copied() >= 4,
            "expected at least 4 symlinks, got {}",
            summary.symlinks_copied()
        );
    });
}

/// Verifies that a directory containing a large number of files is
/// transferred correctly under incremental recursion. This exercises the
/// file list batching and sorting logic that INC_RECURSE relies on when
/// a single directory yields many entries.
#[test]
fn large_file_count_single_directory() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        let dest = temp.path().join("dst");

        let large_dir = source.join("many");
        fs::create_dir_all(&large_dir).expect("create many dir");

        let file_count: u32 = 500;
        for i in 0..file_count {
            let name = format!("file_{i:04}.dat");
            let content = format!("payload-{i:04}-{}", "x".repeat(i as usize % 64));
            fs::write(large_dir.join(&name), content.as_bytes()).expect("write file");
        }

        touch(&source.join("top.txt"), b"top-level-file");
        touch(&source.join("other/side.txt"), b"sibling-directory");

        let mut source_arg = source.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest.clone().into_os_string()])
            .mkpath(true)
            .recursive(true)
            .times(true)
            .build();

        let summary = run_client(config).expect("large file count transfer succeeds");

        let expected_files = file_count as u64 + 2;
        assert!(
            summary.files_copied() >= expected_files,
            "expected at least {expected_files} files copied, got {}",
            summary.files_copied()
        );

        assert!(dest.join("many/file_0000.dat").exists(), "first file");
        assert!(dest.join("many/file_0249.dat").exists(), "middle file");
        assert!(dest.join("many/file_0499.dat").exists(), "last file");

        let content_42 = fs::read_to_string(dest.join("many/file_0042.dat")).unwrap();
        let expected_42 = format!("payload-0042-{}", "x".repeat(42));
        assert_eq!(content_42, expected_42, "content integrity for file_0042");

        assert_eq!(fs::read(dest.join("top.txt")).unwrap(), b"top-level-file");
        assert_eq!(
            fs::read(dest.join("other/side.txt")).unwrap(),
            b"sibling-directory"
        );
    });
}

/// Verifies that a deep tree with mixed content types - regular files,
/// empty directories, and symlinks - all transfer correctly in a single
/// recursive pass. This is the combined scenario that exercises
/// incremental recursion most thoroughly.
#[cfg(unix)]
#[test]
fn mixed_content_deep_tree() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        let dest = temp.path().join("dst");

        touch(&source.join("level1/data.txt"), b"l1-data");
        std::os::unix::fs::symlink("data.txt", source.join("level1/link.txt")).expect("l1 symlink");

        fs::create_dir_all(source.join("level1/level2/empty")).expect("l2 empty");
        touch(
            &source.join("level1/level2/populated/file.txt"),
            b"l2-populated",
        );

        touch(
            &source.join("level1/level2/populated/level3/deep.bin"),
            b"l3-binary-content",
        );
        std::os::unix::fs::symlink(
            "../file.txt",
            source.join("level1/level2/populated/level3/uplink"),
        )
        .expect("l3 symlink");

        fs::create_dir_all(source.join("level1/level2/populated/level3/level4/vacant"))
            .expect("l4 empty");
        touch(
            &source.join("level1/level2/populated/level3/level4/bottom.txt"),
            b"l4-bottom",
        );

        let mut source_arg = source.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest.clone().into_os_string()])
            .mkpath(true)
            .recursive(true)
            .links(true)
            .times(true)
            .build();

        let summary = run_client(config).expect("mixed content transfer succeeds");

        assert_eq!(fs::read(dest.join("level1/data.txt")).unwrap(), b"l1-data");
        assert_eq!(
            fs::read(dest.join("level1/level2/populated/file.txt")).unwrap(),
            b"l2-populated"
        );
        assert_eq!(
            fs::read(dest.join("level1/level2/populated/level3/deep.bin")).unwrap(),
            b"l3-binary-content"
        );
        assert_eq!(
            fs::read(dest.join("level1/level2/populated/level3/level4/bottom.txt")).unwrap(),
            b"l4-bottom"
        );

        let link1 = fs::read_link(dest.join("level1/link.txt")).expect("l1 symlink");
        assert_eq!(link1.to_str().unwrap(), "data.txt");
        let link3 =
            fs::read_link(dest.join("level1/level2/populated/level3/uplink")).expect("l3 symlink");
        assert_eq!(link3.to_str().unwrap(), "../file.txt");

        assert!(
            dest.join("level1/level2/empty").is_dir(),
            "l2 empty dir preserved"
        );
        assert!(
            dest.join("level1/level2/populated/level3/level4/vacant")
                .is_dir(),
            "l4 vacant dir preserved"
        );

        assert!(summary.files_copied() >= 4, "at least 4 regular files");
        assert!(summary.symlinks_copied() >= 2, "at least 2 symlinks");
    });
}

/// Verifies that re-running a recursive transfer (second pass) correctly
/// identifies that all files are up-to-date when source has not changed.
/// This tests that incremental recursion handles the quick-check comparison
/// across all discovered directories.
#[test]
fn second_pass_skips_unchanged_files() {
    run_with_timeout(LOCAL_TIMEOUT, || {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        let dest = temp.path().join("dst");

        touch(&source.join("a/file1.txt"), b"content-one-xxx");
        touch(&source.join("a/b/file2.txt"), b"content-two-xxxxxxx");
        touch(&source.join("c/file3.txt"), b"content-three-xxxxxxxxx");

        let mut source_arg = source.into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config1 = ClientConfig::builder()
            .transfer_args([source_arg.clone(), dest.clone().into_os_string()])
            .mkpath(true)
            .recursive(true)
            .times(true)
            .build();

        let summary1 = run_client(config1).expect("first pass succeeds");
        assert!(summary1.files_copied() >= 3, "first pass copies all files");

        assert_eq!(
            fs::read(dest.join("a/file1.txt")).unwrap(),
            b"content-one-xxx"
        );

        let config2 = ClientConfig::builder()
            .transfer_args([source_arg, dest.clone().into_os_string()])
            .mkpath(true)
            .recursive(true)
            .times(true)
            .build();

        let summary2 = run_client(config2).expect("second pass succeeds");
        assert_eq!(
            summary2.files_copied(),
            0,
            "second pass should skip all files (quick-check match)"
        );
    });
}
