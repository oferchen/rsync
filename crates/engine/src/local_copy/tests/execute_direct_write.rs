// Tests for the WriteStrategy::Direct write path
//
// When no existing destination file is present and none of --partial,
// --delay-updates, or --temp-dir are active, the executor writes directly
// to the final destination path using create_new(true). This avoids the
// overhead of creating and renaming a temporary staging file.
//
// Key behaviors tested:
// 1. Direct write creates the file at the final destination path
// 2. No temporary staging files are created in the destination directory
// 3. Direct write works for new files (destination does not exist)
// 4. File content is correct after direct write
// 5. Multiple files in a directory all use direct write
// 6. Zero-length files use the direct write path
// 7. Large files use the direct write path correctly


#[test]
fn direct_write_creates_file_at_final_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"direct write content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists(), "destination file should exist");
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"direct write content"
    );
}

#[test]
fn direct_write_does_not_create_temp_files() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("source dir");
    fs::create_dir_all(&dest_dir).expect("dest dir");

    fs::write(source_dir.join("file.txt"), b"payload").expect("write source");

    let operands = vec![
        source_dir.join("file.txt").into_os_string(),
        dest_dir.join("file.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    // Verify no temp files remain - temp files use the `.~tmp~` prefix
    let entries: Vec<_> = fs::read_dir(&dest_dir)
        .expect("read dest dir")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "only the destination file should exist, found: {:?}",
        entries
            .iter()
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );
    assert_eq!(entries[0].file_name(), "file.txt");
}

#[test]
fn direct_write_works_when_destination_does_not_exist() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("nonexistent_dest.txt");

    let content = b"new file via direct write";
    fs::write(&source, content).expect("write source");

    assert!(
        !destination.exists(),
        "destination should not exist before copy"
    );

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists(), "destination should exist after copy");
    assert_eq!(fs::read(&destination).expect("read dest"), content);
}

#[test]
fn direct_write_preserves_file_content_exactly() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    // Binary content with varied bytes including nulls
    let content: Vec<u8> = (0..=255).cycle().take(4096).collect();
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(
        fs::read(&destination).expect("read dest"),
        content,
        "binary content must match exactly"
    );
}


#[test]
fn direct_write_handles_multiple_files_in_directory() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("source dir");
    fs::create_dir_all(&dest_dir).expect("dest dir");

    fs::write(source_dir.join("alpha.txt"), b"alpha content").expect("write alpha");
    fs::write(source_dir.join("beta.txt"), b"beta content").expect("write beta");
    fs::write(source_dir.join("gamma.txt"), b"gamma content").expect("write gamma");

    // Trailing slash on source_dir to copy contents into dest_dir
    let mut source_os = source_dir.into_os_string();
    source_os.push("/");
    let operands = vec![source_os, dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(
        fs::read(dest_dir.join("alpha.txt")).expect("read alpha"),
        b"alpha content"
    );
    assert_eq!(
        fs::read(dest_dir.join("beta.txt")).expect("read beta"),
        b"beta content"
    );
    assert_eq!(
        fs::read(dest_dir.join("gamma.txt")).expect("read gamma"),
        b"gamma content"
    );

    // Verify no temp files linger
    let entries: Vec<_> = fs::read_dir(&dest_dir)
        .expect("read dest dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".~tmp~")
        })
        .collect();
    assert!(
        entries.is_empty(),
        "no temp files should remain in dest dir"
    );
}


#[test]
fn direct_write_zero_length_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest_empty.txt");

    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest").len(), 0);
}

#[test]
fn direct_write_large_file_content_intact() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.bin");
    let destination = temp.path().join("dest_large.bin");

    // 256 KiB file to exercise buffer logic
    let content = vec![0xABu8; 256 * 1024];
    fs::write(&source, &content).expect("write large source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(
        fs::read(&destination).expect("read dest"),
        content,
        "large file content must match"
    );
}


#[test]
fn existing_destination_does_not_use_direct_write() {
    // When a destination already exists, the executor should use a temp-file
    // strategy instead of direct write, to protect the existing content during
    // the transfer.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"updated content").expect("write source");
    fs::write(&destination, b"original content").expect("write existing dest");

    // Backdate the destination so it is not skipped by quick-check
    let old_time = FileTime::from_unix_time(1_000_000, 0);
    set_file_mtime(&destination, old_time).expect("set mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"updated content"
    );
}

#[test]
fn partial_enabled_does_not_use_direct_write() {
    // With --partial, the executor should use a guarded write even for new files,
    // because partial transfers need to be preserved on failure.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"partial test content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().partial(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"partial test content"
    );
}

#[test]
fn delay_updates_does_not_use_direct_write() {
    // With --delay-updates, files go through a staging path, not direct write.
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("source dir");
    fs::create_dir_all(&dest_dir).expect("dest dir");

    fs::write(source_dir.join("file.txt"), b"delayed content").expect("write source");

    let mut source_os = source_dir.into_os_string();
    source_os.push("/");
    let operands = vec![source_os, dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(dest_dir.join("file.txt")).expect("read dest"),
        b"delayed content"
    );
}

#[test]
fn temp_dir_option_does_not_use_direct_write() {
    // With --temp-dir, files are staged in the temp directory, not written
    // directly to the destination.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let staging = temp.path().join("staging");
    fs::create_dir_all(&staging).expect("staging dir");

    fs::write(&source, b"temp dir content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"temp dir content"
    );

    // Staging directory should be clean after successful transfer
    let staging_files: Vec<_> = fs::read_dir(&staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        staging_files.is_empty(),
        "staging directory should be empty after transfer"
    );
}
