
// ==================== Option plumbing tests ====================

#[test]
fn block_size_override_default_is_none() {
    let opts = LocalCopyOptions::default();
    assert!(
        opts.block_size_override().is_none(),
        "default block_size_override should be None (auto-calculate)"
    );
}

#[test]
fn block_size_override_setter_sets_value() {
    let size = NonZeroU32::new(2048).unwrap();
    let opts = LocalCopyOptions::default().with_block_size_override(Some(size));
    assert_eq!(opts.block_size_override(), Some(size));
}

#[test]
fn block_size_override_setter_clears_with_none() {
    let size = NonZeroU32::new(4096).unwrap();
    let opts = LocalCopyOptions::default()
        .with_block_size_override(Some(size))
        .with_block_size_override(None);
    assert!(opts.block_size_override().is_none());
}

#[test]
fn block_size_builder_sets_override() {
    let size = NonZeroU32::new(8192).unwrap();
    let opts = LocalCopyOptions::builder()
        .block_size(Some(size))
        .build()
        .expect("valid options");
    assert_eq!(opts.block_size_override(), Some(size));
}

#[test]
fn block_size_builder_default_is_none() {
    let opts = LocalCopyOptions::builder().build().expect("valid options");
    assert!(opts.block_size_override().is_none());
}

#[test]
fn block_size_builder_none_clears_override() {
    let size = NonZeroU32::new(1024).unwrap();
    let opts = LocalCopyOptions::builder()
        .block_size(Some(size))
        .block_size(None)
        .build()
        .expect("valid options");
    assert!(opts.block_size_override().is_none());
}

// ==================== Delta transfer integration tests ====================

#[test]
fn block_size_override_affects_delta_matching_small_block() {
    // Use a small custom block size to increase the number of blocks,
    // which should still produce correct delta matches.
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let dest_path = temp.path().join("dest.bin");

    // Create data with a common prefix (4096 bytes) and differing suffix (2048 bytes).
    let common = vec![b'A'; 4096];
    let old_tail = vec![b'B'; 2048];
    let new_tail = vec![b'C'; 2048];

    let mut dest_content = common.clone();
    dest_content.extend_from_slice(&old_tail);
    fs::write(&dest_path, &dest_content).expect("write destination");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("set dest mtime");

    let mut source_content = common.clone();
    source_content.extend_from_slice(&new_tail);
    fs::write(&source_path, &source_content).expect("write source");
    set_file_mtime(&source_path, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Use a 512-byte block size override (much smaller than the default 700).
    let small_block = NonZeroU32::new(512).unwrap();
    let opts = LocalCopyOptions::default()
        .whole_file(false)
        .with_block_size_override(Some(small_block));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, opts)
        .expect("delta copy with small block size succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(
        summary.matched_bytes() > 0,
        "delta transfer should match common blocks with small block size"
    );
    assert_eq!(
        fs::read(&dest_path).expect("read destination"),
        source_content,
        "destination content must match source"
    );
    assert_eq!(
        summary.bytes_copied() + summary.matched_bytes(),
        source_content.len() as u64,
        "copied + matched should equal total file size"
    );
}

#[test]
fn block_size_override_affects_delta_matching_large_block() {
    // Use a large custom block size. With a block size equal to the total
    // file size, delta matching will treat the file as a single block.
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let dest_path = temp.path().join("dest.bin");

    // Create files: 8192 bytes each. Source and dest share content.
    let common_data = vec![b'X'; 4096];
    let old_tail = vec![b'Y'; 4096];
    let new_tail = vec![b'Z'; 4096];

    let mut dest_content = common_data.clone();
    dest_content.extend_from_slice(&old_tail);
    fs::write(&dest_path, &dest_content).expect("write destination");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("set dest mtime");

    let mut source_content = common_data.clone();
    source_content.extend_from_slice(&new_tail);
    fs::write(&source_path, &source_content).expect("write source");
    set_file_mtime(&source_path, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Block size larger than common prefix => the common prefix will span a
    // single block boundary differently than with default.
    let large_block = NonZeroU32::new(8192).unwrap();
    let opts = LocalCopyOptions::default()
        .whole_file(false)
        .with_block_size_override(Some(large_block));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, opts)
        .expect("delta copy with large block size succeeds");

    assert_eq!(summary.files_copied(), 1);
    // With block size = file size, the whole file is one block. If the block
    // changed it won't match, so we just verify correctness.
    assert_eq!(
        fs::read(&dest_path).expect("read destination"),
        source_content,
        "destination content must match source"
    );
    assert_eq!(
        summary.bytes_copied() + summary.matched_bytes(),
        source_content.len() as u64,
        "copied + matched should equal total file size"
    );
}

