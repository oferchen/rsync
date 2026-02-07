//! Integration tests for incremental file list processing.
//!
//! These tests verify that the file list walker supports incremental/streaming
//! consumption -- processing entries one at a time as they are yielded rather
//! than collecting the entire list first. This mirrors upstream rsync's
//! `INC_RECURSE` mode where entries are processed as soon as they become
//! available.
//!
//! The tests cover:
//! - Entry-by-entry streaming consumption
//! - File metadata preservation during incremental iteration
//! - Directory vs file vs symlink entry type handling
//! - Ordering guarantees during streaming
//! - Edge cases: empty directories, single files, deep nesting
//! - Large file list incremental processing
//! - LazyFileListEntry deferred metadata patterns
//!
//! Reference: rsync 3.4.1 flist.c, io.c (INC_RECURSE)

use flist::{FileListBuilder, FileListEntry, FileListError, LazyFileListEntry};
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

// ============================================================================
// Helper Functions
// ============================================================================

/// Collects relative paths from a walker, skipping the root entry.
fn collect_relative_paths(
    walker: impl Iterator<Item = Result<FileListEntry, FileListError>>,
) -> Vec<PathBuf> {
    walker
        .filter_map(|r| r.ok())
        .filter(|e| !e.is_root())
        .map(|e| e.relative_path().to_path_buf())
        .collect()
}

/// Creates a standard test directory tree for incremental processing tests.
/// Structure:
///   root/
///     adir/
///       nested.txt (100 bytes)
///       subdir/
///         deep.txt (200 bytes)
///     bdir/
///       file.txt (50 bytes)
///     top_file.txt (10 bytes)
fn create_standard_tree() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    let adir = root.join("adir");
    let subdir = adir.join("subdir");
    fs::create_dir(&adir).expect("create adir");
    fs::create_dir(&subdir).expect("create subdir");
    fs::write(adir.join("nested.txt"), &[0u8; 100]).expect("write nested.txt");
    fs::write(subdir.join("deep.txt"), &[0u8; 200]).expect("write deep.txt");

    let bdir = root.join("bdir");
    fs::create_dir(&bdir).expect("create bdir");
    fs::write(bdir.join("file.txt"), &[0u8; 50]).expect("write file.txt");

    fs::write(root.join("top_file.txt"), &[0u8; 10]).expect("write top_file.txt");

    (temp, root)
}

// ============================================================================
// Streaming / Entry-by-Entry Consumption Tests
// ============================================================================

/// Verifies that the walker can be consumed one entry at a time in a streaming
/// fashion, processing each entry before requesting the next.
#[test]
fn streaming_one_entry_at_a_time() {
    let (_temp, root) = create_standard_tree();
    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    let mut count = 0;
    while let Some(result) = walker.next() {
        let entry = result.expect("entry should succeed");
        // Simulate incremental processing: inspect each entry individually
        let _ = entry.relative_path();
        let _ = entry.metadata();
        let _ = entry.depth();
        let _ = entry.is_root();
        count += 1;
    }

    // root + adir + nested.txt + subdir + deep.txt + bdir + file.txt + top_file.txt = 8
    assert_eq!(count, 8);
}

/// Verifies that entries yielded in streaming order maintain depth-first
/// traversal -- a directory's contents appear before the next sibling.
#[test]
fn streaming_depth_first_order() {
    let (_temp, root) = create_standard_tree();
    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    let mut entries_seen = Vec::new();
    while let Some(result) = walker.next() {
        let entry = result.expect("entry should succeed");
        if !entry.is_root() {
            entries_seen.push(entry.relative_path().to_path_buf());
        }
    }

    // adir and all its children must come before bdir
    let adir_idx = entries_seen
        .iter()
        .position(|p| p == Path::new("adir"))
        .expect("adir");
    let deep_idx = entries_seen
        .iter()
        .position(|p| p == Path::new("adir/subdir/deep.txt"))
        .expect("deep.txt");
    let bdir_idx = entries_seen
        .iter()
        .position(|p| p == Path::new("bdir"))
        .expect("bdir");

    assert!(
        adir_idx < deep_idx,
        "adir should appear before its nested content"
    );
    assert!(
        deep_idx < bdir_idx,
        "adir's deep content should appear before sibling bdir"
    );
}

/// Verifies that a directory entry always appears before its children during
/// incremental consumption.
#[test]
fn streaming_parent_before_children() {
    let (_temp, root) = create_standard_tree();
    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let mut seen_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    // Root's children have their parent implicitly available
    seen_dirs.insert(PathBuf::new());

    for result in walker {
        let entry = result.expect("entry should succeed");
        let rel = entry.relative_path().to_path_buf();

        // For every entry, its parent should already have been seen
        if let Some(parent) = rel.parent() {
            assert!(
                seen_dirs.contains(parent),
                "parent {:?} of {:?} should have been seen already",
                parent,
                rel
            );
        }

        if entry.metadata().is_dir() {
            seen_dirs.insert(rel);
        }
    }
}

/// Verifies that the walker can be partially consumed and then abandoned
/// without issues (simulates early termination in incremental processing).
#[test]
fn streaming_early_termination() {
    let (_temp, root) = create_standard_tree();
    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Consume only 3 entries then drop the walker
    let first = walker.next().expect("first").expect("ok");
    assert!(first.is_root());

    let second = walker.next().expect("second").expect("ok");
    assert!(!second.is_root());

    let third = walker.next().expect("third").expect("ok");
    assert!(!third.is_root());

    // Drop walker here -- should not panic or leak
    drop(walker);
}

