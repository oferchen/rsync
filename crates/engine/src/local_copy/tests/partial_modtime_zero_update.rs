// Tests for the interaction between partial file mtime=0 stamping and --update.
//
// Upstream rsync sets modtime=0 on retained partial files (cleanup.c:174-178)
// so that a subsequent run with --update never skips them - epoch is always
// older than any real source timestamp.

/// Partial file with mtime=0 (epoch) must NOT be skipped by --update.
///
/// Simulates: an interrupted transfer left a partial file whose mtime was
/// stamped to 0 (the Unix epoch). On the next run with `--update`, the
/// partial file's mtime=0 is older than every real source timestamp, so
/// the transfer proceeds and replaces the partial with full content.
#[cfg(unix)]
#[test]
fn update_transfers_file_when_dest_has_epoch_mtime() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("data.bin");
    let destination = temp.path().join("data.bin.dest");

    let full_content = b"complete file content - 32 bytes";
    let partial_content = b"partial";

    fs::write(&source, full_content).expect("write source");
    fs::write(&destination, partial_content).expect("write partial dest");

    let source_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, source_time, source_time).expect("set source times");

    // Destination has mtime=0 (epoch), simulating the PIR-3.a stamp.
    let epoch = FileTime::zero();
    set_file_times(&destination, epoch, epoch).expect("set dest epoch mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("transfer succeeds");

    // mtime=0 is older than any real source timestamp, so --update must
    // NOT skip this file.
    assert_eq!(
        summary.files_copied(),
        1,
        "partial file with mtime=0 must be re-transferred under --update"
    );
    assert_eq!(summary.regular_files_skipped_newer(), 0);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        full_content,
        "destination must contain the full source content"
    );
}

/// Partial file with a future mtime SHOULD be skipped by --update.
///
/// Negative case: if the destination has a timestamp newer than the source,
/// --update correctly skips it. This confirms the mtime=0 test above is
/// meaningful - the skip mechanism works when the dest is actually newer.
#[test]
fn update_skips_file_when_dest_has_future_mtime() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("data.bin");
    let destination = temp.path().join("data.bin.dest");

    let full_content = b"complete file content - 32 bytes";
    let partial_content = b"partial_with_padding_to_32_byte";

    fs::write(&source, full_content).expect("write source");
    fs::write(&destination, partial_content).expect("write dest");

    let source_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, source_time, source_time).expect("set source times");

    let future_time = FileTime::from_unix_time(4_102_444_800, 0);
    set_file_times(&destination, future_time, future_time).expect("set dest future mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("transfer succeeds");

    // Destination is newer, so --update must skip it.
    assert_eq!(
        summary.files_copied(),
        0,
        "file with future mtime must be skipped by --update"
    );
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        partial_content,
        "destination content must be preserved when skipped"
    );
}

/// Partial file with mtime=0 in a recursive directory transfer.
///
/// Validates the mtime=0 interaction with --update across multiple files
/// in a directory, where some have epoch mtime and others have real
/// timestamps.
#[cfg(unix)]
#[test]
fn update_recursive_transfers_epoch_mtime_files_only() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let source_time = FileTime::from_unix_time(1_700_000_000, 0);
    let future_time = FileTime::from_unix_time(1_700_000_100, 0);
    let epoch = FileTime::zero();

    // File 1: dest has mtime=0 (simulated partial) - should be transferred
    fs::write(source_root.join("partial.txt"), b"full_content_a").expect("write partial src");
    fs::write(dest_root.join("partial.txt"), b"incomplete_xxx").expect("write partial dst");
    set_file_mtime(source_root.join("partial.txt"), source_time).expect("set partial src time");
    set_file_mtime(dest_root.join("partial.txt"), epoch).expect("set partial dst epoch");

    // File 2: dest is newer than source - should be skipped
    fs::write(source_root.join("uptodate.txt"), b"source_data__").expect("write uptodate src");
    fs::write(dest_root.join("uptodate.txt"), b"newer_dest___").expect("write uptodate dst");
    set_file_mtime(source_root.join("uptodate.txt"), source_time).expect("set uptodate src time");
    set_file_mtime(dest_root.join("uptodate.txt"), future_time).expect("set uptodate dst time");

    // File 3: dest does not exist - should be transferred
    fs::write(source_root.join("newfile.txt"), b"brand new").expect("write newfile src");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true).recursive(true),
        )
        .expect("transfer succeeds");

    // partial.txt (epoch mtime) and newfile.txt (missing) should be transferred.
    // uptodate.txt (newer dest) should be skipped.
    assert_eq!(
        summary.files_copied(),
        2,
        "epoch-mtime partial + missing file should be transferred"
    );
    assert_eq!(
        summary.regular_files_skipped_newer(),
        1,
        "only the newer-dest file should be skipped"
    );

    assert_eq!(
        fs::read(dest_root.join("partial.txt")).expect("read partial"),
        b"full_content_a",
        "partial file with epoch mtime must be replaced"
    );
    assert_eq!(
        fs::read(dest_root.join("uptodate.txt")).expect("read uptodate"),
        b"newer_dest___",
        "newer dest file must be preserved"
    );
    assert_eq!(
        fs::read(dest_root.join("newfile.txt")).expect("read newfile"),
        b"brand new",
        "new file must be created"
    );
}
