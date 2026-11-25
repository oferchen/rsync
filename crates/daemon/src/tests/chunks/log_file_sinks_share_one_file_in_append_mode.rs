/// Every daemon connection opens its own sink on the same `log file` and the
/// lines must land after each other, never on top of each other. That needs
/// the file opened `O_APPEND`, as upstream's logfile_open() does
/// (log.c:176-178, `O_WRONLY|O_APPEND|O_CREAT` then `fdopen(fd, "a")`); a
/// plain write-mode open would put every sink at offset 0, so the second
/// sink's line would overwrite the first.
#[test]
fn log_file_sinks_share_one_file_in_append_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rsyncd.log");
    std::fs::write(&path, "kept\n").expect("seed log");
    let first = open_daemon_log_sink(&path, Brand::Oc).expect("open first sink");
    let second = open_daemon_log_sink(&path, Brand::Oc).expect("open second sink");

    log_module_limit(&first, 1);
    log_module_limit(&second, 2);

    let log = std::fs::read_to_string(&path).expect("read log");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 3, "log:\n{log}");
    assert_eq!(lines[0], "kept");
    assert!(
        lines[1].ends_with("] max connections (1) reached"),
        "log:\n{log}"
    );
    assert!(
        lines[2].ends_with("] max connections (2) reached"),
        "log:\n{log}"
    );
}
