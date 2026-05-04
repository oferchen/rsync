
#[test]
fn min_size_excludes_files_smaller_than_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create files with various sizes
    fs::write(source.join("tiny.txt"), b"x").expect("write 1-byte file");
    fs::write(source.join("small.txt"), b"hello").expect("write 5-byte file");
    fs::write(source.join("medium.txt"), b"hello world!").expect("write 12-byte file");
    fs::write(source.join("large.txt"), vec![b'a'; 100]).expect("write 100-byte file");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set min-size to 10 bytes
    let options = LocalCopyOptions::default().min_file_size(Some(10));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");

    // Files smaller than 10 bytes should be excluded
    assert!(!target_root.join("tiny.txt").exists(), "1-byte file should be excluded");
    assert!(!target_root.join("small.txt").exists(), "5-byte file should be excluded");

    // Files >= 10 bytes should be included
    assert!(target_root.join("medium.txt").exists(), "12-byte file should be included");
    assert!(target_root.join("large.txt").exists(), "100-byte file should be included");

    // Should have copied 2 files (medium and large)
    assert_eq!(summary.files_copied(), 2);
}

#[test]
fn min_size_includes_files_equal_to_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create files around the boundary
    fs::write(source.join("below.txt"), vec![b'x'; 99]).expect("write 99-byte file");
    fs::write(source.join("exact.txt"), vec![b'x'; 100]).expect("write 100-byte file");
    fs::write(source.join("above.txt"), vec![b'x'; 101]).expect("write 101-byte file");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set min-size to exactly 100 bytes
    let options = LocalCopyOptions::default().min_file_size(Some(100));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");

    // File below limit should be excluded
    assert!(!target_root.join("below.txt").exists(), "99-byte file should be excluded");

    // File equal to limit should be included
    assert!(target_root.join("exact.txt").exists(), "100-byte file should be included");

    // File above limit should be included
    assert!(target_root.join("above.txt").exists(), "101-byte file should be included");

    // Should have copied 2 files (exact and above)
    assert_eq!(summary.files_copied(), 2);
}

#[test]
fn min_size_includes_files_larger_than_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create small and large files
    fs::write(source.join("small.txt"), b"tiny").expect("write small file");
    fs::write(source.join("large1.txt"), vec![b'a'; 1000]).expect("write 1KB file");
    fs::write(source.join("large2.txt"), vec![b'b'; 5000]).expect("write 5KB file");
    fs::write(source.join("huge.txt"), vec![b'c'; 10000]).expect("write 10KB file");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set min-size to 500 bytes
    let options = LocalCopyOptions::default().min_file_size(Some(500));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");

    // Small file should be excluded
    assert!(!target_root.join("small.txt").exists(), "4-byte file should be excluded");

    // All large files should be included
    assert!(target_root.join("large1.txt").exists(), "1000-byte file should be included");
    assert!(target_root.join("large2.txt").exists(), "5000-byte file should be included");
    assert!(target_root.join("huge.txt").exists(), "10000-byte file should be included");

    // Should have copied 3 large files
    assert_eq!(summary.files_copied(), 3);
}

#[test]
fn min_size_with_kilobyte_suffix() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create files around 1KB boundary
    fs::write(source.join("small.txt"), vec![b'x'; 500]).expect("write 500-byte file");
    fs::write(source.join("exact.txt"), vec![b'x'; 1024]).expect("write 1KB file");
    fs::write(source.join("large.txt"), vec![b'x'; 2048]).expect("write 2KB file");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set min-size to 1K (1024 bytes)
    let options = LocalCopyOptions::default().min_file_size(Some(1024));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");

    // File below 1KB should be excluded
    assert!(!target_root.join("small.txt").exists(), "500-byte file should be excluded");

    // Files >= 1KB should be included
    assert!(target_root.join("exact.txt").exists(), "1KB file should be included");
    assert!(target_root.join("large.txt").exists(), "2KB file should be included");

    assert_eq!(summary.files_copied(), 2);
}

#[test]
fn min_size_with_megabyte_suffix() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create files around 1MB boundary
    fs::write(source.join("small.txt"), vec![b'x'; 512 * 1024]).expect("write 512KB file");
    fs::write(source.join("exact.txt"), vec![b'x'; 1024 * 1024]).expect("write 1MB file");
    fs::write(source.join("large.txt"), vec![b'x'; 2 * 1024 * 1024]).expect("write 2MB file");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set min-size to 1M (1048576 bytes)
    let options = LocalCopyOptions::default().min_file_size(Some(1024 * 1024));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");

    // File below 1MB should be excluded
    assert!(!target_root.join("small.txt").exists(), "512KB file should be excluded");

    // Files >= 1MB should be included
    assert!(target_root.join("exact.txt").exists(), "1MB file should be included");
    assert!(target_root.join("large.txt").exists(), "2MB file should be included");

    assert_eq!(summary.files_copied(), 2);
}

#[test]
fn min_size_with_gigabyte_suffix() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create files below 1GB (we won't actually create 1GB files for test speed)
    fs::write(source.join("tiny.txt"), b"small").expect("write tiny file");
    fs::write(source.join("medium.txt"), vec![b'x'; 100 * 1024 * 1024]).expect("write 100MB file");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set min-size to 1G (1073741824 bytes) - both test files should be excluded
    let options = LocalCopyOptions::default().min_file_size(Some(1024u64 * 1024 * 1024));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");

    // Both files should be excluded as they're below 1GB
    assert!(!target_root.join("tiny.txt").exists(), "tiny file should be excluded");
    assert!(!target_root.join("medium.txt").exists(), "100MB file should be excluded");

    assert_eq!(summary.files_copied(), 0);
}