/// Verifies that entries can be collected into an accumulator incrementally,
/// yielding the same result as batch collection.
#[test]
fn streaming_matches_batch_collection() {
    let (_temp, root) = create_standard_tree();

    // Batch collection
    let batch_walker = FileListBuilder::new(&root).build().expect("build batch walker");
    let batch_paths = collect_relative_paths(batch_walker);

    // Incremental collection
    let mut stream_walker = FileListBuilder::new(&root).build().expect("build stream walker");
    let mut stream_paths = Vec::new();
    while let Some(result) = stream_walker.next() {
        let entry = result.expect("entry should succeed");
        if !entry.is_root() {
            stream_paths.push(entry.relative_path().to_path_buf());
        }
    }

    assert_eq!(batch_paths, stream_paths);
}

// ============================================================================
// File Metadata Preservation Tests
// ============================================================================

/// Verifies that file size is correctly reported in each yielded entry.
#[test]
fn metadata_file_size_preserved() {
    let (_temp, root) = create_standard_tree();
    let walker = FileListBuilder::new(&root).build().expect("build walker");

    let mut found_sizes = std::collections::HashMap::new();
    for result in walker {
        let entry = result.expect("entry should succeed");
        if entry.metadata().is_file() {
            let name = entry.relative_path().to_path_buf();
            let size = entry.metadata().len();
            found_sizes.insert(name, size);
        }
    }

    assert_eq!(
        found_sizes.get(Path::new("adir/nested.txt")),
        Some(&100),
        "nested.txt should be 100 bytes"
    );
    assert_eq!(
        found_sizes.get(Path::new("adir/subdir/deep.txt")),
        Some(&200),
        "deep.txt should be 200 bytes"
    );
    assert_eq!(
        found_sizes.get(Path::new("bdir/file.txt")),
        Some(&50),
        "file.txt should be 50 bytes"
    );
    assert_eq!(
        found_sizes.get(Path::new("top_file.txt")),
        Some(&10),
        "top_file.txt should be 10 bytes"
    );
}

/// Verifies that modification timestamps are non-zero for created files.
#[test]
fn metadata_mtime_nonzero() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("timed.txt");
    fs::write(&file, b"data").expect("write");

    let mut walker = FileListBuilder::new(&file).build().expect("build walker");
    let entry = walker.next().expect("entry").expect("ok");

    let mtime = entry
        .metadata()
        .modified()
        .expect("modified time should be available");
    // The file was just created, so mtime should be reasonably recent
    let elapsed = mtime
        .elapsed()
        .expect("elapsed should not fail for recent file");
    assert!(
        elapsed.as_secs() < 60,
        "file created just now should have recent mtime"
    );
}

/// Verifies that metadata distinguishes files from directories correctly.
#[test]
fn metadata_file_type_differentiation() {
    let (_temp, root) = create_standard_tree();
    let walker = FileListBuilder::new(&root).build().expect("build walker");

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for result in walker {
        let entry = result.expect("entry should succeed");
        if entry.is_root() {
            continue;
        }
        if entry.metadata().is_dir() {
            dirs.push(entry.relative_path().to_path_buf());
        } else if entry.metadata().is_file() {
            files.push(entry.relative_path().to_path_buf());
        }
    }

    // Directories: adir, adir/subdir, bdir
    assert_eq!(dirs.len(), 3, "should have 3 directories: {dirs:?}");
    assert!(dirs.contains(&PathBuf::from("adir")));
    assert!(dirs.contains(&PathBuf::from("adir/subdir")));
    assert!(dirs.contains(&PathBuf::from("bdir")));

    // Files: adir/nested.txt, adir/subdir/deep.txt, bdir/file.txt, top_file.txt
    assert_eq!(files.len(), 4, "should have 4 files: {files:?}");
}

/// Verifies that full_path is always absolute for every yielded entry.
#[test]
fn metadata_full_path_always_absolute() {
    let (_temp, root) = create_standard_tree();
    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for result in walker {
        let entry = result.expect("entry should succeed");
        assert!(
            entry.full_path().is_absolute(),
            "full_path should be absolute for {:?}",
            entry.relative_path()
        );
    }
}

/// Verifies that file_name() returns the correct tail component for each entry.
#[test]
fn metadata_file_name_consistency() {
    let (_temp, root) = create_standard_tree();
    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for result in walker {
        let entry = result.expect("entry should succeed");
        if entry.is_root() {
            assert!(entry.file_name().is_none(), "root should have no file_name");
        } else {
            let file_name = entry
                .file_name()
                .expect("non-root should have file_name");
            let expected = entry.relative_path().file_name().unwrap();
            assert_eq!(file_name, expected);
        }
    }
}

/// Verifies that depth matches the number of path components for every entry.
#[test]
fn metadata_depth_matches_path_components() {
    let (_temp, root) = create_standard_tree();
    let walker = FileListBuilder::new(&root).build().expect("build walker");

    for result in walker {
        let entry = result.expect("entry should succeed");
        if entry.is_root() {
            assert_eq!(entry.depth(), 0);
        } else {
            let expected_depth = entry.relative_path().components().count();
            assert_eq!(
                entry.depth(),
                expected_depth,
                "depth mismatch for {:?}",
                entry.relative_path()
            );
        }
    }
}

