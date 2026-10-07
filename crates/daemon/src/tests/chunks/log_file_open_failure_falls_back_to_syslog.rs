/// upstream: log.c:175-182 - a log file that cannot be opened is not fatal.
/// The daemon falls back to syslog and logs the failure followed by
/// `Ignoring "log file" setting.`, in that order.
#[test]
fn log_file_open_failure_falls_back_to_syslog() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let nonexistent = dir.path().join("no/such/dir/log.txt");
    let fallback = open_daemon_log_sink(&nonexistent, Brand::Oc).unwrap_err();
    let [failure, ignoring] = fallback.lines();
    // Windows reports a missing parent as ERROR_PATH_NOT_FOUND, so the errno
    // text comes from the OS there; on Unix it must be upstream's ENOENT text.
    #[cfg(unix)]
    let cause = String::from("No such file or directory (2)");
    #[cfg(not(unix))]
    let cause = logging::upstream_errno_text(&std::fs::File::open(&nonexistent).unwrap_err());
    assert_eq!(
        failure,
        format!(
            "oc-rsync: [Receiver] failed to open log-file {}: {cause}",
            nonexistent.display()
        )
    );
    assert_eq!(ignoring, "Ignoring \"log file\" setting.");
}
