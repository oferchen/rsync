use super::common::*;
use super::*;

/// Asserts the upstream log-file line prefix shape
/// `^\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} \[\d+\] ` (upstream: log.c:127).
fn assert_upstream_log_prefix(line: &str) -> &str {
    let bytes = line.as_bytes();
    let shape_ok = bytes.len() > 21
        && bytes[..19].iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'/',
            10 => *b == b' ',
            13 | 16 => *b == b':',
            _ => b.is_ascii_digit(),
        })
        && bytes[19] == b' '
        && bytes[20] == b'[';
    assert!(
        shape_ok,
        "log line missing upstream `%Y/%m/%d %H:%M:%S [pid] ` prefix (log.c:127): {line:?}"
    );
    let (pid, body) = line[21..].split_once("] ").expect("prefix `] ` separator");
    assert!(
        !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()),
        "pid field must be numeric: {line:?}"
    );
    body
}

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

    // upstream routing for a verbosity-0 `--log-file` run: the FLOG banner
    // (flist.c:2248), the per-file logfile-format line (log.c:818-826), and
    // the FLOG totals trailer (log.c:894-899 via the cleanup.c:222-226
    // `!INFO_GTE(STATS, 1)` gate) - each stamped by logit() (log.c:122-132).
    let logged = std::fs::read_to_string(&log_path).expect("read log file");
    let bodies: Vec<&str> = logged.lines().map(assert_upstream_log_prefix).collect();
    assert_eq!(bodies.len(), 3, "unexpected log lines: {logged:?}");
    assert_eq!(bodies[0], "building file list");
    assert_eq!(bodies[1], "custom.txt 6");
    assert!(
        bodies[2].starts_with("sent ") && bodies[2].contains("  total size "),
        "missing FLOG totals trailer (log.c:895): {logged:?}"
    );
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

    // upstream: options.c:112 defaults `recurse = 0` and flist.c:2452 prints
    // `skipping directory %s` for a directory operand without -r/-a/-d, so a
    // trailing-slash directory source must be paired with --recursive to fan
    // out into the per-child %f log entries this test verifies. Mirrors the
    // pattern from PRs #5985, #5946, #5934, #5955.
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--recursive"),
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

/// upstream: rwrite() mirrors FINFO to the log when `--log-file` is active
/// (log.c:290-303): a `-v` run logs the `bytes/sec` + `speedup` trailer pair
/// instead of the FLOG totals line, which cleanup.c:222-226 suppresses once
/// `INFO_GTE(STATS, 1)` holds. FLOG lines never reach stdout (log.c:304-307).
#[test]
fn verbose_log_file_mirrors_info_trailer_and_keeps_flog_off_stdout() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("mirror.txt");
    let destination_dir = temp.path().join("dest");
    std::fs::write(&source, b"mirror").expect("write source");
    std::fs::create_dir(&destination_dir).expect("create destination dir");

    let log_path = temp.path().join("mirror.log");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("--log-file"),
        log_path.clone().into_os_string(),
        source.into_os_string(),
        destination_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    let stdout = String::from_utf8_lossy(&stdout);
    assert!(
        !stdout.contains("building file list"),
        "FLOG banner must never reach stdout (log.c:304-307): {stdout:?}"
    );

    let logged = std::fs::read_to_string(&log_path).expect("read log file");
    let bodies: Vec<&str> = logged.lines().map(assert_upstream_log_prefix).collect();
    assert_eq!(bodies.first(), Some(&"building file list"));
    assert!(
        bodies.iter().any(|body| body.contains(" bytes/sec")),
        "FINFO trailer must be mirrored into the log (log.c:290-303): {logged:?}"
    );
    assert!(
        bodies.iter().any(|body| body.starts_with("total size is ")),
        "FINFO speedup line must be mirrored into the log: {logged:?}"
    );
    assert!(
        !bodies.iter().any(|body| body.contains("  total size ")),
        "the FLOG totals line is suppressed when STATS >= 1 (cleanup.c:222-226): {logged:?}"
    );
    assert!(
        logged.lines().all(|line| !line.trim().is_empty()),
        "FCLIENT blank separators never reach the log (log.c:288-289): {logged:?}"
    );
}
