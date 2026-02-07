use super::common::*;
use super::*;

#[test]
fn local_transfer_appends_default_log_entries() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("payload.txt");
    let destination_dir = temp.path().join("dest");
    std::fs::write(&source, b"payload").expect("write source");
    std::fs::create_dir(&destination_dir).expect("create destination dir");

    let log_path = temp.path().join("transfer.log");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        log_path.clone().into_os_string(),
        source.into_os_string(),
        destination_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let logged = std::fs::read_to_string(&log_path).expect("read log file");
    assert!(
        logged.contains("payload.txt"),
        "missing file entry: {logged:?}"
    );
    assert!(logged.ends_with('\n'));

    let destination = destination_dir.join("payload.txt");
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"payload"
    );
}

#[test]
fn local_transfer_respects_custom_log_format() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("custom.txt");
    let destination_dir = temp.path().join("dest");
    std::fs::write(&source, b"format").expect("write source");
    std::fs::create_dir(&destination_dir).expect("create destination dir");

    let log_path = temp.path().join("custom.log");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        log_path.clone().into_os_string(),
        OsString::from("--log-file-format=%f %l"),
        source.into_os_string(),
        destination_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let logged = std::fs::read_to_string(&log_path).expect("read log file");
    assert_eq!(logged, "custom.txt 6\n");
}

#[test]
fn log_file_append_mode_preserves_previous_entries() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let log_path = temp.path().join("append.log");

    // Pre-populate the log file with existing content.
    std::fs::write(&log_path, "previous entry\n").expect("seed log");

    let source = temp.path().join("append.txt");
    let destination_dir = temp.path().join("dest");
    std::fs::write(&source, b"data").expect("write source");
    std::fs::create_dir(&destination_dir).expect("create destination dir");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        log_path.clone().into_os_string(),
        OsString::from("--log-file-format=%f"),
        source.into_os_string(),
        destination_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);

    let logged = std::fs::read_to_string(&log_path).expect("read log file");
    assert!(
        logged.starts_with("previous entry\n"),
        "previous content should be preserved: {logged:?}"
    );
    assert!(
        logged.contains("append.txt"),
        "new transfer entry should be appended: {logged:?}"
    );
}

#[test]
fn log_file_multiple_files_produce_multiple_entries() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let destination_dir = temp.path().join("dest");
    std::fs::create_dir(&source_dir).expect("create source dir");
    std::fs::create_dir(&destination_dir).expect("create destination dir");

    std::fs::write(source_dir.join("alpha.txt"), b"a").expect("write alpha");
    std::fs::write(source_dir.join("beta.txt"), b"bb").expect("write beta");
    std::fs::write(source_dir.join("gamma.txt"), b"ccc").expect("write gamma");

    let log_path = temp.path().join("multi.log");

    let mut source_trailing = source_dir.into_os_string();
    source_trailing.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        log_path.clone().into_os_string(),
        OsString::from("--log-file-format=%f"),
        source_trailing,
        destination_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);

    let logged = std::fs::read_to_string(&log_path).expect("read log file");
    assert!(
        logged.contains("alpha.txt"),
        "alpha.txt should appear in log: {logged:?}"
    );
    assert!(
        logged.contains("beta.txt"),
        "beta.txt should appear in log: {logged:?}"
    );
    assert!(
        logged.contains("gamma.txt"),
        "gamma.txt should appear in log: {logged:?}"
    );

    // Each file on its own line means at least 3 non-empty lines.
    let non_empty_lines: Vec<&str> = logged.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        non_empty_lines.len() >= 3,
        "expected at least 3 log lines, got {}: {logged:?}",
        non_empty_lines.len()
    );
}

#[test]
fn log_file_with_dry_run_still_logs() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("dryrun.txt");
    let destination_dir = temp.path().join("dest");
    std::fs::write(&source, b"dry").expect("write source");
    std::fs::create_dir(&destination_dir).expect("create destination dir");

    let log_path = temp.path().join("dryrun.log");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--dry-run"),
        OsString::from("--log-file"),
        log_path.clone().into_os_string(),
        OsString::from("--log-file-format=%f"),
        source.into_os_string(),
        destination_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);

    // The destination should not have the file.
    assert!(
        !destination_dir.join("dryrun.txt").exists(),
        "dry run should not create destination file"
    );

    let logged = std::fs::read_to_string(&log_path).expect("read log file");
    assert!(
        logged.contains("dryrun.txt"),
        "dry run should still produce log entries: {logged:?}"
    );
}

#[test]
fn log_file_equals_syntax_creates_log() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("equals.txt");
    let destination_dir = temp.path().join("dest");
    std::fs::write(&source, b"eq").expect("write source");
    std::fs::create_dir(&destination_dir).expect("create destination dir");

    let log_path = temp.path().join("equals.log");
    let log_arg = format!("--log-file={}", log_path.display());

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(log_arg),
        OsString::from("--log-file-format=%f"),
        source.into_os_string(),
        destination_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);

    let logged = std::fs::read_to_string(&log_path).expect("read log file");
    assert!(
        logged.contains("equals.txt"),
        "equals syntax should create a working log: {logged:?}"
    );
}

#[test]
fn log_file_successive_transfers_append() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let destination_dir = temp.path().join("dest");
    std::fs::create_dir(&destination_dir).expect("create destination dir");

    let log_path = temp.path().join("successive.log");

    // First transfer.
    let first_source = temp.path().join("first.txt");
    std::fs::write(&first_source, b"one").expect("write first");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        log_path.clone().into_os_string(),
        OsString::from("--log-file-format=%f"),
        first_source.into_os_string(),
        destination_dir.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);

    // Second transfer with a different file.
    let second_source = temp.path().join("second.txt");
    std::fs::write(&second_source, b"two").expect("write second");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        log_path.clone().into_os_string(),
        OsString::from("--log-file-format=%f"),
        second_source.into_os_string(),
        destination_dir.into_os_string(),
    ]);
    assert_eq!(code, 0);

    let logged = std::fs::read_to_string(&log_path).expect("read log file");
    assert!(
        logged.contains("first.txt"),
        "first transfer should be in log: {logged:?}"
    );
    assert!(
        logged.contains("second.txt"),
        "second transfer should be appended: {logged:?}"
    );
}