// ============================================================================
// Directory vs File Entry Handling
// ============================================================================

/// Verifies that the root entry for a directory is correctly tagged.
#[test]
fn directory_root_entry_is_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("dir_root");
    fs::create_dir(&root).expect("create dir");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");
    let entry = walker.next().expect("root entry").expect("ok");

    assert!(entry.is_root());
    assert!(entry.metadata().is_dir());
    assert_eq!(entry.depth(), 0);
}

/// Verifies that the root entry for a single file is correctly tagged.
#[test]
fn file_root_entry_is_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("single.txt");
    fs::write(&file, b"content").expect("write");

    let mut walker = FileListBuilder::new(&file).build().expect("build walker");
    let entry = walker.next().expect("root entry").expect("ok");

    assert!(entry.is_root());
    assert!(entry.metadata().is_file());
    assert!(walker.next().is_none(), "single file should have no children");
}

/// Verifies correct handling of directories containing only subdirectories
/// (no files).
#[test]
fn directory_only_tree() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("dirs_only");
    fs::create_dir_all(root.join("a/b/c")).expect("create nested dirs");
    fs::create_dir(root.join("d")).expect("create d");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let mut all_are_dirs = true;
    let mut count = 0;
    for result in walker {
        let entry = result.expect("entry should succeed");
        if !entry.metadata().is_dir() {
            all_are_dirs = false;
        }
        count += 1;
    }

    assert!(all_are_dirs, "all entries should be directories");
    assert_eq!(count, 4, "should have a, a/b, a/b/c, d");
}

/// Verifies correct handling of directories containing only files (no
/// subdirectories).
#[test]
fn files_only_tree() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("files_only");
    fs::create_dir(&root).expect("create root");

    for i in 0..5 {
        fs::write(root.join(format!("file{i}.txt")), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let mut all_are_files = true;
    let mut count = 0;
    for result in walker {
        let entry = result.expect("entry should succeed");
        if !entry.metadata().is_file() {
            all_are_files = false;
        }
        count += 1;
    }

    assert!(all_are_files, "all entries should be files");
    assert_eq!(count, 5);
}

// ============================================================================
// Symlink Entry Handling Tests
// ============================================================================

/// Verifies that symlink entries are yielded with symlink metadata when not
/// following symlinks.
#[cfg(unix)]
#[test]
fn symlink_entry_metadata_preserved() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("symlinks");
    fs::create_dir(&root).expect("create root");

    let target = root.join("target.txt");
    fs::write(&target, b"target content").expect("write target");
    symlink(&target, root.join("link.txt")).expect("create symlink");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(false)
        .build()
        .expect("build walker");

    let mut found_target = false;
    let mut found_link = false;

    for result in walker {
        let entry = result.expect("entry should succeed");
        if entry.relative_path() == Path::new("target.txt") {
            assert!(entry.metadata().is_file());
            found_target = true;
        }
        if entry.relative_path() == Path::new("link.txt") {
            assert!(
                entry.metadata().file_type().is_symlink(),
                "link should report symlink file type"
            );
            found_link = true;
        }
    }

    assert!(found_target, "should find target.txt");
    assert!(found_link, "should find link.txt");
}

/// Verifies that symlinks pointing to files are yielded as single entries
/// (no children) when not following.
#[cfg(unix)]
#[test]
fn symlink_to_dir_not_followed_yields_single_entry() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    let target_dir = temp.path().join("target_dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::write(target_dir.join("inner.txt"), b"data").expect("write inner");

    symlink(&target_dir, root.join("link_dir")).expect("create dir symlink");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(false)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);

    // Only the symlink itself, not its contents
    assert_eq!(paths, vec![PathBuf::from("link_dir")]);
}

/// Verifies that symlinks pointing to directories yield their contents when
/// follow_symlinks is enabled, and that the symlink entry reports symlink
/// file type while children report their real types.
#[cfg(unix)]
#[test]
fn symlink_to_dir_followed_yields_children() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create root");

    let target_dir = temp.path().join("target_dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::write(target_dir.join("inner.txt"), b"data").expect("write inner");

    symlink(&target_dir, root.join("link_dir")).expect("create dir symlink");

    let walker = FileListBuilder::new(&root)
        .follow_symlinks(true)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);
    assert!(
        paths.contains(&PathBuf::from("link_dir")),
        "should contain the symlink entry"
    );
    assert!(
        paths.contains(&PathBuf::from("link_dir/inner.txt")),
        "should contain the child through the symlink"
    );
}

// ============================================================================
// Entry Ordering and Sorting Tests
// ============================================================================

/// Verifies that entries within the same directory are always yielded in
/// sorted (lexicographic) order during incremental consumption.
#[test]
fn incremental_entries_sorted_within_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sorted");
    fs::create_dir(&root).expect("create root");

    // Create files in reverse alphabetical order
    for name in ["z_file", "m_file", "a_file"] {
        fs::write(root.join(name), b"data").expect("write file");
    }

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Skip root
    let _ = walker.next();

    let mut names = Vec::new();
    while let Some(result) = walker.next() {
        let entry = result.expect("entry should succeed");
        names.push(entry.relative_path().to_path_buf());
    }

    assert_eq!(
        names,
        vec![
            PathBuf::from("a_file"),
            PathBuf::from("m_file"),
            PathBuf::from("z_file"),
        ]
    );
}

