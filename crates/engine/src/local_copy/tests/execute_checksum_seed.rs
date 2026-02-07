
// ==================== --checksum-seed tests ====================
//
// Upstream rsync behavior:
// - Without --checksum-seed: the seed is negotiated automatically (typically
//   random) between sender and receiver.
// - With --checksum-seed=N: a fixed seed is used for all checksum computations,
//   producing deterministic results across runs. This is useful for debugging,
//   testing, and batch mode reproducibility.

// ==================== Option Unit Tests ====================

#[test]
fn checksum_seed_defaults_to_none() {
    let opts = LocalCopyOptions::default();
    assert_eq!(
        opts.checksum_seed(),
        None,
        "checksum_seed should default to None"
    );
}

#[test]
fn checksum_seed_can_be_set_to_zero() {
    let opts = LocalCopyOptions::default().with_checksum_seed(Some(0));
    assert_eq!(
        opts.checksum_seed(),
        Some(0),
        "checksum_seed should accept zero"
    );
}

#[test]
fn checksum_seed_can_be_set_to_one() {
    let opts = LocalCopyOptions::default().with_checksum_seed(Some(1));
    assert_eq!(
        opts.checksum_seed(),
        Some(1),
        "checksum_seed should accept one"
    );
}

#[test]
fn checksum_seed_can_be_set_to_max_u32() {
    let opts = LocalCopyOptions::default().with_checksum_seed(Some(u32::MAX));
    assert_eq!(
        opts.checksum_seed(),
        Some(u32::MAX),
        "checksum_seed should accept u32::MAX"
    );
}

#[test]
fn checksum_seed_typical_value() {
    let opts = LocalCopyOptions::default().with_checksum_seed(Some(12345));
    assert_eq!(
        opts.checksum_seed(),
        Some(12345),
        "checksum_seed should accept typical values"
    );
}

#[test]
fn checksum_seed_can_be_cleared() {
    let opts = LocalCopyOptions::default()
        .with_checksum_seed(Some(42))
        .with_checksum_seed(None);
    assert_eq!(
        opts.checksum_seed(),
        None,
        "checksum_seed should be clearable back to None"
    );
}

#[test]
fn checksum_seed_can_be_overwritten() {
    let opts = LocalCopyOptions::default()
        .with_checksum_seed(Some(100))
        .with_checksum_seed(Some(200));
    assert_eq!(
        opts.checksum_seed(),
        Some(200),
        "checksum_seed should use the last value set"
    );
}

#[test]
fn checksum_seed_new_defaults_to_none() {
    let opts = LocalCopyOptions::new();
    assert_eq!(
        opts.checksum_seed(),
        None,
        "LocalCopyOptions::new() should default checksum_seed to None"
    );
}

// ==================== Builder Unit Tests ====================

#[test]
fn checksum_seed_builder_defaults_to_none() {
    let opts = LocalCopyOptions::builder()
        .build()
        .expect("valid options");
    assert_eq!(
        opts.checksum_seed(),
        None,
        "builder should default checksum_seed to None"
    );
}

#[test]
fn checksum_seed_builder_can_set_value() {
    let opts = LocalCopyOptions::builder()
        .with_checksum_seed(Some(42))
        .build()
        .expect("valid options");
    assert_eq!(
        opts.checksum_seed(),
        Some(42),
        "builder should set checksum_seed"
    );
}

#[test]
fn checksum_seed_builder_can_set_zero() {
    let opts = LocalCopyOptions::builder()
        .with_checksum_seed(Some(0))
        .build()
        .expect("valid options");
    assert_eq!(
        opts.checksum_seed(),
        Some(0),
        "builder should accept zero seed"
    );
}

#[test]
fn checksum_seed_builder_can_set_max_u32() {
    let opts = LocalCopyOptions::builder()
        .with_checksum_seed(Some(u32::MAX))
        .build()
        .expect("valid options");
    assert_eq!(
        opts.checksum_seed(),
        Some(u32::MAX),
        "builder should accept u32::MAX seed"
    );
}

