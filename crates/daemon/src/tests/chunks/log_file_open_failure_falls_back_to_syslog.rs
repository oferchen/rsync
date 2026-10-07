/// upstream: log.c:175-182 - a log file that cannot be opened is not fatal.
/// The daemon falls back to syslog and logs the failure followed by
/// `Ignoring "log file" setting.`, in that order.
#[test]
fn log_file_open_failure_falls_back_to_syslog() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let nonexistent = dir.path().join("no/such/dir/log.txt");
    let fallback = open_daemon_log_sink(&nonexistent, Brand::Oc).unwrap_err();
    let [failure, ignoring] = fallback.lines();
    assert_eq!(
        failure,
        format!(
            "oc-rsync: [Receiver] failed to open log-file {}: No such file or directory (2)",
            nonexistent.display()
        )
    );
    assert_eq!(ignoring, "Ignoring \"log file\" setting.");
}