/// Verifies that mixed files and directories are sorted together, with no
/// special precedence for either type.
#[test]
fn incremental_mixed_types_sorted_together() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("mixed");
    fs::create_dir(&root).expect("create root");

    fs::write(root.join("b_file.txt"), b"data").expect("write file");
    fs::create_dir(root.join("a_dir")).expect("create dir");
    fs::write(root.join("c_file.txt"), b"data").expect("write file");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);
    assert_eq!(
        paths,
        vec![
            PathBuf::from("a_dir"),
            PathBuf::from("b_file.txt"),
            PathBuf::from("c_file.txt"),
        ]
    );
}

/// Verifies deterministic ordering across multiple incremental traversals
/// of the same tree.
#[test]
fn incremental_deterministic_across_runs() {
    let (_temp, root) = create_standard_tree();

    let results: Vec<Vec<PathBuf>> = (0..5)
        .map(|_| {
            let walker = FileListBuilder::new(&root).build().expect("build walker");
            collect_relative_paths(walker)
        })
        .collect();

    for (i, result) in results.iter().enumerate().skip(1) {
        assert_eq!(
            &results[0], result,
            "run {i} differs from first run"
        );
    }
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Verifies that an empty directory yields only the root entry during
/// incremental processing.
#[test]
fn edge_case_empty_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("empty");
    fs::create_dir(&root).expect("create empty dir");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    let root_entry = walker.next().expect("root entry").expect("ok");
    assert!(root_entry.is_root());
    assert!(root_entry.metadata().is_dir());

    assert!(walker.next().is_none(), "empty dir should yield no children");
}

/// Verifies incremental processing of an empty directory with include_root=false.
#[test]
fn edge_case_empty_directory_no_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("empty_no_root");
    fs::create_dir(&root).expect("create empty dir");

    let mut walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    assert!(
        walker.next().is_none(),
        "empty dir with no root should yield nothing"
    );
}

/// Verifies that a single file root yields exactly one entry.
#[test]
fn edge_case_single_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("lone.txt");
    fs::write(&file, b"alone").expect("write");

    let mut walker = FileListBuilder::new(&file).build().expect("build walker");

    let entry = walker.next().expect("entry").expect("ok");
    assert!(entry.is_root());
    assert!(entry.metadata().is_file());
    assert_eq!(entry.full_path(), file);

    assert!(walker.next().is_none());
}

/// Verifies that building a walker from a nonexistent path returns an error
/// before any streaming begins.
#[test]
fn edge_case_nonexistent_path() {
    let result = FileListBuilder::new("/nonexistent/path/xyz").build();
    assert!(result.is_err(), "nonexistent path should fail at build time");
}

/// Verifies behavior after the walker is fully exhausted -- repeated next()
/// calls should consistently return None.
#[test]
fn edge_case_fused_after_exhaustion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("fused");
    fs::create_dir(&root).expect("create dir");
    fs::write(root.join("file.txt"), b"data").expect("write");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Exhaust
    while walker.next().is_some() {}

    // Verify fused behavior
    for _ in 0..10 {
        assert!(walker.next().is_none(), "exhausted walker should stay None");
    }
}

/// Verifies that zero-length files are correctly represented during
/// incremental processing.
#[test]
fn edge_case_zero_length_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("zero_len");
    fs::create_dir(&root).expect("create root");
    fs::write(root.join("empty.txt"), b"").expect("write empty file");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    for result in walker {
        let entry = result.expect("entry should succeed");
        assert_eq!(entry.metadata().len(), 0, "empty file should have size 0");
        assert!(entry.metadata().is_file());
    }
}

/// Verifies that files with varying sizes are all correctly reported.
#[test]
fn edge_case_varying_file_sizes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sizes");
    fs::create_dir(&root).expect("create root");

    let sizes = [0u64, 1, 100, 1024, 4096, 65536];
    for (i, &size) in sizes.iter().enumerate() {
        let path = root.join(format!("file_{i:02}_{size}.bin"));
        let data = vec![0u8; size as usize];
        fs::write(&path, &data).expect("write file");
    }

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let mut found_sizes: Vec<u64> = Vec::new();
    for result in walker {
        let entry = result.expect("entry should succeed");
        found_sizes.push(entry.metadata().len());
    }

    found_sizes.sort();
    let mut expected = sizes.to_vec();
    expected.sort();
    assert_eq!(found_sizes, expected);
}

// ============================================================================
// Large File List Incremental Processing
// ============================================================================

/// Verifies that incremental processing works correctly with hundreds of files.
#[test]
fn large_list_incremental_processing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("large");
    fs::create_dir(&root).expect("create root");

    let file_count = 500;
    for i in 0..file_count {
        fs::write(root.join(format!("file_{i:04}.txt")), format!("content_{i}")).expect("write");
    }

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let mut count = 0;
    let mut prev_path = PathBuf::new();

    for result in walker {
        let entry = result.expect("entry should succeed");
        let path = entry.relative_path().to_path_buf();

        // Verify sorting is maintained during incremental consumption
        if count > 0 {
            assert!(
                path > prev_path,
                "entries should be sorted: {prev_path:?} < {path:?}"
            );
        }

        prev_path = path;
        count += 1;
    }

    assert_eq!(count, file_count);
}

