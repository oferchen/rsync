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
        source.clone().into_os_string(),
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
        source.clone().into_os_string(),
        destination_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let logged = std::fs::read_to_string(&log_path).expect("read log file");
    assert_eq!(logged, "custom.txt 6\n");
}