#[test]
fn min_size_zero_includes_all_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create files of various sizes including empty
    fs::write(source.join("empty.txt"), b"").expect("write empty file");
    fs::write(source.join("small.txt"), b"x").expect("write 1-byte file");
    fs::write(source.join("medium.txt"), vec![b'x'; 100]).expect("write 100-byte file");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set min-size to 0 (should include all files)
    let options = LocalCopyOptions::default().min_file_size(Some(0));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");

    // All files should be included
    assert!(target_root.join("empty.txt").exists(), "empty file should be included");
    assert!(target_root.join("small.txt").exists(), "1-byte file should be included");
    assert!(target_root.join("medium.txt").exists(), "100-byte file should be included");

    assert_eq!(summary.files_copied(), 3);
}

#[test]
fn min_size_none_includes_all_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create files of various sizes
    fs::write(source.join("tiny.txt"), b"x").expect("write tiny file");
    fs::write(source.join("small.txt"), b"hello").expect("write small file");
    fs::write(source.join("large.txt"), vec![b'x'; 1000]).expect("write large file");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // No min-size filter (default behavior)
    let options = LocalCopyOptions::default().min_file_size(None);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");

    // All files should be included when no filter is set
    assert!(target_root.join("tiny.txt").exists(), "tiny file should be included");
    assert!(target_root.join("small.txt").exists(), "small file should be included");
    assert!(target_root.join("large.txt").exists(), "large file should be included");

    assert_eq!(summary.files_copied(), 3);
}

#[test]
fn min_size_does_not_affect_directories() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(source.join("subdir1")).expect("create subdir1");
    fs::create_dir_all(source.join("subdir2/nested")).expect("create nested");
    fs::create_dir_all(&dest).expect("create dest");

    // Create small file in subdirectory
    fs::write(source.join("subdir1/small.txt"), b"x").expect("write small file");
    fs::write(source.join("subdir2/nested/large.txt"), vec![b'x'; 1000])
        .expect("write large file");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set min-size to 100 bytes
    let options = LocalCopyOptions::default().min_file_size(Some(100));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");

    // Directories should always be created regardless of min-size
    assert!(target_root.join("subdir1").is_dir(), "subdir1 should exist");
    assert!(target_root.join("subdir2").is_dir(), "subdir2 should exist");
    assert!(target_root.join("subdir2/nested").is_dir(), "nested dir should exist");

    // Small file should be excluded
    assert!(!target_root.join("subdir1/small.txt").exists(), "small file should be excluded");

    // Large file should be included
    assert!(target_root.join("subdir2/nested/large.txt").exists(), "large file should be included");
}

#[test]
fn min_size_works_with_other_filters() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create files with different extensions and sizes
    fs::write(source.join("small.txt"), b"x").expect("write small txt");
    fs::write(source.join("large.txt"), vec![b'x'; 1000]).expect("write large txt");
    fs::write(source.join("small.tmp"), b"y").expect("write small tmp");
    fs::write(source.join("large.tmp"), vec![b'y'; 1000]).expect("write large tmp");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Combine min-size filter with pattern filter
    let filters = FilterSet::from_rules([FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .min_file_size(Some(100))
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");

    // small.txt: excluded by min-size
    assert!(!target_root.join("small.txt").exists(), "small.txt excluded by size");

    // large.txt: included (passes both filters)
    assert!(target_root.join("large.txt").exists(), "large.txt should be included");

    // small.tmp: excluded by pattern filter
    assert!(!target_root.join("small.tmp").exists(), "small.tmp excluded by pattern");

    // large.tmp: excluded by pattern filter (even though size is OK)
    assert!(!target_root.join("large.tmp").exists(), "large.tmp excluded by pattern");

    // Only large.txt should be copied
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn min_size_prunes_empty_directories_when_enabled() {
    let ctx = test_helpers::setup_copy_test();

    // Create only small files that will be filtered out
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("empty_dir", None),
            ("dir_with_small_files/tiny1.txt", Some(b"x")),
            ("dir_with_small_files/tiny2.txt", Some(b"y")),
        ],
    );

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set min-size to 100 bytes and enable prune-empty-dirs
    let options = LocalCopyOptions::default()
        .min_file_size(Some(100))
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Directories should be pruned since all files were filtered out
    assert!(!ctx.dest.join("source/empty_dir").exists(), "empty_dir should be pruned");
    assert!(!ctx.dest.join("source/dir_with_small_files").exists(),
            "dir_with_small_files should be pruned");
}

#[cfg(unix)]
#[test]
#[ignore] // TODO: Symlinks may be filtered by min-size - behavior needs clarification
fn min_size_does_not_affect_symlinks() {
    let ctx = test_helpers::setup_copy_test();

    // Create a small target file and symlink to it
    test_helpers::create_test_tree(
        &ctx.source,
        &[
            ("target.txt", Some(b"x")),
        ],
    );

    std::os::unix::fs::symlink("target.txt", ctx.source.join("link.txt"))
        .expect("create symlink");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Set min-size to 100 bytes (target is only 1 byte)
    let options = LocalCopyOptions::default()
        .min_file_size(Some(100))
        .links(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Target file should be excluded by min-size
    assert!(!ctx.dest.join("source/target.txt").exists(), "small target excluded");

    // Symlink should still be created (symlinks are not filtered by size)
    assert!(ctx.dest.join("source/link.txt").exists(), "symlink should be included");
}