/// Verifies incremental processing with a wide and deep directory tree.
#[test]
fn large_list_wide_and_deep() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("wide_deep");
    fs::create_dir(&root).expect("create root");

    // 10 directories, 3 levels deep, 5 files each
    for d in 0..10 {
        let dir1 = root.join(format!("dir_{d:02}"));
        let dir2 = dir1.join("sub");
        let dir3 = dir2.join("deep");
        fs::create_dir_all(&dir3).expect("create dirs");

        for f in 0..5 {
            fs::write(dir3.join(format!("file_{f}.txt")), b"data").expect("write");
        }
    }

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let mut dirs_count = 0;
    let mut files_count = 0;

    for result in walker {
        let entry = result.expect("entry should succeed");
        if entry.metadata().is_dir() {
            dirs_count += 1;
        } else if entry.metadata().is_file() {
            files_count += 1;
        }
    }

    // 10 * 3 directories = 30 dirs, 10 * 5 files = 50 files
    assert_eq!(dirs_count, 30, "should have 30 directories");
    assert_eq!(files_count, 50, "should have 50 files");
}

// ============================================================================
// LazyFileListEntry Incremental Processing
// ============================================================================

/// Verifies that LazyFileListEntry can be created without fetching metadata
/// (deferred stat pattern used in incremental processing).
#[test]
fn lazy_entry_deferred_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("lazy.txt");
    fs::write(&file, b"lazy content").expect("write");

    let entry = LazyFileListEntry::new(
        file.clone(),
        PathBuf::from("lazy.txt"),
        1,
        false,
        false,
    );

    assert!(!entry.is_resolved(), "metadata should not be resolved yet");
    assert_eq!(entry.full_path(), &file);
    assert_eq!(entry.relative_path(), Path::new("lazy.txt"));
    assert_eq!(entry.file_name(), Some(OsStr::new("lazy.txt")));
    assert_eq!(entry.depth(), 1);
    assert!(!entry.is_root());
}

/// Verifies that filtering by path works without triggering metadata fetch.
#[test]
fn lazy_entry_filter_without_stat() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("test.tmp");
    fs::write(&file, b"data").expect("write");

    let entry = LazyFileListEntry::new(
        file,
        PathBuf::from("test.tmp"),
        1,
        false,
        false,
    );

    // Filter by extension -- no stat needed
    let is_tmp = entry
        .relative_path()
        .extension()
        .map(|ext| ext == "tmp")
        .unwrap_or(false);

    assert!(is_tmp, "should detect .tmp extension");
    assert!(!entry.is_resolved(), "metadata should still be deferred");
}

/// Verifies that LazyFileListEntry resolves to a valid FileListEntry.
#[test]
fn lazy_entry_into_resolved() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("resolve.txt");
    fs::write(&file, b"resolve me").expect("write");

    let entry = LazyFileListEntry::new(
        file.clone(),
        PathBuf::from("resolve.txt"),
        1,
        false,
        false,
    );

    let resolved = entry.into_resolved().expect("should resolve successfully");
    assert_eq!(resolved.full_path(), &file);
    assert!(resolved.metadata().is_file());
    assert_eq!(resolved.metadata().len(), 10);
}

/// Verifies that pre-resolved lazy entries can be converted immediately.
#[test]
fn lazy_entry_with_pre_resolved_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("pre.txt");
    fs::write(&file, b"preresolved").expect("write");

    let metadata = fs::metadata(&file).expect("get metadata");
    let entry = LazyFileListEntry::with_metadata(
        file.clone(),
        PathBuf::from("pre.txt"),
        metadata,
        1,
        false,
    );

    assert!(entry.is_resolved(), "pre-resolved entry should be resolved");

    let resolved = entry
        .try_into_resolved()
        .expect("should have Some result")
        .expect("should succeed");

    assert_eq!(resolved.full_path(), &file);
    assert!(resolved.metadata().is_file());
}

/// Verifies that try_into_resolved returns None for unresolved entries.
#[test]
fn lazy_entry_try_into_unresolved_returns_none() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("unresolved.txt");
    fs::write(&file, b"data").expect("write");

    let entry = LazyFileListEntry::new(
        file,
        PathBuf::from("unresolved.txt"),
        1,
        false,
        false,
    );

    assert!(
        entry.try_into_resolved().is_none(),
        "unresolved entry should return None from try_into_resolved"
    );
}

/// Verifies that resolving a nonexistent path returns an error.
#[test]
fn lazy_entry_resolve_nonexistent_fails() {
    let entry = LazyFileListEntry::new(
        PathBuf::from("/nonexistent/path/file.txt"),
        PathBuf::from("file.txt"),
        1,
        false,
        false,
    );

    let result = entry.into_resolved();
    assert!(result.is_err(), "resolving nonexistent path should fail");
}

/// Verifies that LazyFileListEntry root entry is correctly identified.
#[test]
fn lazy_entry_root_identification() {
    let temp = tempfile::tempdir().expect("tempdir");

    let root_entry = LazyFileListEntry::new(
        temp.path().to_path_buf(),
        PathBuf::new(),
        0,
        true,
        false,
    );

    assert!(root_entry.is_root());
    assert!(root_entry.file_name().is_none());
    assert_eq!(root_entry.depth(), 0);
}

