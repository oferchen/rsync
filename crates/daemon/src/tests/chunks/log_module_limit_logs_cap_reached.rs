#[test]
fn log_module_limit_logs_cap_reached() {
    let dir = tempdir().expect("log dir");
    let path = dir.path().join("daemon.log");
    let log = open_log_sink(&path, Brand::Oc).expect("open log");
    let limit = NonZeroU32::new(4).expect("non-zero limit");

    log_module_limit(
        &log,
        Some("client.example"),
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 17)),
        "docs",
        limit,
        4,
    );

    drop(log);

    let contents = fs::read_to_string(&path).expect("read log");
    assert!(
        contents.starts_with("oc-rsync warning:"),
        "expected warning-level message, got: {contents}"
    );
    assert!(
        contents.contains("max-connections cap reached"),
        "missing structured prefix: {contents}"
    );
    assert!(
        contents.contains("which=docs"),
        "missing which= field: {contents}"
    );
    assert!(
        contents.contains("peer=client.example"),
        "missing peer= field: {contents}"
    );
    assert!(
        contents.contains("(192.0.2.17)"),
        "missing peer ip: {contents}"
    );
    assert!(contents.contains("cap=4"), "missing cap= field: {contents}");
    assert!(
        contents.contains("current=4"),
        "missing current= field: {contents}"
    );
}