#[test]
fn block_size_override_produces_correct_content_for_identical_files() {
    // When source and destination are identical, delta transfer with
    // any block size should match everything and copy nothing.
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let dest_path = temp.path().join("dest.bin");

    let content = vec![b'Q'; 8192];
    fs::write(&dest_path, &content).expect("write destination");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("set dest mtime");

    fs::write(&source_path, &content).expect("write source");
    set_file_mtime(&source_path, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];

    for block_size_val in [128, 256, 512, 1024, 2048, 4096] {
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let block = NonZeroU32::new(block_size_val).unwrap();
        let opts = LocalCopyOptions::default()
            .whole_file(false)
            .ignore_times(true)
            .with_block_size_override(Some(block));

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, opts)
            .expect("delta copy with identical files succeeds");

        assert_eq!(summary.files_copied(), 1);
        assert_eq!(
            summary.matched_bytes(),
            content.len() as u64,
            "block_size={block_size_val}: identical files should fully match"
        );
        assert_eq!(
            summary.bytes_copied(),
            0,
            "block_size={block_size_val}: identical files should copy 0 bytes"
        );
        assert_eq!(
            fs::read(&dest_path).expect("read destination"),
            content,
            "block_size={block_size_val}: content must remain identical"
        );
    }
}

#[test]
fn block_size_override_vs_auto_both_produce_correct_output() {
    // Verify that using a custom block size and auto-calculated block size
    // both produce the same output file, even though delta match statistics
    // may differ.
    let temp = tempdir().expect("tempdir");

    let common = vec![b'D'; 4096];
    let extra = vec![b'E'; 2048];

    let mut dest_content = common.clone();
    dest_content.extend_from_slice(&vec![b'F'; 2048]);

    let mut source_content = common.clone();
    source_content.extend_from_slice(&extra);

    // Run with auto block size
    let auto_dest = temp.path().join("auto_dest.bin");
    let auto_source = temp.path().join("auto_source.bin");
    fs::write(&auto_dest, &dest_content).expect("write auto dest");
    fs::write(&auto_source, &source_content).expect("write auto source");
    set_file_mtime(&auto_dest, FileTime::from_unix_time(1, 0)).expect("set mtime");
    set_file_mtime(&auto_source, FileTime::from_unix_time(2, 0)).expect("set mtime");

    let auto_operands = vec![
        auto_source.into_os_string(),
        auto_dest.clone().into_os_string(),
    ];
    let auto_plan = LocalCopyPlan::from_operands(&auto_operands).expect("plan");
    let auto_summary = auto_plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(false),
        )
        .expect("auto block size succeeds");

    // Run with override block size
    let override_dest = temp.path().join("override_dest.bin");
    let override_source = temp.path().join("override_source.bin");
    fs::write(&override_dest, &dest_content).expect("write override dest");
    fs::write(&override_source, &source_content).expect("write override source");
    set_file_mtime(&override_dest, FileTime::from_unix_time(1, 0)).expect("set mtime");
    set_file_mtime(&override_source, FileTime::from_unix_time(2, 0)).expect("set mtime");

    let override_operands = vec![
        override_source.into_os_string(),
        override_dest.clone().into_os_string(),
    ];
    let override_plan = LocalCopyPlan::from_operands(&override_operands).expect("plan");
    let block = NonZeroU32::new(256).unwrap();
    let override_summary = override_plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .whole_file(false)
                .with_block_size_override(Some(block)),
        )
        .expect("override block size succeeds");

    // Both must produce identical output
    let auto_result = fs::read(&auto_dest).expect("read auto dest");
    let override_result = fs::read(&override_dest).expect("read override dest");
    assert_eq!(
        auto_result, override_result,
        "auto and override block sizes must produce identical output"
    );
    assert_eq!(auto_result, source_content);

    // Both should have used delta transfer (not whole file)
    assert!(
        auto_summary.matched_bytes() > 0,
        "auto should use delta matching"
    );
    assert!(
        override_summary.matched_bytes() > 0,
        "override should use delta matching"
    );
}

