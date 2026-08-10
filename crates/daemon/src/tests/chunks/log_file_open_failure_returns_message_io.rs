/// upstream: log.c:163 - daemon log-open failures produce RERR_MESSAGEIO (13).
#[test]
fn log_file_open_failure_returns_message_io() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let nonexistent = dir.path().join("no/such/dir/log.txt");
    let err = open_log_sink(&nonexistent, Brand::Oc).unwrap_err();
    assert_eq!(err.code(), ExitCode::MessageIo);
    assert!(err.to_string().contains("failed to open log file"));
}
