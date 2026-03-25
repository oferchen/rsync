//! Stress tests for incremental recursion (INC_RECURSE) with large file counts
//! and deep directory nesting.
//!
//! These tests exercise the file list batching, sorting, and progressive
//! discovery logic under load. They are gated with `#[ignore]` to avoid
//! slowing CI - run manually with:
//!
//! ```sh
//! cargo nextest run -p core --all-features -E 'test(inc_recurse_stress)' -- --ignored
//! ```
//!
//! Reference: upstream rsync 3.4.1 flist.c, io.c (INC_RECURSE)

#[cfg(unix)]
mod test_timeout;

#[cfg(unix)]
mod stress {
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    use core::client::{ClientConfig, run_client};
    use tempfile::tempdir;

    use super::test_timeout::run_with_timeout;

    /// Generous timeout for stress tests that create thousands of files.
    const STRESS_TIMEOUT: Duration = Duration::from_secs(120);

    /// Creates a file with the given content, building parent directories as needed.
    fn touch(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    /// Generates file content that is unique per index, with a size derived from
    /// the index to avoid quick-check collisions (same mtime + size = skip).
    fn unique_content(index: usize) -> Vec<u8> {
        let payload = format!("stress-payload-{index:06}");
        // Vary size by embedding index-proportional padding.
        let padding_len = (index % 256) + 1;
        let mut buf = payload.into_bytes();
        buf.extend(std::iter::repeat_n(b'#', padding_len));
        buf
    }

    /// Builds a path string from nested directory components at a given depth.
    fn nested_path(depth: usize) -> String {
        (0..depth)
            .map(|d| format!("d{d}"))
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Runs a local recursive transfer from `source` to `dest`.
    fn transfer(source: &Path, dest: &Path) -> core::client::ClientSummary {
        let mut source_arg = source.as_os_str().to_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest.as_os_str().to_os_string()])
            .mkpath(true)
            .recursive(true)
            .times(true)
            .build();

        run_client(config).expect("transfer succeeds")
    }

