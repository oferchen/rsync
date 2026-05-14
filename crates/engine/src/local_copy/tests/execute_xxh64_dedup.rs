// Tests for the internal-only xxh64 file-dedup heuristic.
//
// The heuristic is opt-in via `LocalCopyOptions::enable_xxh64_dedup` and
// never affects the wire protocol. When identical content is detected by
// xxh64, the receiver bypasses the rolling+strong delta pipeline and
// records a metadata-only sync (the same outcome as `try_skip_up_to_date`
// would produce on an exact match). When content differs, the heuristic
// falls through to the normal delta path.

#[test]
fn xxh64_dedup_skips_delta_when_files_match() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    let payload = vec![0xA5u8; 32 * 1024];
    fs::write(&source, &payload).expect("write source");
    fs::write(&destination, &payload).expect("write destination");

    // Backdate the destination so the quick-check (size+mtime) does not
    // already short-circuit the transfer. We want the xxh64 heuristic to
    // be the deciding factor.
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("source mtime");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("dest mtime");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .whole_file(false)
        .enable_xxh64_dedup(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execute succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.bytes_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), payload);
}

#[test]
fn xxh64_dedup_falls_through_when_files_differ() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    let mut source_bytes = vec![0u8; 4096];
    for (offset, byte) in source_bytes.iter_mut().enumerate() {
        *byte = (offset & 0xFF) as u8;
    }
    let mut dest_bytes = source_bytes.clone();
    dest_bytes[2048] ^= 0xFF;
    fs::write(&source, &source_bytes).expect("write source");
    fs::write(&destination, &dest_bytes).expect("write destination");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("source mtime");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("dest mtime");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .whole_file(false)
        .enable_xxh64_dedup(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execute succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    assert_eq!(fs::read(&destination).expect("read dest"), source_bytes);
}

#[test]
fn xxh64_dedup_skipped_when_file_exceeds_size_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    // Use identical content but configure a tight size limit so the
    // heuristic does not run. The normal delta path then handles the
    // transfer (and finds a full block match, copying zero literal bytes).
    let payload = vec![0xCDu8; 8 * 1024];
    fs::write(&source, &payload).expect("write source");
    fs::write(&destination, &payload).expect("write destination");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("source mtime");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("dest mtime");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .whole_file(false)
        .enable_xxh64_dedup(true)
        .with_xxh64_dedup_size_limit(1024);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execute succeeds");

    // When the heuristic is skipped, the delta path runs. Identical
    // content produces a full block match, so the transfer reports a
    // copy event with zero literal bytes (the destination data is
    // reconstructed from the basis blocks).
    assert_eq!(summary.regular_files_matched(), 0);
    assert_eq!(summary.bytes_copied(), 0);
    assert_eq!(fs::read(&destination).expect("read dest"), payload);
}

#[test]
fn xxh64_dedup_disabled_runs_full_delta_path() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    let payload = vec![0x33u8; 16 * 1024];
    fs::write(&source, &payload).expect("write source");
    fs::write(&destination, &payload).expect("write destination");
    set_file_mtime(&source, FileTime::from_unix_time(2, 0)).expect("source mtime");
    set_file_mtime(&destination, FileTime::from_unix_time(1, 0)).expect("dest mtime");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Heuristic disabled (the default). With identical content but
    // differing mtimes, the delta path runs and produces zero literal
    // bytes (every block matches the basis).
    let options = LocalCopyOptions::default().whole_file(false);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execute succeeds");

    assert_eq!(summary.regular_files_matched(), 0);
    assert_eq!(summary.bytes_copied(), 0);
}

#[test]
fn xxh64_dedup_defaults_to_disabled() {
    let options = LocalCopyOptions::default();
    assert!(!options.xxh64_dedup_enabled());
    assert!(options.xxh64_dedup_size_limit() > 0);
}

#[test]
fn xxh64_dedup_options_round_trip_through_builder() {
    let options = LocalCopyOptions::builder()
        .enable_xxh64_dedup(true)
        .xxh64_dedup_size_limit(2 * 1024 * 1024)
        .build()
        .expect("builder produces valid options");
    assert!(options.xxh64_dedup_enabled());
    assert_eq!(options.xxh64_dedup_size_limit(), 2 * 1024 * 1024);
}