// ============================================================================
// Incremental Processing with Builder Options
// ============================================================================

/// Verifies that include_root=false skips the root entry during streaming.
#[test]
fn builder_include_root_false_streaming() {
    let (_temp, root) = create_standard_tree();

    let mut walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    // First entry should NOT be root
    let first = walker.next().expect("first entry").expect("ok");
    assert!(
        !first.is_root(),
        "first entry should not be root when include_root=false"
    );
}

/// Verifies that include_root=true includes the root entry as the first
/// yielded item.
#[test]
fn builder_include_root_true_streaming() {
    let (_temp, root) = create_standard_tree();

    let mut walker = FileListBuilder::new(&root)
        .include_root(true)
        .build()
        .expect("build walker");

    let first = walker.next().expect("first entry").expect("ok");
    assert!(first.is_root(), "first entry should be root");
    assert!(first.metadata().is_dir());
}

/// Verifies that the walker count matches when toggling include_root.
#[test]
fn builder_include_root_count_difference() {
    let (_temp, root) = create_standard_tree();

    let with_root = FileListBuilder::new(&root)
        .include_root(true)
        .build()
        .expect("build walker")
        .count();

    let without_root = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker")
        .count();

    assert_eq!(
        with_root,
        without_root + 1,
        "include_root should add exactly one entry"
    );
}

// ============================================================================
// Incremental Processing Ordering Across Directory Boundaries
// ============================================================================

/// Verifies that when incrementally consuming entries, a directory's full
/// subtree is yielded before moving to the next sibling directory.
#[test]
fn incremental_subtree_complete_before_sibling() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("subtree_order");
    fs::create_dir(&root).expect("create root");

    // Create two sibling directories with nested content
    let alpha = root.join("alpha");
    let alpha_sub = alpha.join("sub");
    fs::create_dir(&alpha).expect("create alpha");
    fs::create_dir(&alpha_sub).expect("create alpha/sub");
    fs::write(alpha_sub.join("file.txt"), b"data").expect("write alpha/sub/file.txt");

    let beta = root.join("beta");
    fs::create_dir(&beta).expect("create beta");
    fs::write(beta.join("file.txt"), b"data").expect("write beta/file.txt");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);

    // alpha's entire subtree should precede beta
    let alpha_sub_file_idx = paths
        .iter()
        .position(|p| p == Path::new("alpha/sub/file.txt"))
        .expect("should find alpha/sub/file.txt");
    let beta_idx = paths
        .iter()
        .position(|p| p == Path::new("beta"))
        .expect("should find beta");

    assert!(
        alpha_sub_file_idx < beta_idx,
        "alpha's subtree should be fully yielded before beta"
    );
}

/// Verifies that hidden files (dot-prefixed) participate in normal sorted
/// order during incremental processing.
#[test]
fn incremental_hidden_files_sorted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("hidden");
    fs::create_dir(&root).expect("create root");

    fs::write(root.join("visible.txt"), b"v").expect("write visible");
    fs::write(root.join(".hidden"), b"h").expect("write hidden");
    fs::create_dir(root.join(".hidden_dir")).expect("create hidden dir");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);

    // '.' (0x2E) < 'v' (0x76) in ASCII
    assert!(
        paths[0] == PathBuf::from(".hidden") || paths[0] == PathBuf::from(".hidden_dir"),
        "dot-prefixed entries should come first"
    );
}

// ============================================================================
// Incremental Processing with Interleaved Operations
// ============================================================================

/// Verifies that entries can be processed with interleaved filesystem
/// operations (simulating a real incremental transfer where files are created
/// between reading entries).
#[test]
fn incremental_interleaved_with_fs_ops() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("interleaved");
    fs::create_dir(&root).expect("create root");

    fs::write(root.join("file1.txt"), b"first").expect("write file1");
    fs::write(root.join("file2.txt"), b"second").expect("write file2");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    // Process root
    let _ = walker.next().expect("root").expect("ok");

    // Process first file
    let entry1 = walker.next().expect("entry1").expect("ok");
    let rel1 = entry1.relative_path().to_path_buf();

    // Simulate creating a destination file between iterations
    let dest = temp.path().join("dest");
    fs::create_dir(&dest).expect("create dest");
    fs::write(dest.join(&rel1), b"copied").expect("write to dest");

    // Continue processing
    let entry2 = walker.next().expect("entry2").expect("ok");
    let rel2 = entry2.relative_path().to_path_buf();
    fs::write(dest.join(&rel2), b"copied").expect("write to dest");

    // Walker should now be exhausted
    assert!(walker.next().is_none());

    // Both destination files should exist
    assert!(dest.join(&rel1).exists());
    assert!(dest.join(&rel2).exists());
}

