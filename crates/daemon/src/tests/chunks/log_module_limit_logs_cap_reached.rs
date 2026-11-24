/// A module refused by its `max connections` cap logs upstream's line, with
/// the configured number and nothing oc-specific.
///
/// upstream: clientserver.c:801-802 - `max connections (%d) reached`.
#[test]
fn log_module_limit_logs_cap_reached() {
    let dir = tempdir().expect("log dir");
    let path = dir.path().join("daemon.log");
    let log = open_daemon_log_sink(&path, Brand::Oc).expect("open log");
    let limit =
        MaxConnections::Limited(NonZeroU32::new(4).expect("non-zero limit")).display_value();

    log_module_limit(&log, limit);
    log_module_limit(&log, -1);

    drop(log);

    let contents = fs::read_to_string(&path).expect("read log");
    // upstream: log.c:132-142 logit() stamps `%Y/%m/%d %H:%M:%S [pid] ` ahead
    // of the rendered body.
    let bodies: Vec<&str> = contents
        .lines()
        .map(|line| line.split_once("] ").map_or(line, |(_, body)| body))
        .collect();
    assert_eq!(
        bodies,
        [
            "max connections (4) reached",
            "max connections (-1) reached"
        ],
        "log: {contents}"
    );
}