#[test]
fn checksum_seed_builder_can_clear() {
    let opts = LocalCopyOptions::builder()
        .with_checksum_seed(Some(42))
        .with_checksum_seed(None)
        .build()
        .expect("valid options");
    assert_eq!(
        opts.checksum_seed(),
        None,
        "builder should clear checksum_seed with None"
    );
}

#[test]
fn checksum_seed_builder_can_overwrite() {
    let opts = LocalCopyOptions::builder()
        .with_checksum_seed(Some(100))
        .with_checksum_seed(Some(200))
        .build()
        .expect("valid options");
    assert_eq!(
        opts.checksum_seed(),
        Some(200),
        "builder should use the last checksum_seed value"
    );
}

#[test]
fn checksum_seed_build_unchecked_works() {
    let opts = LocalCopyOptions::builder()
        .with_checksum_seed(Some(999))
        .build_unchecked();
    assert_eq!(
        opts.checksum_seed(),
        Some(999),
        "build_unchecked should preserve checksum_seed"
    );
}

// ==================== Combination Tests ====================

#[test]
fn checksum_seed_compatible_with_checksum_mode() {
    let opts = LocalCopyOptions::builder()
        .checksum(true)
        .with_checksum_seed(Some(42))
        .build()
        .expect("valid options");
    assert!(opts.checksum_enabled());
    assert_eq!(opts.checksum_seed(), Some(42));
}

#[test]
fn checksum_seed_compatible_with_delete() {
    let opts = LocalCopyOptions::builder()
        .delete(true)
        .with_checksum_seed(Some(42))
        .build()
        .expect("valid options");
    assert!(opts.delete_extraneous());
    assert_eq!(opts.checksum_seed(), Some(42));
}

#[test]
fn checksum_seed_compatible_with_archive() {
    let opts = LocalCopyOptions::builder()
        .archive()
        .with_checksum_seed(Some(42))
        .build()
        .expect("valid options");
    assert!(opts.recursive_enabled());
    assert!(opts.preserve_times());
    assert_eq!(opts.checksum_seed(), Some(42));
}

#[test]
fn checksum_seed_without_checksum_mode() {
    // --checksum-seed can be set even without --checksum; the seed still
    // applies to block-level checksums used during delta transfers.
    let opts = LocalCopyOptions::default().with_checksum_seed(Some(42));
    assert!(!opts.checksum_enabled());
    assert_eq!(opts.checksum_seed(), Some(42));
}

// ==================== Functional Tests ====================
//
// These tests verify that the local copy engine works correctly when
// checksum_seed is configured.

#[test]
fn transfer_works_with_checksum_seed_set() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("file.txt"), b"hello world").expect("write file");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().with_checksum_seed(Some(42));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest.join("file.txt").exists(), "file should be copied");
    assert_eq!(
        fs::read(dest.join("file.txt")).expect("read"),
        b"hello world",
        "content should match"
    );
    assert!(summary.files_copied() >= 1, "should report file copied");
}

#[test]
fn transfer_works_with_checksum_seed_zero() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("data.bin"), b"binary data here").expect("write file");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().with_checksum_seed(Some(0));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds with seed=0");

    assert!(dest.join("data.bin").exists(), "file should be copied");
    assert_eq!(
        fs::read(dest.join("data.bin")).expect("read"),
        b"binary data here"
    );
    assert!(summary.files_copied() >= 1);
}

#[test]
fn transfer_works_with_checksum_seed_max() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("max.txt"), b"max seed test").expect("write file");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().with_checksum_seed(Some(u32::MAX));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds with seed=u32::MAX");

    assert!(dest.join("max.txt").exists(), "file should be copied");
    assert_eq!(
        fs::read(dest.join("max.txt")).expect("read"),
        b"max seed test"
    );
    assert!(summary.files_copied() >= 1);
}