    /// Runs a local recursive transfer with `--delete` enabled.
    fn transfer_with_delete(source: &Path, dest: &Path) -> core::client::ClientSummary {
        let mut source_arg = source.as_os_str().to_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest.as_os_str().to_os_string()])
            .mkpath(true)
            .recursive(true)
            .times(true)
            .delete(true)
            .build();

        run_client(config).expect("transfer with delete succeeds")
    }

    /// Deep nesting (depth=50): 50 levels of nested directories, each with 2 files.
    /// Total: 100 files across 50 directory levels.
    ///
    /// Exercises incremental recursion's progressive directory discovery at extreme
    /// depth. Each level must be discovered and its entries sorted independently.
    #[test]
    #[ignore = "stress test - run manually"]
    fn deep_nesting_50_levels() {
        run_with_timeout(STRESS_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source = temp.path().join("src");
            let dest = temp.path().join("dst");

            // Create 50 levels deep, 2 files per level.
            for depth in 0..50usize {
                let dir = nested_path(depth + 1);
                for file_idx in 0..2usize {
                    let global_idx = depth * 2 + file_idx;
                    let path = source.join(format!("{dir}/file_{file_idx}.dat"));
                    touch(&path, &unique_content(global_idx));
                }
            }

            let summary = transfer(&source, &dest);

            assert!(
                summary.files_copied() >= 100,
                "expected at least 100 files copied, got {}",
                summary.files_copied()
            );

            // Verify content at several depths.
            for depth in [0, 9, 24, 49] {
                let dir = nested_path(depth + 1);
                for file_idx in 0..2usize {
                    let global_idx = depth * 2 + file_idx;
                    let rel = format!("{dir}/file_{file_idx}.dat");
                    let actual =
                        fs::read(dest.join(&rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
                    let expected = unique_content(global_idx);
                    assert_eq!(
                        actual, expected,
                        "content mismatch at depth {depth}, file {file_idx}"
                    );
                }
            }

            // Verify deepest directory exists.
            let deepest = nested_path(50);
            assert!(
                dest.join(&deepest).is_dir(),
                "deepest directory (level 50) must exist"
            );
        });
    }

    /// Wide directory (1000 files): single directory containing 1000 files of
    /// varying sizes (1B to ~256B).
    ///
    /// Exercises the file list sorting and batching when a single directory yields
    /// a large number of entries under incremental recursion.
    #[test]
    #[ignore = "stress test - run manually"]
    fn wide_directory_1000_files() {
        run_with_timeout(STRESS_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source = temp.path().join("src");
            let dest = temp.path().join("dst");

            let wide_dir = source.join("wide");
            fs::create_dir_all(&wide_dir).expect("create wide dir");

            for i in 0..1000usize {
                let name = format!("item_{i:04}.dat");
                fs::write(wide_dir.join(&name), unique_content(i)).expect("write file");
            }

            let summary = transfer(&source, &dest);

            assert!(
                summary.files_copied() >= 1000,
                "expected at least 1000 files copied, got {}",
                summary.files_copied()
            );

            // Spot-check content at boundaries and middle.
            for i in [0, 1, 499, 500, 998, 999] {
                let name = format!("wide/item_{i:04}.dat");
                let actual =
                    fs::read(dest.join(&name)).unwrap_or_else(|e| panic!("read {name}: {e}"));
                assert_eq!(
                    actual,
                    unique_content(i),
                    "content mismatch for file index {i}"
                );
            }
        });
    }

    /// Mixed deep+wide: 10 top-level directories, each 5 levels deep, each leaf
    /// directory containing 20 files. Total: 10 * 20 = 200 leaf files, plus
    /// intermediate levels.
    ///
    /// Exercises incremental recursion's handling of multiple concurrent subtree
    /// discoveries with non-trivial fan-out at the leaf level.
    #[test]
    #[ignore = "stress test - run manually"]
    fn mixed_deep_and_wide() {
        run_with_timeout(STRESS_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source = temp.path().join("src");
            let dest = temp.path().join("dst");

            let mut total_files = 0usize;
            for top in 0..10usize {
                // 5 levels deep: top/l1/l2/l3/l4
                let leaf = format!("branch_{top}/l1/l2/l3/l4");
                for f in 0..20usize {
                    let global_idx = top * 20 + f;
                    let path = source.join(format!("{leaf}/data_{f:02}.bin"));
                    touch(&path, &unique_content(global_idx));
                    total_files += 1;
                }
                // Also add a file at each intermediate level for variety.
                for level in 0..4usize {
                    let parts: Vec<&str> = vec!["l1", "l2", "l3", "l4"];
                    let intermediate: String = if level == 0 {
                        format!("branch_{top}")
                    } else {
                        format!("branch_{top}/{}", parts[..level].join("/"))
                    };
                    let idx = 200 + top * 4 + level;
                    touch(
                        &source.join(format!("{intermediate}/marker_{level}.txt")),
                        &unique_content(idx),
                    );
                    total_files += 1;
                }
            }

            let summary = transfer(&source, &dest);

            assert!(
                summary.files_copied() >= total_files as u64,
                "expected at least {total_files} files copied, got {}",
                summary.files_copied()
            );

            // Verify leaf content in a few branches.
            for top in [0, 4, 9] {
                let leaf = format!("branch_{top}/l1/l2/l3/l4");
                for f in [0, 10, 19] {
                    let global_idx = top * 20 + f;
                    let rel = format!("{leaf}/data_{f:02}.bin");
                    let actual =
                        fs::read(dest.join(&rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
                    assert_eq!(
                        actual,
                        unique_content(global_idx),
                        "content mismatch at branch {top}, file {f}"
                    );
                }
            }

            // Verify intermediate markers.
            for top in [0, 9] {
                assert!(
                    dest.join(format!("branch_{top}/marker_0.txt")).exists(),
                    "top-level marker in branch {top}"
                );
                assert!(
                    dest.join(format!("branch_{top}/l1/l2/l3/marker_3.txt"))
                        .exists(),
                    "deep marker in branch {top}"
                );
            }
        });
    }

    /// Incremental update: transfers a tree, then adds, removes, and modifies
    /// files before transferring again with `--delete`. Verifies that only the
    /// changes are applied and deletions are propagated.
    ///
    /// This tests incremental recursion's interaction with quick-check and
    /// deletion logic across multiple transfer passes.
    #[test]
    #[ignore = "stress test - run manually"]
    fn incremental_update_add_remove_modify() {
        run_with_timeout(STRESS_TIMEOUT, || {
            let temp = tempdir().expect("tempdir");
            let source = temp.path().join("src");
            let dest = temp.path().join("dst");

            // Phase 1: create initial tree with 500 files across 5 directories.
            for dir_idx in 0..5usize {
                let dir = format!("dir_{dir_idx}");
                for f in 0..100usize {
                    let global_idx = dir_idx * 100 + f;
                    let path = source.join(format!("{dir}/file_{f:03}.dat"));
                    touch(&path, &unique_content(global_idx));
                }
            }

            let summary1 = transfer(&source, &dest);
            assert!(
                summary1.files_copied() >= 500,
                "first pass: expected >= 500, got {}",
                summary1.files_copied()
            );

            // Verify a sample file from initial transfer.
            let sample_content = unique_content(42);
            assert_eq!(
                fs::read(dest.join("dir_0/file_042.dat")).unwrap(),
                sample_content,
                "initial transfer content check"
            );

            // Phase 2: mutate the source tree.
            // (a) Remove dir_4 entirely (100 files).
            fs::remove_dir_all(source.join("dir_4")).expect("remove dir_4");

            // (b) Add a new directory with 50 files.
            for f in 0..50usize {
                let path = source.join(format!("dir_new/file_{f:03}.dat"));
                // Use indices 600+ so sizes differ from existing files.
                touch(&path, &unique_content(600 + f));
            }

            // (c) Modify 10 files in dir_0 with new (different-length) content.
            for f in 0..10usize {
                let path = source.join(format!("dir_0/file_{f:03}.dat"));
                let modified = format!("MODIFIED-{f:03}-{}", "M".repeat(f * 7 + 50));
                fs::write(&path, modified.as_bytes()).expect("modify file");
            }

            // Phase 3: transfer again with --delete.
            let summary2 = transfer_with_delete(&source, &dest);

            // New files should have been copied.
            assert!(
                summary2.files_copied() >= 50,
                "second pass: expected >= 50 new files, got {}",
                summary2.files_copied()
            );

            // Verify deletions: dir_4 should be gone.
            assert!(
                !dest.join("dir_4").exists(),
                "dir_4 should be deleted from destination"
            );

            // Verify new files exist with correct content.
            for f in [0, 25, 49] {
                let rel = format!("dir_new/file_{f:03}.dat");
                let actual =
                    fs::read(dest.join(&rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
                assert_eq!(
                    actual,
                    unique_content(600 + f),
                    "new file content mismatch at index {f}"
                );
            }

            // Verify modified files have updated content.
            for f in 0..10usize {
                let rel = format!("dir_0/file_{f:03}.dat");
                let actual = fs::read_to_string(dest.join(&rel))
                    .unwrap_or_else(|e| panic!("read {rel}: {e}"));
                let expected = format!("MODIFIED-{f:03}-{}", "M".repeat(f * 7 + 50));
                assert_eq!(
                    actual, expected,
                    "modified file content mismatch at index {f}"
                );
            }

            // Verify unmodified files in dir_1 are still intact.
            let check_idx = 150usize; // dir_1, file_050
            assert_eq!(
                fs::read(dest.join("dir_1/file_050.dat")).unwrap(),
                unique_content(check_idx),
                "unmodified file should remain intact"
            );
        });
    }
}