#[test]
fn block_size_override_with_builder_integration() {
    // Verify block_size flows correctly through the builder -> options -> execution path.
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let dest_path = temp.path().join("dest.bin");

    let common = vec![b'G'; 4096];
    let old_tail = vec![b'H'; 2048];
    let new_tail = vec![b'I'; 2048];

    let mut dest_content = common.clone();
    dest_content.extend_from_slice(&old_tail);
    fs::write(&dest_path, &dest_content).expect("write destination");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("set dest mtime");

    let mut source_content = common.clone();
    source_content.extend_from_slice(&new_tail);
    fs::write(&source_path, &source_content).expect("write source");
    set_file_mtime(&source_path, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let block = NonZeroU32::new(1024).unwrap();
    let opts = LocalCopyOptions::builder()
        .whole_file(false)
        .block_size(Some(block))
        .build()
        .expect("valid options");

    assert_eq!(opts.block_size_override(), Some(block));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, opts)
        .expect("builder block size copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(
        summary.matched_bytes() > 0,
        "builder-configured block size should enable delta matching"
    );
    assert_eq!(
        fs::read(&dest_path).expect("read destination"),
        source_content,
        "destination content must match source"
    );
}

#[test]
fn block_size_override_with_new_destination_file() {
    // When the destination does not exist, block_size_override should not
    // interfere -- the file is simply copied in full.
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let dest_path = temp.path().join("dest.bin");

    let content = vec![b'K'; 4096];
    fs::write(&source_path, &content).expect("write source");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let block = NonZeroU32::new(512).unwrap();
    let opts = LocalCopyOptions::default()
        .whole_file(false)
        .with_block_size_override(Some(block));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, opts)
        .expect("copy to new destination succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), content.len() as u64);
    assert_eq!(summary.matched_bytes(), 0, "no basis file means no matches");
    assert_eq!(
        fs::read(&dest_path).expect("read destination"),
        content,
        "new file must be correct"
    );
}