#[test]
fn transfer_multiple_files_with_checksum_seed() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("a.txt"), b"alpha").expect("write a");
    fs::write(source.join("b.txt"), b"bravo").expect("write b");
    fs::write(source.join("c.txt"), b"charlie").expect("write c");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().with_checksum_seed(Some(7777));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest.join("a.txt").exists());
    assert!(dest.join("b.txt").exists());
    assert!(dest.join("c.txt").exists());
    assert_eq!(fs::read(dest.join("a.txt")).expect("read"), b"alpha");
    assert_eq!(fs::read(dest.join("b.txt")).expect("read"), b"bravo");
    assert_eq!(fs::read(dest.join("c.txt")).expect("read"), b"charlie");
    assert!(summary.files_copied() >= 3);
}

#[test]
fn transfer_with_checksum_seed_and_checksum_mode() {
    // Combining --checksum and --checksum-seed should work: the seed is used
    // for whole-file checksum comparisons.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("check.txt"), b"checksum test").expect("write file");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .checksum(true)
        .with_checksum_seed(Some(54321));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds with checksum + seed");

    assert!(dest.join("check.txt").exists());
    assert_eq!(
        fs::read(dest.join("check.txt")).expect("read"),
        b"checksum test"
    );
    assert!(summary.files_copied() >= 1);
}

#[test]
fn transfer_with_checksum_seed_dry_run() {
    // Dry-run with --checksum-seed should not modify disk but still report.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("dry.txt"), b"dry run").expect("write file");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .with_checksum_seed(Some(42))
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    let summary = report.summary();

    // File should not exist on disk (dry-run)
    assert!(
        !dest.join("dry.txt").exists(),
        "file should not exist in dry-run"
    );

    // But the summary should report what would happen
    assert!(
        summary.files_copied() >= 1,
        "dry-run should report files that would be copied"
    );
}

#[test]
fn transfer_with_checksum_seed_and_delete() {
    // --checksum-seed combined with --delete should work correctly.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest.join("keep.txt"), b"old keep").expect("write old keep");
    fs::write(dest.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete(true)
        .with_checksum_seed(Some(42));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest.join("keep.txt").exists(), "kept file should remain");
    assert!(
        !dest.join("extra.txt").exists(),
        "extra file should be deleted"
    );
    assert!(summary.items_deleted() >= 1, "should report deletion");
}

// ==================== Round-Trip Tests ====================

#[test]
fn checksum_seed_round_trip_direct() {
    // Set via direct method, read back via accessor
    for seed in [0u32, 1, 42, 12345, 999_999, u32::MAX] {
        let opts = LocalCopyOptions::default().with_checksum_seed(Some(seed));
        assert_eq!(
            opts.checksum_seed(),
            Some(seed),
            "round-trip failed for seed {seed}"
        );
    }
}

#[test]
fn checksum_seed_round_trip_builder() {
    // Set via builder, read back via accessor
    for seed in [0u32, 1, 42, 12345, 999_999, u32::MAX] {
        let opts = LocalCopyOptions::builder()
            .with_checksum_seed(Some(seed))
            .build()
            .expect("valid options");
        assert_eq!(
            opts.checksum_seed(),
            Some(seed),
            "builder round-trip failed for seed {seed}"
        );
    }
}

#[test]
fn checksum_seed_round_trip_builder_unchecked() {
    // Set via builder, build_unchecked, read back via accessor
    for seed in [0u32, 1, 42, 12345, 999_999, u32::MAX] {
        let opts = LocalCopyOptions::builder()
            .with_checksum_seed(Some(seed))
            .build_unchecked();
        assert_eq!(
            opts.checksum_seed(),
            Some(seed),
            "build_unchecked round-trip failed for seed {seed}"
        );
    }
}

#[test]
fn checksum_seed_none_round_trip() {
    let opts = LocalCopyOptions::default().with_checksum_seed(None);
    assert_eq!(opts.checksum_seed(), None);

    let opts = LocalCopyOptions::builder()
        .with_checksum_seed(None)
        .build()
        .expect("valid options");
    assert_eq!(opts.checksum_seed(), None);

    let opts = LocalCopyOptions::builder()
        .with_checksum_seed(None)
        .build_unchecked();
    assert_eq!(opts.checksum_seed(), None);
}