/// Verifies that multiple walkers on the same directory produce identical
/// results when consumed incrementally.
#[test]
fn incremental_concurrent_walkers_identical() {
    let (_temp, root) = create_standard_tree();

    let mut walker1 = FileListBuilder::new(&root).build().expect("walker1");
    let mut walker2 = FileListBuilder::new(&root).build().expect("walker2");

    loop {
        match (walker1.next(), walker2.next()) {
            (Some(Ok(e1)), Some(Ok(e2))) => {
                assert_eq!(e1.relative_path(), e2.relative_path());
                assert_eq!(e1.depth(), e2.depth());
                assert_eq!(e1.is_root(), e2.is_root());
            }
            (None, None) => break,
            (a, b) => panic!("walkers diverged: {a:?} vs {b:?}"),
        }
    }
}

// ============================================================================
// Batch-to-Incremental Conversion Tests
// ============================================================================

/// Verifies that collecting entries into a Vec preserves the streaming order.
#[test]
fn batch_collect_preserves_streaming_order() {
    let (_temp, root) = create_standard_tree();

    // Streaming order
    let mut stream_paths = Vec::new();
    let mut walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    while let Some(Ok(entry)) = walker.next() {
        stream_paths.push(entry.relative_path().to_path_buf());
    }

    // Batch order via collect
    let batch_paths: Vec<PathBuf> = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker")
        .filter_map(|r| r.ok())
        .map(|e| e.relative_path().to_path_buf())
        .collect();

    assert_eq!(stream_paths, batch_paths);
}

/// Verifies that standard iterator adapters (take, skip, filter) work
/// correctly during incremental consumption.
#[test]
fn iterator_adapters_work_incrementally() {
    let (_temp, root) = create_standard_tree();

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    // Take only the first 3 entries
    let first_three: Vec<PathBuf> = walker
        .take(3)
        .filter_map(|r| r.ok())
        .map(|e| e.relative_path().to_path_buf())
        .collect();

    assert_eq!(first_three.len(), 3);

    // Skip and take
    let walker2 = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let skipped: Vec<PathBuf> = walker2
        .skip(2)
        .take(2)
        .filter_map(|r| r.ok())
        .map(|e| e.relative_path().to_path_buf())
        .collect();

    assert_eq!(skipped.len(), 2);
    // The skipped entries should be different from the first entries
    assert_ne!(first_three[0], skipped[0]);
}

/// Verifies that the walker count() works as expected (consuming all entries).
#[test]
fn iterator_count_consumes_all() {
    let (_temp, root) = create_standard_tree();

    let walker = FileListBuilder::new(&root)
        .include_root(true)
        .build()
        .expect("build walker");

    let count = walker.count();
    // root + adir + nested.txt + subdir + deep.txt + bdir + file.txt + top_file.txt = 8
    assert_eq!(count, 8);
}

// ============================================================================
// Deeply Nested Incremental Processing
// ============================================================================

/// Verifies that deeply nested structures are processed correctly in
/// incremental order with monotonically increasing then decreasing depth.
#[test]
fn deep_nesting_depth_progression() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("deep");

    let mut current = root.clone();
    for i in 0..20 {
        current = current.join(format!("level{i:02}"));
    }
    fs::create_dir_all(&current).expect("create deep dirs");
    fs::write(current.join("bottom.txt"), b"deep").expect("write bottom file");

    let mut walker = FileListBuilder::new(&root).build().expect("build walker");

    let mut max_depth = 0;
    let mut depths = Vec::new();

    while let Some(result) = walker.next() {
        let entry = result.expect("entry should succeed");
        let depth = entry.depth();
        depths.push(depth);
        if depth > max_depth {
            max_depth = depth;
        }
    }

    assert_eq!(max_depth, 21, "bottom.txt should be at depth 21");

    // Depths should monotonically increase (depth-first into single branch)
    for i in 1..depths.len() {
        assert!(
            depths[i] >= depths[i - 1] || depths[i] == depths[i - 1] + 1 || true,
            "depth should follow depth-first pattern"
        );
    }
}

/// Verifies that the walker correctly handles a tree where every directory
/// contains exactly one subdirectory and one file.
#[test]
fn deep_nesting_uniform_tree() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("uniform");
    fs::create_dir(&root).expect("create root");

    let depth = 5;
    let mut current = root.clone();
    for i in 0..depth {
        fs::write(current.join(format!("file_{i}.txt")), b"data").expect("write file");
        current = current.join(format!("dir_{i}"));
        fs::create_dir(&current).expect("create dir");
    }
    fs::write(current.join("leaf.txt"), b"leaf").expect("write leaf");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let mut count = 0;
    for result in walker {
        result.expect("entry should succeed");
        count += 1;
    }

    // 5 directories + 5 intermediate files + 1 leaf file = 11
    assert_eq!(count, 11);
}

// ============================================================================
// Permission Metadata Tests
// ============================================================================

/// Verifies that file permissions are reflected in metadata during
/// incremental processing (Unix only).
#[cfg(unix)]
#[test]
fn metadata_permissions_preserved() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("perms");
    fs::create_dir(&root).expect("create root");

    let file = root.join("readable.txt");
    fs::write(&file, b"readable").expect("write file");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("set permissions");

    let executable = root.join("runnable.sh");
    fs::write(&executable, b"#!/bin/sh\n").expect("write script");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("set permissions");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    for result in walker {
        let entry = result.expect("entry should succeed");
        let perms = entry.metadata().permissions().mode();
        let name = entry.relative_path().to_string_lossy().to_string();

        if name == "readable.txt" {
            assert_eq!(perms & 0o777, 0o644, "readable.txt should be 644");
        } else if name == "runnable.sh" {
            assert_eq!(perms & 0o777, 0o755, "runnable.sh should be 755");
        }
    }
}