#[test]
fn block_size_override_with_empty_destination() {
    // An empty destination file means no useful basis for delta matching.
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let dest_path = temp.path().join("dest.bin");

    let content = vec![b'L'; 4096];
    fs::write(&source_path, &content).expect("write source");
    fs::write(&dest_path, b"").expect("write empty destination");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("set dest mtime");
    set_file_mtime(&source_path, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let block = NonZeroU32::new(1024).unwrap();
    let opts = LocalCopyOptions::default()
        .whole_file(false)
        .with_block_size_override(Some(block));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, opts)
        .expect("copy with empty destination succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), content.len() as u64);
    assert_eq!(summary.matched_bytes(), 0, "empty basis yields no matches");
    assert_eq!(
        fs::read(&dest_path).expect("read destination"),
        content,
        "destination content must match source"
    );
}

#[test]
fn block_size_override_does_not_affect_whole_file_mode() {
    // When whole_file is enabled, block_size_override should be irrelevant --
    // the file is transferred in full without delta matching.
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let dest_path = temp.path().join("dest.bin");

    let common = vec![b'M'; 4096];
    let extra = vec![b'N'; 2048];

    let mut dest_content = common.clone();
    dest_content.extend_from_slice(&vec![b'O'; 2048]);
    fs::write(&dest_path, &dest_content).expect("write destination");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("set dest mtime");

    let mut source_content = common.clone();
    source_content.extend_from_slice(&extra);
    fs::write(&source_path, &source_content).expect("write source");
    set_file_mtime(&source_path, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let block = NonZeroU32::new(256).unwrap();
    let opts = LocalCopyOptions::default()
        .whole_file(true)
        .with_block_size_override(Some(block));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, opts)
        .expect("whole file with block_size_override succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), source_content.len() as u64);
    assert_eq!(
        summary.matched_bytes(),
        0,
        "whole file mode should not use delta matching despite block_size_override"
    );
    assert_eq!(
        fs::read(&dest_path).expect("read destination"),
        source_content,
    );
}

#[test]
fn block_size_override_recursive_directory_copy() {
    // Verify block_size_override works in a recursive directory copy scenario.
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let dest_source = dest_root.join("source");
    fs::create_dir_all(&dest_source).expect("create dest source dir");

    // Create file pairs with shared prefixes
    let common_a = vec![b'A'; 2048];
    let common_b = vec![b'B'; 2048];

    let mut old_a = common_a.clone();
    old_a.extend_from_slice(&vec![b'X'; 1024]);
    let mut new_a = common_a.clone();
    new_a.extend_from_slice(&vec![b'Y'; 1024]);

    let mut old_b = common_b.clone();
    old_b.extend_from_slice(&vec![b'U'; 1024]);
    let mut new_b = common_b.clone();
    new_b.extend_from_slice(&vec![b'V'; 1024]);

    // Existing destination files (old versions)
    fs::write(dest_source.join("a.bin"), &old_a).expect("write dest a");
    fs::write(dest_source.join("b.bin"), &old_b).expect("write dest b");
    set_file_mtime(dest_source.join("a.bin"), FileTime::from_unix_time(1, 0)).expect("set mtime");
    set_file_mtime(dest_source.join("b.bin"), FileTime::from_unix_time(1, 0)).expect("set mtime");

    // New source files
    fs::write(source_root.join("a.bin"), &new_a).expect("write source a");
    fs::write(source_root.join("b.bin"), &new_b).expect("write source b");
    set_file_mtime(source_root.join("a.bin"), FileTime::from_unix_time(2, 0)).expect("set mtime");
    set_file_mtime(source_root.join("b.bin"), FileTime::from_unix_time(2, 0)).expect("set mtime");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let block = NonZeroU32::new(512).unwrap();
    let opts = LocalCopyOptions::default()
        .whole_file(false)
        .with_block_size_override(Some(block));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, opts)
        .expect("recursive delta copy with block_size_override succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert!(
        summary.matched_bytes() > 0,
        "recursive delta should match common blocks"
    );
    assert_eq!(
        fs::read(dest_root.join("source/a.bin")).expect("read a"),
        new_a
    );
    assert_eq!(
        fs::read(dest_root.join("source/b.bin")).expect("read b"),
        new_b
    );
}

#[test]
fn block_size_override_different_sizes_all_produce_correct_output() {
    // Sweep through a range of block sizes and verify output correctness.
    let temp = tempdir().expect("tempdir");

    let common = vec![b'S'; 4096];
    let old_tail = vec![b'T'; 2048];
    let new_tail = vec![b'U'; 2048];

    let mut dest_template = common.clone();
    dest_template.extend_from_slice(&old_tail);

    let mut source_content = common.clone();
    source_content.extend_from_slice(&new_tail);

    for block_size_val in [64, 128, 256, 512, 700, 1024, 2048, 4096, 8192] {
        let src = temp.path().join(format!("src_{block_size_val}.bin"));
        let dst = temp.path().join(format!("dst_{block_size_val}.bin"));

        fs::write(&dst, &dest_template).expect("write dest");
        fs::write(&src, &source_content).expect("write source");
        set_file_mtime(&dst, FileTime::from_unix_time(1, 0)).expect("set mtime");
        set_file_mtime(&src, FileTime::from_unix_time(2, 0)).expect("set mtime");

        let operands = vec![src.into_os_string(), dst.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let block = NonZeroU32::new(block_size_val).unwrap();
        let opts = LocalCopyOptions::default()
            .whole_file(false)
            .with_block_size_override(Some(block));

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, opts)
            .unwrap_or_else(|_| panic!("block_size={block_size_val} copy succeeds"));

        assert_eq!(
            fs::read(&dst).expect("read dest"),
            source_content,
            "block_size={block_size_val}: output must match source"
        );
        assert_eq!(
            summary.bytes_copied() + summary.matched_bytes(),
            source_content.len() as u64,
            "block_size={block_size_val}: copied + matched must equal file size"
        );
    }
}