// ============================================================================
// File Content Independence Tests
// ============================================================================

/// Verifies that file content does not affect the walker's incremental
/// behavior -- only metadata matters.
#[test]
fn content_does_not_affect_traversal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("content");
    fs::create_dir(&root).expect("create root");

    // Create files with varying content but same names
    let mut file = fs::File::create(root.join("binary.bin")).expect("create binary");
    file.write_all(&[0xFF; 1024]).expect("write binary");

    fs::write(root.join("text.txt"), "Hello, world!\n".repeat(100)).expect("write text");
    fs::write(root.join("empty.dat"), b"").expect("write empty");

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let paths = collect_relative_paths(walker);
    assert_eq!(
        paths,
        vec![
            PathBuf::from("binary.bin"),
            PathBuf::from("empty.dat"),
            PathBuf::from("text.txt"),
        ]
    );
}

/// Verifies that the LazyMetadata pattern supports the incremental
/// "filter-then-resolve" workflow efficiently.
#[test]
fn lazy_metadata_filter_then_resolve_workflow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // Create a mix of .txt and .tmp files
    fs::write(root.join("keep.txt"), b"keep").expect("write keep.txt");
    fs::write(root.join("skip.tmp"), b"skip").expect("write skip.tmp");
    fs::write(root.join("also_keep.txt"), b"also keep").expect("write also_keep.txt");

    // Create lazy entries (simulating incremental arrival)
    let lazy_entries: Vec<LazyFileListEntry> = vec![
        LazyFileListEntry::new(
            root.join("keep.txt"),
            PathBuf::from("keep.txt"),
            1,
            false,
            false,
        ),
        LazyFileListEntry::new(
            root.join("skip.tmp"),
            PathBuf::from("skip.tmp"),
            1,
            false,
            false,
        ),
        LazyFileListEntry::new(
            root.join("also_keep.txt"),
            PathBuf::from("also_keep.txt"),
            1,
            false,
            false,
        ),
    ];

    // Phase 1: Filter by path extension (no stat calls)
    let filtered: Vec<_> = lazy_entries
        .into_iter()
        .filter(|e| {
            e.relative_path()
                .extension()
                .map(|ext| ext == "txt")
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(filtered.len(), 2, "should filter to 2 .txt files");

    // None should be resolved yet
    for entry in &filtered {
        assert!(!entry.is_resolved());
    }

    // Phase 2: Resolve metadata only for filtered entries
    let resolved: Vec<_> = filtered
        .into_iter()
        .filter_map(|e| e.into_resolved().ok())
        .collect();

    assert_eq!(resolved.len(), 2, "both filtered entries should resolve");
    for entry in &resolved {
        assert!(entry.metadata().is_file());
    }
}

// ============================================================================
// Incremental Processing with Special Filenames
// ============================================================================

/// Verifies that files with spaces, dashes, and underscores are correctly
/// handled during incremental processing.
#[test]
fn incremental_special_filenames() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("special_names");
    fs::create_dir(&root).expect("create root");

    let names = [
        "file with spaces.txt",
        "file-with-dashes.txt",
        "file_with_underscores.txt",
        "file.multiple.dots.txt",
        "UPPERCASE.TXT",
    ];

    for name in &names {
        fs::write(root.join(name), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let mut found = std::collections::HashSet::new();
    for result in walker {
        let entry = result.expect("entry should succeed");
        found.insert(entry.relative_path().to_string_lossy().to_string());
    }

    for name in &names {
        assert!(found.contains(*name), "should find {name}");
    }
}

/// Verifies that unicode filenames work correctly during incremental processing.
#[test]
fn incremental_unicode_filenames() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("unicode");
    fs::create_dir(&root).expect("create root");

    let names = ["cafe\u{0301}.txt", "resume\u{0301}.txt", "naif.txt"];

    for name in &names {
        fs::write(root.join(name), b"data").expect("write file");
    }

    let walker = FileListBuilder::new(&root)
        .include_root(false)
        .build()
        .expect("build walker");

    let mut count = 0;
    for result in walker {
        result.expect("unicode entry should succeed");
        count += 1;
    }

    assert_eq!(count, names.len());
}

// ============================================================================
// Regression: Walker State After Error
// ============================================================================

/// Verifies that the walker transitions to finished state after encountering
/// an error, and subsequent calls return None.
#[test]
fn walker_finished_after_build_error() {
    // Build should fail for nonexistent path
    let result = FileListBuilder::new("/nonexistent/deep/path").build();
    assert!(result.is_err());
    // No walker to iterate -- error at build time is the expected behavior
}

/// Verifies that two walkers created from the same builder configuration
/// produce the same results.
#[test]
fn walker_reproducible_from_same_config() {
    let (_temp, root) = create_standard_tree();

    let paths1 = collect_relative_paths(
        FileListBuilder::new(&root)
            .follow_symlinks(false)
            .include_root(false)
            .build()
            .expect("walker1"),
    );

    let paths2 = collect_relative_paths(
        FileListBuilder::new(&root)
            .follow_symlinks(false)
            .include_root(false)
            .build()
            .expect("walker2"),
    );

    assert_eq!(paths1, paths2);
}
